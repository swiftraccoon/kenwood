/*
 * SPDX-FileCopyrightText: 2026 Swift Raccoon
 * SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later
 *
 * Bulk byte pump for the TH-D75 CDC Data interface (interface 1).
 * Mutable state is serialized on the driver's default dispatch queue.
 */

#include <os/log.h>

#include <DriverKit/IOBufferMemoryDescriptor.h>
#include <DriverKit/IODispatchQueue.h>
#include <DriverKit/IOLib.h>
#include <DriverKit/IOService.h>
#include <DriverKit/IOTypes.h>
#include <DriverKit/IOUserServer.h>
#include <DriverKit/OSAction.h>
#include <DriverKit/OSData.h>
#include <USBDriverKit/AppleUSBDescriptorParsing.h>
#include <USBDriverKit/IOUSBHostInterface.h>
#include <USBDriverKit/IOUSBHostPipe.h>
#include <USBDriverKit/USBDriverKitDefs.h>

#include "AzimuthUSBSerialDriver.h"
#include "AzimuthUserClient.h"

#define Log(fmt, ...) os_log(OS_LOG_DEFAULT, "[Azimuth dext] " fmt, ##__VA_ARGS__)

namespace {
constexpr size_t kRxRingSize = 32768;
constexpr size_t kTxQueueSize = 16384;
constexpr uint32_t kInBufferSize = 1024;
constexpr uint32_t kOutBufferSize = 4096;
// Bulk OUT has no artificial completion deadline. App-session detach
// synchronously aborts any remaining flight, so an unresponsive endpoint
// cannot poison the next connection.
constexpr uint32_t kOutCompletionTimeoutMs = 0;

constexpr uint8_t kCDCRequestTypeClassInterfaceOut = 0x21;
constexpr uint8_t kCDCSetLineCoding = 0x20;
constexpr uint8_t kCDCSetControlLineState = 0x22;
constexpr uint16_t kCDCControlLinesClosed = 0x0000;
constexpr uint16_t kCDCDTRRTS = 0x0003;

/// Exact 32-byte wire record mirrored by AzimuthUSBDextLogEntry.
struct AzimuthLogEntry {
    uint32_t sequence;
    uint32_t event;
    int64_t code;
    uint64_t a;
    uint64_t b;
};
static_assert(sizeof(AzimuthLogEntry) == 32, "diagnostic ABI changed");
constexpr size_t kLogEntryCount = 96;

enum AzimuthEvent : uint32_t {
    kEventStartEndpoint = 1,
    kEventStartBuffers = 2,
    kEventStartControlLine = 3,
    kEventStartOK = 4,
    kEventStartFailed = 5,
    kEventArmDoorbell = 6,
    kEventDoorbellFired = 7,
    kEventBulkInError = 8,
    kEventRxData = 9,
    kEventEnqueueWrite = 10,
    kEventTxSubmit = 11,
    kEventLinkFailed = 12,
    kEventReadCopy = 13,
    kEventStartLineCoding = 14,
    kEventSetBaudRate = 15,
    kEventTxComplete = 16,
    kEventClientAttach = 17,
    kEventClientDetach = 18,
    kEventSessionLineCoding = 19,
    kEventSessionControlSet = 20,
    kEventBulkInSubmit = 21,
    kEventBulkInComplete = 22,
    kEventSessionControlClear = 23,
};
}

struct AzimuthUSBSerialDriver_IVars
{
    IOUSBHostInterface *interface = nullptr;
    IODispatchQueue *ioQueue = nullptr;
    IOUSBHostPipe *inPipe = nullptr;
    IOUSBHostPipe *outPipe = nullptr;
    IOBufferMemoryDescriptor *inBuffer = nullptr;
    IOBufferMemoryDescriptor *outBuffer = nullptr;
    uint8_t *inAddress = nullptr;
    uint8_t *outAddress = nullptr;
    OSAction *inAction = nullptr;
    OSAction *outAction = nullptr;

    // Monotonic indexes make full and empty distinguishable without wasting a
    // byte. The newest bytes are dropped on overflow; already received bytes
    // remain ordered for the app.
    uint8_t rx[kRxRingSize];
    size_t rxHead = 0;
    size_t rxTail = 0;
    uint64_t rxOverflow = 0;

    uint8_t tx[kTxQueueSize];
    size_t txLength = 0;
    bool txInFlight = false;
    uint32_t txExpectedLength = 0;

    // The client is intentionally not retained. Stop unregisters it on this
    // same queue before destruction. An armed action is retained.
    AzimuthUserClient *userClient = nullptr;
    OSAction *doorbell = nullptr;

    bool linkUp = false;
    bool stopping = false;
    uint32_t inErrorStreak = 0;
    uint32_t baudRate = 115200;

    AzimuthLogEntry logRing[kLogEntryCount];
    uint32_t nextLogSequence = 0;
};

static size_t RxCount(const AzimuthUSBSerialDriver_IVars *ivars)
{
    return ivars->rxHead - ivars->rxTail;
}

static void LogEvent(AzimuthUSBSerialDriver_IVars *ivars,
                     uint32_t event,
                     int64_t code,
                     uint64_t a,
                     uint64_t b)
{
    AzimuthLogEntry &entry =
        ivars->logRing[ivars->nextLogSequence % kLogEntryCount];
    entry.sequence = ivars->nextLogSequence++;
    entry.event = event;
    entry.code = code;
    entry.a = a;
    entry.b = b;
}

/// Mark the pump dead and ring an armed doorbell so a parked app read cannot
/// hang forever after unplug, a dext failure, or a terminal pipe error.
static void LinkFailed(AzimuthUSBSerialDriver_IVars *ivars,
                       const char *why,
                       uint64_t reason)
{
    if (!ivars->linkUp && !ivars->doorbell) return;
    Log("link failed: %s", why);
    LogEvent(ivars, kEventLinkFailed, 0, reason, 0);
    ivars->linkUp = false;
    if (ivars->doorbell && ivars->userClient) {
        LogEvent(ivars, kEventDoorbellFired, kIOReturnAborted, 0, 0);
        ivars->userClient->FireDoorbell(ivars->doorbell, kIOReturnAborted);
    }
    OSSafeReleaseNULL(ivars->doorbell);
}

/// Submit one FIFO chunk. Flight state is established before AsyncIO so even a
/// reentrant completion cannot leave a stale true flag after the call returns.
/// A synchronous submission error is returned to the app through linkDown.
static void SendNextTx(AzimuthUSBSerialDriver_IVars *ivars)
{
    if (ivars->txInFlight || ivars->txLength == 0 || !ivars->linkUp) return;
    if (!ivars->outAddress) {
        LinkFailed(ivars, "bulk-OUT buffer is unmapped", 6);
        return;
    }
    const uint32_t count = static_cast<uint32_t>(
        ivars->txLength < kOutBufferSize ? ivars->txLength : kOutBufferSize);
    const kern_return_t lengthResult = ivars->outBuffer->SetLength(count);
    if (lengthResult != kIOReturnSuccess) {
        LogEvent(ivars, kEventTxSubmit, lengthResult, count,
                 kOutCompletionTimeoutMs);
        LinkFailed(ivars, "bulk-OUT valid length failed", 10);
        return;
    }
    memcpy(ivars->outAddress, ivars->tx, count);
    memmove(ivars->tx, ivars->tx + count, ivars->txLength - count);
    ivars->txLength -= count;
    ivars->txInFlight = true;
    ivars->txExpectedLength = count;
    const kern_return_t result = ivars->outPipe->AsyncIO(
        ivars->outBuffer, count, ivars->outAction,
        kOutCompletionTimeoutMs);
    LogEvent(ivars, kEventTxSubmit, result, count,
             kOutCompletionTimeoutMs);
    if (result != kIOReturnSuccess) {
        ivars->txInFlight = false;
        ivars->txExpectedLength = 0;
        LinkFailed(ivars, "bulk-OUT submit failed", 5);
    }
}

/// End one app-owned TX session. The FIFO is discarded before aborting so a
/// completion delivered during synchronous Abort cannot pump stale commands.
/// Flight state is cleared only after DriverKit has quiesced the descriptor.
static kern_return_t ResetClientTx(
    AzimuthUSBSerialDriver_IVars *ivars,
    uint64_t *activeBytes,
    uint64_t *queuedBytes)
{
    *activeBytes = ivars->txInFlight ? ivars->txExpectedLength : 0;
    *queuedBytes = ivars->txLength;
    ivars->txLength = 0;

    if (!ivars->txInFlight) {
        ivars->txExpectedLength = 0;
        return kIOReturnSuccess;
    }
    if (!ivars->outPipe) {
        LinkFailed(ivars, "bulk-OUT session reset without pipe", 9);
        return kIOReturnNotAttached;
    }

    const kern_return_t result = ivars->outPipe->Abort(
        kIOUSBAbortSynchronous, kIOReturnAborted, nullptr);
    if (result == kIOReturnSuccess) {
        // Synchronous Abort guarantees that the old completion is finished.
        ivars->txInFlight = false;
        ivars->txExpectedLength = 0;
    } else {
        // Do not permit descriptor reuse when the old transfer could remain.
        LinkFailed(ivars, "bulk-OUT session reset failed", 9);
    }
    return result;
}

static void ReleaseHardware(AzimuthUSBSerialDriver *self,
                            AzimuthUSBSerialDriver_IVars *ivars,
                            bool interfaceWasOpened)
{
    OSSafeReleaseNULL(ivars->inAction);
    OSSafeReleaseNULL(ivars->outAction);
    OSSafeReleaseNULL(ivars->inBuffer);
    OSSafeReleaseNULL(ivars->outBuffer);
    ivars->inAddress = nullptr;
    ivars->outAddress = nullptr;
    OSSafeReleaseNULL(ivars->inPipe);
    OSSafeReleaseNULL(ivars->outPipe);
    if (ivars->interface) {
        if (interfaceWasOpened) ivars->interface->Close(self, 0);
        OSSafeReleaseNULL(ivars->interface);
    }
    OSSafeReleaseNULL(ivars->ioQueue);
}

/// Issue CDC SET_LINE_CODING to communications interface 0. Call only on the
/// driver queue so this control transfer cannot race Stop or another change.
static kern_return_t ApplyBaudRate(AzimuthUSBSerialDriver_IVars *ivars,
                                   uint32_t baudRate)
{
    if (baudRate != 9600 && baudRate != 115200) return kIOReturnBadArgument;
    if (!ivars->interface) return kIOReturnNotAttached;

    const uint8_t lineCoding[7] = {
        static_cast<uint8_t>(baudRate & 0xff),
        static_cast<uint8_t>((baudRate >> 8) & 0xff),
        static_cast<uint8_t>((baudRate >> 16) & 0xff),
        static_cast<uint8_t>((baudRate >> 24) & 0xff),
        0x00, // one stop bit
        0x00, // no parity
        0x08, // eight data bits
    };
    IOBufferMemoryDescriptor *descriptor = nullptr;
    kern_return_t result = IOBufferMemoryDescriptor::Create(
        kIOMemoryDirectionOut, sizeof(lineCoding), 0, &descriptor);
    if (result != kIOReturnSuccess || !descriptor) {
        return result != kIOReturnSuccess ? result : kIOReturnNoMemory;
    }

    IOAddressSegment range = {};
    result = descriptor->SetLength(sizeof(lineCoding));
    if (result == kIOReturnSuccess) {
        result = descriptor->GetAddressRange(&range);
    }
    if (result == kIOReturnSuccess
        && range.address != 0
        && range.length >= sizeof(lineCoding)) {
        memcpy(reinterpret_cast<void *>(range.address),
               lineCoding, sizeof(lineCoding));
        uint16_t transferred = 0;
        result = ivars->interface->DeviceRequest(
            kCDCRequestTypeClassInterfaceOut,
            kCDCSetLineCoding,
            0,
            0,
            sizeof(lineCoding),
            descriptor,
            &transferred,
            1000);
        if (result == kIOReturnSuccess && transferred != sizeof(lineCoding)) {
            result = kIOReturnUnderrun;
        }
    } else if (result == kIOReturnSuccess) {
        result = kIOReturnNoMemory;
    }
    OSSafeReleaseNULL(descriptor);
    return result;
}

static kern_return_t ApplyControlLineState(
    AzimuthUSBSerialDriver_IVars *ivars,
    uint16_t state)
{
    if (!ivars->interface) return kIOReturnNotAttached;
    uint16_t transferred = 0;
    return ivars->interface->DeviceRequest(
        kCDCRequestTypeClassInterfaceOut,
        kCDCSetControlLineState,
        state,
        0,
        0,
        nullptr,
        &transferred,
        1000);
}

/// Recreate the serial-port-open sequence only after the app has opened its user
/// client. At that point the companion control-interface service is already
/// registered, unlike the unordered personality Start callbacks.
static kern_return_t PrepareClientSerialSession(
    AzimuthUSBSerialDriver_IVars *ivars)
{
    const kern_return_t lineResult = ApplyBaudRate(ivars, ivars->baudRate);
    LogEvent(ivars, kEventSessionLineCoding, lineResult,
             ivars->baudRate, 0);

    const kern_return_t setResult = ApplyControlLineState(ivars, kCDCDTRRTS);
    LogEvent(ivars, kEventSessionControlSet, setResult, kCDCDTRRTS, 0);

    if (lineResult != kIOReturnSuccess) return lineResult;
    if (setResult != kIOReturnSuccess) return setResult;

    // Give the radio's CDC firmware a bounded interval to consume the control
    // requests before the app submits its first mode-probe packet.
    IOSleep(100);
    return kIOReturnSuccess;
}

bool AzimuthUSBSerialDriver::init()
{
    if (!super::init()) return false;
    ivars = IONewZero(AzimuthUSBSerialDriver_IVars, 1);
    return ivars != nullptr;
}

void AzimuthUSBSerialDriver::free()
{
    if (ivars) IOSafeDeleteNULL(ivars, AzimuthUSBSerialDriver_IVars, 1);
    super::free();
}

kern_return_t IMPL(AzimuthUSBSerialDriver, Start)
{
    kern_return_t result = Start(provider, SUPERDISPATCH);
    if (result != kIOReturnSuccess) return result;
    ivars->stopping = false;
    ivars->txLength = 0;
    ivars->txInFlight = false;
    ivars->txExpectedLength = 0;
    ivars->baudRate = 115200;

    ivars->interface = OSDynamicCast(IOUSBHostInterface, provider);
    if (!ivars->interface) {
        Stop(provider, SUPERDISPATCH);
        return kIOReturnNoDevice;
    }
    ivars->interface->retain();

    result = CopyDispatchQueue(kIOServiceDefaultQueueName, &ivars->ioQueue);
    if (result != kIOReturnSuccess) {
        ReleaseHardware(this, ivars, false);
        Stop(provider, SUPERDISPATCH);
        return result;
    }
    result = ivars->interface->Open(this, 0, nullptr);
    if (result != kIOReturnSuccess) {
        ReleaseHardware(this, ivars, false);
        Stop(provider, SUPERDISPATCH);
        return result;
    }

    const IOUSBConfigurationDescriptor *configuration =
        ivars->interface->CopyConfigurationDescriptor();
    if (!configuration) {
        LogEvent(ivars, kEventStartFailed, kIOReturnNotFound, 1, 0);
        ReleaseHardware(this, ivars, true);
        Stop(provider, SUPERDISPATCH);
        return kIOReturnNotFound;
    }
    const IOUSBInterfaceDescriptor *interfaceDescriptor =
        ivars->interface->GetInterfaceDescriptor(configuration);
    uint8_t bulkIn = 0;
    uint8_t bulkOut = 0;
    if (interfaceDescriptor) {
        const IOUSBEndpointDescriptor *endpoint = nullptr;
        while ((endpoint = IOUSBGetNextEndpointDescriptor(
                    configuration,
                    interfaceDescriptor,
                    reinterpret_cast<const IOUSBDescriptorHeader *>(endpoint))) != nullptr) {
            const uint8_t address = IOUSBGetEndpointAddress(endpoint);
            const uint8_t type = IOUSBGetEndpointType(endpoint);
            LogEvent(ivars, kEventStartEndpoint, 0, address, type);
            if (type == kIOUSBEndpointTypeBulk) {
                if (address & 0x80) bulkIn = address;
                else bulkOut = address;
            }
        }
    }
    IOUSBHostFreeDescriptor(configuration);
    if (bulkIn == 0 || bulkOut == 0) {
        LogEvent(ivars, kEventStartFailed, kIOReturnNotFound, 2, 0);
        ReleaseHardware(this, ivars, true);
        Stop(provider, SUPERDISPATCH);
        return kIOReturnNotFound;
    }

    result = ivars->interface->CopyPipe(bulkIn, &ivars->inPipe);
    if (result != kIOReturnSuccess || !ivars->inPipe) {
        if (result == kIOReturnSuccess) result = kIOReturnNotFound;
        LogEvent(ivars, kEventStartFailed, result, 3, 0);
        ReleaseHardware(this, ivars, true);
        Stop(provider, SUPERDISPATCH);
        return result;
    }
    result = ivars->interface->CopyPipe(bulkOut, &ivars->outPipe);
    if (result != kIOReturnSuccess || !ivars->outPipe) {
        if (result == kIOReturnSuccess) result = kIOReturnNotFound;
        LogEvent(ivars, kEventStartFailed, result, 4, 0);
        ReleaseHardware(this, ivars, true);
        Stop(provider, SUPERDISPATCH);
        return result;
    }

    // Plain IOBufferMemoryDescriptor memory is directly mapped into the dext.
    // Guard every address before dereference; a bad mapping fails Start rather
    // than crashing the process on the first memcpy.
    IOAddressSegment range = {};
    result = IOBufferMemoryDescriptor::Create(
        kIOMemoryDirectionIn, kInBufferSize, 0, &ivars->inBuffer);
    if (result != kIOReturnSuccess || !ivars->inBuffer) {
        if (result == kIOReturnSuccess) result = kIOReturnNoMemory;
        LogEvent(ivars, kEventStartFailed, result, 8, 0);
        ReleaseHardware(this, ivars, true);
        Stop(provider, SUPERDISPATCH);
        return result;
    }
    result = ivars->inBuffer->SetLength(kInBufferSize);
    if (result != kIOReturnSuccess) {
        LogEvent(ivars, kEventStartFailed, result, 13, 0);
        ReleaseHardware(this, ivars, true);
        Stop(provider, SUPERDISPATCH);
        return result;
    }
    result = ivars->inBuffer->GetAddressRange(&range);
    if (result != kIOReturnSuccess
        || range.address == 0
        || range.length < kInBufferSize) {
        if (result == kIOReturnSuccess) result = kIOReturnNoMemory;
        LogEvent(ivars, kEventStartFailed, result, 9, 0);
        ReleaseHardware(this, ivars, true);
        Stop(provider, SUPERDISPATCH);
        return result;
    }
    ivars->inAddress = reinterpret_cast<uint8_t *>(range.address);

    range = {};
    result = IOBufferMemoryDescriptor::Create(
        kIOMemoryDirectionOut, kOutBufferSize, 0, &ivars->outBuffer);
    if (result != kIOReturnSuccess || !ivars->outBuffer) {
        if (result == kIOReturnSuccess) result = kIOReturnNoMemory;
        LogEvent(ivars, kEventStartFailed, result, 10, 0);
        ReleaseHardware(this, ivars, true);
        Stop(provider, SUPERDISPATCH);
        return result;
    }
    result = ivars->outBuffer->SetLength(kOutBufferSize);
    if (result != kIOReturnSuccess) {
        LogEvent(ivars, kEventStartFailed, result, 14, 0);
        ReleaseHardware(this, ivars, true);
        Stop(provider, SUPERDISPATCH);
        return result;
    }
    result = ivars->outBuffer->GetAddressRange(&range);
    if (result != kIOReturnSuccess
        || range.address == 0
        || range.length < kOutBufferSize) {
        if (result == kIOReturnSuccess) result = kIOReturnNoMemory;
        LogEvent(ivars, kEventStartFailed, result, 11, 0);
        ReleaseHardware(this, ivars, true);
        Stop(provider, SUPERDISPATCH);
        return result;
    }
    ivars->outAddress = reinterpret_cast<uint8_t *>(range.address);
    LogEvent(ivars, kEventStartBuffers, 0,
             ivars->inAddress != nullptr, ivars->outAddress != nullptr);

    result = CreateActionBulkInComplete(0, &ivars->inAction);
    if (result != kIOReturnSuccess || !ivars->inAction) {
        if (result == kIOReturnSuccess) result = kIOReturnNoMemory;
        ReleaseHardware(this, ivars, true);
        Stop(provider, SUPERDISPATCH);
        return result;
    }
    result = CreateActionBulkOutComplete(0, &ivars->outAction);
    if (result != kIOReturnSuccess || !ivars->outAction) {
        if (result == kIOReturnSuccess) result = kIOReturnNoMemory;
        ReleaseHardware(this, ivars, true);
        Stop(provider, SUPERDISPATCH);
        return result;
    }

    // The dext can outlive an app connection, so Start establishes the
    // serial-port-closed state. A user-client attach performs the actual
    // DTR|RTS assertion after applying line coding.
    result = ApplyBaudRate(ivars, 115200);
    LogEvent(ivars, kEventStartLineCoding, result, 0, 0);

    result = ApplyControlLineState(ivars, kCDCControlLinesClosed);
    LogEvent(ivars, kEventStartControlLine, result,
             kCDCControlLinesClosed, 0);

    result = ivars->inPipe->AsyncIO(
        ivars->inBuffer, kInBufferSize, ivars->inAction, 0);
    LogEvent(ivars, kEventBulkInSubmit, result, 0, kInBufferSize);
    if (result != kIOReturnSuccess) {
        LogEvent(ivars, kEventStartFailed, result, 12, 0);
        ReleaseHardware(this, ivars, true);
        Stop(provider, SUPERDISPATCH);
        return result;
    }

    ivars->linkUp = true;
    result = RegisterService();
    if (result != kIOReturnSuccess) {
        ivars->linkUp = false;
        ivars->inPipe->Abort(kIOUSBAbortSynchronous, kIOReturnAborted, nullptr);
        ReleaseHardware(this, ivars, true);
        Stop(provider, SUPERDISPATCH);
        return result;
    }
    LogEvent(ivars, kEventStartOK, 0, bulkIn, bulkOut);
    Log("started bulk IN=0x%02x OUT=0x%02x", bulkIn, bulkOut);
    return kIOReturnSuccess;
}

kern_return_t IMPL(AzimuthUSBSerialDriver, Stop)
{
    ivars->stopping = true;
    ivars->txLength = 0;
    if (ivars->inPipe) {
        ivars->inPipe->Abort(kIOUSBAbortSynchronous, kIOReturnAborted, nullptr);
    }
    if (ivars->outPipe) {
        ivars->outPipe->Abort(kIOUSBAbortSynchronous, kIOReturnAborted, nullptr);
    }
    ivars->txInFlight = false;
    ivars->txExpectedLength = 0;
    LinkFailed(ivars, "Stop", 1);
    ivars->userClient = nullptr;
    ReleaseHardware(this, ivars, true);
    return Stop(provider, SUPERDISPATCH);
}

void IMPL(AzimuthUSBSerialDriver, BulkInComplete)
{
    (void)action;
    (void)completionTimestamp;
    LogEvent(ivars, kEventBulkInComplete, status,
             actualByteCount, ivars->inErrorStreak);
    if (status == kIOReturnAborted || ivars->stopping) return;

    if (status != kIOReturnSuccess) {
        ivars->inErrorStreak++;
        LogEvent(ivars, kEventBulkInError, status,
                 actualByteCount, ivars->inErrorStreak);
        if (ivars->inErrorStreak > 3) {
            LinkFailed(ivars, "bulk-IN error streak", 2);
            return;
        }
        ivars->inPipe->ClearStall(true);
        const kern_return_t rearmResult = ivars->inPipe->AsyncIO(
            ivars->inBuffer, kInBufferSize, ivars->inAction, 0);
        LogEvent(ivars, kEventBulkInSubmit, rearmResult, 2, kInBufferSize);
        if (rearmResult != kIOReturnSuccess) {
            LinkFailed(ivars, "bulk-IN re-arm after stall", 3);
        }
        return;
    }
    ivars->inErrorStreak = 0;

    const uint32_t safeByteCount =
        actualByteCount < kInBufferSize ? actualByteCount : kInBufferSize;
    if (actualByteCount > safeByteCount) {
        ivars->rxOverflow += actualByteCount - safeByteCount;
    }
    const bool wasEmpty = RxCount(ivars) == 0;
    for (uint32_t index = 0; index < safeByteCount; index++) {
        if (RxCount(ivars) >= kRxRingSize) {
            ivars->rxOverflow += safeByteCount - index;
            break;
        }
        ivars->rx[ivars->rxHead % kRxRingSize] = ivars->inAddress[index];
        ivars->rxHead++;
    }

    if (wasEmpty && RxCount(ivars) > 0) {
        LogEvent(ivars, kEventRxData, 0, safeByteCount, 0);
        if (ivars->doorbell && ivars->userClient) {
            LogEvent(ivars, kEventDoorbellFired, kIOReturnSuccess, 0, 0);
            ivars->userClient->FireDoorbell(ivars->doorbell, kIOReturnSuccess);
            OSSafeReleaseNULL(ivars->doorbell);
        }
    }

    const kern_return_t rearmResult = ivars->inPipe->AsyncIO(
        ivars->inBuffer, kInBufferSize, ivars->inAction, 0);
    LogEvent(ivars, kEventBulkInSubmit, rearmResult, 1, kInBufferSize);
    if (rearmResult != kIOReturnSuccess) {
        LinkFailed(ivars, "bulk-IN re-arm", 4);
    }
}

void IMPL(AzimuthUSBSerialDriver, BulkOutComplete)
{
    (void)action;
    (void)completionTimestamp;
    const uint32_t expectedByteCount = ivars->txExpectedLength;
    LogEvent(ivars, kEventTxComplete, status,
             actualByteCount, expectedByteCount);
    ivars->txInFlight = false;
    ivars->txExpectedLength = 0;
    if (status == kIOReturnAborted || ivars->stopping) return;
    if (status != kIOReturnSuccess) {
        Log("bulk-OUT completion status=0x%x", status);
        if (status != kIOReturnTimeout) ivars->outPipe->ClearStall(true);
        LinkFailed(ivars, "bulk-OUT completion failed", 7);
        return;
    } else if (actualByteCount != expectedByteCount) {
        LinkFailed(ivars, "short bulk-OUT completion", 8);
        return;
    }
    SendNextTx(ivars);
}

kern_return_t AzimuthUSBSerialDriver::EnqueueWrite(
    const uint8_t *data,
    size_t length)
{
    if (!data || length == 0 || length > kOutBufferSize) {
        return kIOReturnBadArgument;
    }
    __block kern_return_t result = kIOReturnSuccess;
    ivars->ioQueue->DispatchSync(^{
        if (!ivars->linkUp) {
            result = kIOReturnNotAttached;
            return;
        }
        if (ivars->txLength + length > kTxQueueSize) {
            result = kIOReturnNoResources;
            LogEvent(ivars, kEventEnqueueWrite, result,
                     length, ivars->txLength);
            return;
        }
        memcpy(ivars->tx + ivars->txLength, data, length);
        ivars->txLength += length;
        SendNextTx(ivars);
        if (!ivars->linkUp) result = kIOReturnNotAttached;
        LogEvent(ivars, kEventEnqueueWrite, result,
                 length, ivars->txLength);
    });
    return result;
}

kern_return_t AzimuthUSBSerialDriver::SetBaudRate(uint32_t baudRate)
{
    if (baudRate != 9600 && baudRate != 115200) return kIOReturnBadArgument;
    __block kern_return_t result = kIOReturnSuccess;
    ivars->ioQueue->DispatchSync(^{
        if (!ivars->linkUp) {
            result = kIOReturnNotAttached;
            return;
        }
        result = ApplyBaudRate(ivars, baudRate);
        LogEvent(ivars, kEventSetBaudRate, result, baudRate, 0);
        if (result == kIOReturnSuccess) ivars->baudRate = baudRate;
    });
    return result;
}

kern_return_t AzimuthUSBSerialDriver::CopyBufferedBytes(
    uint8_t *out,
    size_t capacity,
    size_t *actual)
{
    if (!out || !actual || capacity > kOutBufferSize) {
        return kIOReturnBadArgument;
    }
    __block kern_return_t result = kIOReturnSuccess;
    __block size_t copied = 0;
    ivars->ioQueue->DispatchSync(^{
        if (!ivars->linkUp && RxCount(ivars) == 0) {
            result = kIOReturnNotAttached;
            return;
        }
        while (copied < capacity && RxCount(ivars) > 0) {
            out[copied++] = ivars->rx[ivars->rxTail % kRxRingSize];
            ivars->rxTail++;
        }
        if (copied > 0) {
            LogEvent(ivars, kEventReadCopy, 0, capacity, copied);
        }
    });
    *actual = copied;
    return result;
}

kern_return_t AzimuthUSBSerialDriver::RegisterDoorbell(
    AzimuthUserClient *client,
    OSAction *action)
{
    if (!client || !action) return kIOReturnBadArgument;
    __block kern_return_t result = kIOReturnSuccess;
    action->retain();
    ivars->ioQueue->DispatchSync(^{
        // Returning an immediate error (without firing the completion) lets the
        // app reclaim its async refcon through the normal failed-arm path.
        if (!ivars->linkUp) {
            result = kIOReturnNotAttached;
            action->release();
            return;
        }
        if (ivars->userClient != client) {
            if (ivars->doorbell && ivars->userClient) {
                ivars->userClient->FireDoorbell(
                    ivars->doorbell, kIOReturnAborted);
                OSSafeReleaseNULL(ivars->doorbell);
            }
            uint64_t activeBytes = 0;
            uint64_t queuedBytes = 0;
            const kern_return_t resetResult = ResetClientTx(
                ivars, &activeBytes, &queuedBytes);
            ivars->userClient = client;
            LogEvent(ivars, kEventClientAttach, resetResult,
                     activeBytes, queuedBytes);
            if (resetResult != kIOReturnSuccess) {
                result = resetResult;
            } else {
                result = PrepareClientSerialSession(ivars);
            }
        }
        if (result != kIOReturnSuccess) {
            action->release();
            LinkFailed(ivars, "client serial session preparation failed", 11);
            return;
        }
        if (ivars->doorbell && ivars->userClient) {
            ivars->userClient->FireDoorbell(ivars->doorbell, kIOReturnAborted);
            OSSafeReleaseNULL(ivars->doorbell);
        }
        ivars->userClient = client;
        if (!ivars->linkUp) {
            LogEvent(ivars, kEventArmDoorbell, 0, 2, 0);
            client->FireDoorbell(action, kIOReturnAborted);
            action->release();
            return;
        }
        if (RxCount(ivars) > 0) {
            LogEvent(ivars, kEventArmDoorbell, 0, 1, 0);
            client->FireDoorbell(action, kIOReturnSuccess);
            action->release();
            return;
        }
        LogEvent(ivars, kEventArmDoorbell, 0, 0, 0);
        ivars->doorbell = action;
    });
    return result;
}

void AzimuthUSBSerialDriver::UnregisterUserClient(AzimuthUserClient *client)
{
    ivars->ioQueue->DispatchSync(^{
        if (ivars->userClient != client) return;
        if (ivars->doorbell) {
            ivars->userClient->FireDoorbell(ivars->doorbell, kIOReturnAborted);
            OSSafeReleaseNULL(ivars->doorbell);
        }
        uint64_t activeBytes = 0;
        uint64_t queuedBytes = 0;
        const kern_return_t resetResult = ResetClientTx(
            ivars, &activeBytes, &queuedBytes);
        LogEvent(ivars, kEventClientDetach, resetResult,
                 activeBytes, queuedBytes);
        if (resetResult == kIOReturnSuccess
            && ivars->linkUp
            && !ivars->stopping) {
            const kern_return_t clearResult = ApplyControlLineState(
                ivars, kCDCControlLinesClosed);
            LogEvent(ivars, kEventSessionControlClear, clearResult,
                     kCDCControlLinesClosed, 0);
            if (clearResult == kIOReturnSuccess) {
                // IOServiceClose is synchronous. Holding the serialized queue
                // here guarantees a real low interval before a new client can
                // reapply line coding and assert DTR|RTS.
                IOSleep(100);
            }
        }
        ivars->userClient = nullptr;
    });
}

void AzimuthUSBSerialDriver::CopyStatus(uint64_t out[4])
{
    ivars->ioQueue->DispatchSync(^{
        out[0] = RxCount(ivars);
        out[1] = ivars->rxOverflow;
        out[2] = ivars->linkUp ? 1 : 0;
        out[3] = ivars->doorbell ? 1 : 0;
    });
}

void AzimuthUSBSerialDriver::CopyLogEntries(
    uint8_t *out,
    size_t capacity,
    size_t *actual)
{
    __block size_t copied = 0;
    ivars->ioQueue->DispatchSync(^{
        const uint32_t count = ivars->nextLogSequence < kLogEntryCount
            ? ivars->nextLogSequence
            : static_cast<uint32_t>(kLogEntryCount);
        const uint32_t first = ivars->nextLogSequence - count;
        for (uint32_t index = 0; index < count; index++) {
            if (copied + sizeof(AzimuthLogEntry) > capacity) break;
            const AzimuthLogEntry &entry =
                ivars->logRing[(first + index) % kLogEntryCount];
            memcpy(out + copied, &entry, sizeof(AzimuthLogEntry));
            copied += sizeof(AzimuthLogEntry);
        }
    });
    *actual = copied;
}

kern_return_t IMPL(AzimuthUSBSerialDriver, NewUserClient)
{
    (void)type;
    IOService *client = nullptr;
    kern_return_t result = Create(
        this, "AzimuthUserClientProperties", &client);
    if (result != kIOReturnSuccess || !client) return result;
    *userClient = OSDynamicCast(IOUserClient, client);
    if (!*userClient) {
        OSSafeReleaseNULL(client);
        return kIOReturnError;
    }
    return kIOReturnSuccess;
}
