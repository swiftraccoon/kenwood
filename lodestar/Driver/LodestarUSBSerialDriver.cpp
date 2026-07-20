/*
 * SPDX-FileCopyrightText: 2026 Swift Raccoon
 * SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later
 *
 * LodestarUSBSerialDriver — byte pump between the iPad app and the
 * TH-D75's CDC Data interface (bulk IN/OUT on bInterfaceNumber 1).
 *
 * Concurrency: everything mutable lives behind the driver's default
 * dispatch queue. Kernel upcalls (Start/Stop, AsyncIO completions)
 * arrive there already; entry points called from the user client's
 * queue DispatchSync onto it.
 */

#include <os/log.h>

#include <DriverKit/IOLib.h>
#include <DriverKit/IOUserServer.h>
#include <DriverKit/IOService.h>
#include <DriverKit/IOTypes.h>
#include <DriverKit/IODispatchQueue.h>
#include <DriverKit/IOBufferMemoryDescriptor.h>
#include <DriverKit/OSAction.h>
#include <DriverKit/OSData.h>

#include <USBDriverKit/IOUSBHostInterface.h>
#include <USBDriverKit/IOUSBHostPipe.h>
#include <USBDriverKit/AppleUSBDescriptorParsing.h>
#include <USBDriverKit/USBDriverKitDefs.h>

#include "LodestarUSBSerialDriver.h"
#include "LodestarUserClient.h"

#define Log(fmt, ...) os_log(OS_LOG_DEFAULT, "[Lodestar dext] " fmt, ##__VA_ARGS__)

namespace {
constexpr size_t kRxRingSize = 32768;
constexpr size_t kTxQueueSize = 16384;
constexpr uint32_t kInBufferSize = 1024;   // ≥ wMaxPacketSize (64 @ full speed)
constexpr uint32_t kOutBufferSize = 4096;

// CDC PSTN class requests to the communications interface (wIndex 0).
// Kenwood documents the line-coding VALUE as ignored, but every real
// host OS sends SET_LINE_CODING on port open and the radio's CDC stack
// may not bring up its TX path until it has seen one — send both, in
// the standard host order (line coding, then DTR|RTS). Hardware
// evidence 2026-07-19: with only SET_CONTROL_LINE_STATE the radio ACKs
// our bulk-OUT bytes yet never answers CAT.
constexpr uint8_t kCdcReqTypeClassInterfaceOut = 0x21;
constexpr uint8_t kCdcSetLineCoding = 0x20;
constexpr uint8_t kCdcSetControlLineState = 0x22;
constexpr uint16_t kCdcDtrRts = 0x0003;
// 115200 baud (LE), 1 stop bit, no parity, 8 data bits.
constexpr uint8_t kLineCoding115200_8N1[7] = {0x00, 0xC2, 0x01, 0x00, 0x00, 0x00, 0x08};

// Diagnostic event ring. Entry layout and event codes are mirrored by
// `USBDextLogEntry` in Shared/Transport/USBSerialLink.swift — keep in
// sync. 96 × 32 B = 3072 B, fits one 4096 B read.
struct LodestarLogEntry {
    uint32_t seq;
    uint32_t event;
    int64_t code;
    uint64_t a;
    uint64_t b;
};
constexpr size_t kLogEntries = 96;

enum LodestarEvent : uint32_t {
    kEvStartEndpoint = 1,   // a=bEndpointAddress b=type
    kEvStartBuffers = 2,    // a=inAddr!=0 b=outAddr!=0
    kEvStartClsResult = 3,  // code=DeviceRequest result
    kEvStartOk = 4,         // a=bulkIn b=bulkOut
    kEvStartFail = 5,       // code=kern a=stage
    kEvArmDoorbell = 6,     // a: 0=armed 1=fired-immediately 2=aborted-link-down
    kEvDoorbellFired = 7,   // code=status delivered
    kEvBulkInError = 8,     // code=status a=bytes b=streak
    kEvRxData = 9,          // a=bytes (empty→non-empty edge only)
    kEvEnqueueWrite = 10,   // code=result a=len b=txLen-after
    kEvTxSubmit = 11,       // code=AsyncIO result a=n
    kEvLinkFailed = 12,     // a=reason: 1 Stop 2 in-streak 3 in-rearm-stall
                            //           4 in-rearm 5 out-submit 6 out-addr-null
    kEvReadCopy = 13,       // a=requested b=copied
    kEvStartLineCoding = 14, // code=SET_LINE_CODING result
};
} // namespace

struct LodestarUSBSerialDriver_IVars
{
    IOUSBHostInterface *interface = nullptr;
    IODispatchQueue *ioQueue = nullptr;       // driver default queue
    IOUSBHostPipe *inPipe = nullptr;
    IOUSBHostPipe *outPipe = nullptr;
    IOBufferMemoryDescriptor *inBuffer = nullptr;
    IOBufferMemoryDescriptor *outBuffer = nullptr;
    uint8_t *inAddr = nullptr;                // dext-side view of inBuffer
    uint8_t *outAddr = nullptr;
    OSAction *inAction = nullptr;
    OSAction *outAction = nullptr;

    // RX ring. Head/tail are monotonic; index mod kRxRingSize.
    uint8_t rx[kRxRingSize];
    size_t rxHead = 0;                        // write index
    size_t rxTail = 0;                        // read index
    uint64_t rxOverflow = 0;                  // bytes dropped

    // TX queue (contiguous FIFO, compacted on send).
    uint8_t tx[kTxQueueSize];
    size_t txLen = 0;
    bool txInFlight = false;

    // Doorbell. userClient is not retained: the user client
    // unregisters itself (via the driver queue) before it dies.
    LodestarUserClient *userClient = nullptr;
    OSAction *doorbell = nullptr;             // retained while armed

    bool linkUp = false;
    bool stopping = false;
    uint32_t inErrorStreak = 0;

    // Diagnostic event ring (see LodestarLogEntry). seq is monotonic.
    LodestarLogEntry logRing[kLogEntries];
    uint32_t logSeq = 0;
};

/// Append a diagnostic event. Must run on the driver queue (all call
/// sites are: Start/Stop, completions, and DispatchSync'd bodies).
static void LogEvent(LodestarUSBSerialDriver_IVars *iv, uint32_t event,
                     int64_t code, uint64_t a, uint64_t b)
{
    LodestarLogEntry &e = iv->logRing[iv->logSeq % kLogEntries];
    e.seq = iv->logSeq++;
    e.event = event;
    e.code = code;
    e.a = a;
    e.b = b;
}

static size_t RxCount(const LodestarUSBSerialDriver_IVars *iv)
{
    return iv->rxHead - iv->rxTail;
}

/// Mark the link dead and wake the app. Must run on the driver queue.
/// The armed doorbell is the app's ONLY signal (it never polls Status
/// while a read is parked) — every path that kills the pump must fire
/// it, or a receive-only session hangs forever with no error anywhere.
static void LinkFailed(LodestarUSBSerialDriver_IVars *iv, const char *why,
                       uint64_t reason)
{
    if (!iv->linkUp && !iv->doorbell) return;
    Log("link failed: %s", why);
    LogEvent(iv, kEvLinkFailed, 0, reason, 0);
    iv->linkUp = false;
    if (iv->doorbell && iv->userClient) {
        LogEvent(iv, kEvDoorbellFired, kIOReturnAborted, 0, 0);
        iv->userClient->FireDoorbell(iv->doorbell, kIOReturnAborted);
    }
    OSSafeReleaseNULL(iv->doorbell);
}

/// Submit the TX FIFO head if idle. Must run on the driver queue.
/// Bytes leave the FIFO only AFTER AsyncIO accepts the transfer — a
/// dequeue-before-submit ordering silently loses already-accepted
/// bytes when the submit fails.
static void SendNextTx(LodestarUSBSerialDriver_IVars *iv)
{
    if (iv->txInFlight || iv->txLen == 0 || !iv->linkUp) return;
    if (iv->outAddr == nullptr) {
        LinkFailed(iv, "bulk-OUT buffer not mapped", 6);
        return;
    }
    const uint32_t n = static_cast<uint32_t>(
        iv->txLen < kOutBufferSize ? iv->txLen : kOutBufferSize);
    memcpy(iv->outAddr, iv->tx, n);
    const kern_return_t kr = iv->outPipe->AsyncIO(iv->outBuffer, n, iv->outAction, 0);
    LogEvent(iv, kEvTxSubmit, kr, n, 0);
    if (kr != kIOReturnSuccess) {
        Log("SendNextTx: AsyncIO failed 0x%x (%u bytes still queued)", kr,
            static_cast<uint32_t>(iv->txLen));
        LinkFailed(iv, "bulk-OUT submit failed", 5);
        return;
    }
    memmove(iv->tx, iv->tx + n, iv->txLen - n);
    iv->txLen -= n;
    iv->txInFlight = true;
}

/// Release everything Start acquired. Safe on partial acquisition;
/// closes the interface only when it was opened.
static void ReleaseHardware(LodestarUSBSerialDriver *self,
                            LodestarUSBSerialDriver_IVars *iv,
                            bool interfaceWasOpened)
{
    OSSafeReleaseNULL(iv->inAction);
    OSSafeReleaseNULL(iv->outAction);
    OSSafeReleaseNULL(iv->inBuffer);
    OSSafeReleaseNULL(iv->outBuffer);
    iv->inAddr = nullptr;
    iv->outAddr = nullptr;
    OSSafeReleaseNULL(iv->inPipe);
    OSSafeReleaseNULL(iv->outPipe);
    if (iv->interface) {
        if (interfaceWasOpened) iv->interface->Close(self, 0);
        OSSafeReleaseNULL(iv->interface);
    }
    OSSafeReleaseNULL(iv->ioQueue);
}

bool LodestarUSBSerialDriver::init()
{
    if (!super::init()) return false;
    ivars = IONewZero(LodestarUSBSerialDriver_IVars, 1);
    return ivars != nullptr;
}

void LodestarUSBSerialDriver::free()
{
    if (ivars) IOSafeDeleteNULL(ivars, LodestarUSBSerialDriver_IVars, 1);
    super::free();
}

kern_return_t IMPL(LodestarUSBSerialDriver, Start)
{
    kern_return_t ret = Start(provider, SUPERDISPATCH);
    if (ret != kIOReturnSuccess) return ret;

    ivars->interface = OSDynamicCast(IOUSBHostInterface, provider);
    if (!ivars->interface) {
        Log("Start: provider is not IOUSBHostInterface");
        Stop(provider, SUPERDISPATCH);
        return kIOReturnNoDevice;
    }
    ivars->interface->retain();

    ret = CopyDispatchQueue(kIOServiceDefaultQueueName, &ivars->ioQueue);
    if (ret != kIOReturnSuccess) {
        Log("Start: CopyDispatchQueue failed 0x%x", ret);
        ReleaseHardware(this, ivars, false);
        Stop(provider, SUPERDISPATCH);
        return ret;
    }

    ret = ivars->interface->Open(this, 0, nullptr);
    if (ret != kIOReturnSuccess) {
        Log("Start: interface Open failed 0x%x", ret);
        ReleaseHardware(this, ivars, false);
        Stop(provider, SUPERDISPATCH);
        return ret;
    }

    // --- Endpoint discovery: walk this interface's endpoint
    // descriptors; record the bulk IN and bulk OUT addresses. ---
    const IOUSBConfigurationDescriptor *config =
        ivars->interface->CopyConfigurationDescriptor();
    if (!config) {
        Log("Start: CopyConfigurationDescriptor failed");
        ReleaseHardware(this, ivars, true);
        Stop(provider, SUPERDISPATCH);
        return kIOReturnNotFound;
    }
    const IOUSBInterfaceDescriptor *ifaceDesc =
        ivars->interface->GetInterfaceDescriptor(config);
    uint8_t bulkIn = 0, bulkOut = 0;
    if (ifaceDesc) {
        const IOUSBEndpointDescriptor *ep = nullptr;
        while ((ep = IOUSBGetNextEndpointDescriptor(config, ifaceDesc,
                       reinterpret_cast<const IOUSBDescriptorHeader *>(ep))) != nullptr) {
            const uint8_t addr = IOUSBGetEndpointAddress(ep);
            const uint8_t type = IOUSBGetEndpointType(ep);
            Log("Start: endpoint 0x%02x type %u", addr, type);
            LogEvent(ivars, kEvStartEndpoint, 0, addr, type);
            if (type == kIOUSBEndpointTypeBulk) {
                if (addr & 0x80) bulkIn = addr; else bulkOut = addr;
            }
        }
    }
    IOUSBHostFreeDescriptor(config);
    if (bulkIn == 0 || bulkOut == 0) {
        Log("Start: bulk endpoints not found (in=0x%02x out=0x%02x)", bulkIn, bulkOut);
        ReleaseHardware(this, ivars, true);
        Stop(provider, SUPERDISPATCH);
        return kIOReturnNotFound;
    }

    ret = ivars->interface->CopyPipe(bulkIn, &ivars->inPipe);
    if (ret != kIOReturnSuccess || !ivars->inPipe) {
        Log("Start: CopyPipe(IN 0x%02x) failed 0x%x", bulkIn, ret);
        ReleaseHardware(this, ivars, true);
        Stop(provider, SUPERDISPATCH);
        return ret;
    }
    ret = ivars->interface->CopyPipe(bulkOut, &ivars->outPipe);
    if (ret != kIOReturnSuccess || !ivars->outPipe) {
        Log("Start: CopyPipe(OUT 0x%02x) failed 0x%x", bulkOut, ret);
        ReleaseHardware(this, ivars, true);
        Stop(provider, SUPERDISPATCH);
        return ret;
    }

    // --- Transfer buffers. Plain IOBufferMemoryDescriptor::Create
    // memory: documented as mapped in the driver's address space with
    // GetAddressRange valid. (The interface's CreateIOBuffer variant is
    // controller-optimized but its dext-side dereferenceability proved
    // doubtful on iPadOS 27 — the first `memcpy(outAddr, …)` killed the
    // process with MIG_SERVER_DIED. Bounced DMA is irrelevant at our
    // ~1 KB/s.) Addresses are additionally guarded: a zero address
    // fails Start instead of crashing on first use. ---
    IOAddressSegment range = {};
    ret = IOBufferMemoryDescriptor::Create(kIOMemoryDirectionIn,
                                           kInBufferSize, 0, &ivars->inBuffer);
    if (ret != kIOReturnSuccess) { Log("Start: in buffer Create 0x%x", ret); LogEvent(ivars, kEvStartFail, ret, 8, 0); ReleaseHardware(this, ivars, true); Stop(provider, SUPERDISPATCH); return ret; }
    ret = ivars->inBuffer->GetAddressRange(&range);
    if (ret != kIOReturnSuccess || range.address == 0) { Log("Start: in GetAddressRange 0x%x addr=0x%llx", ret, range.address); LogEvent(ivars, kEvStartFail, ret, 9, 0); ReleaseHardware(this, ivars, true); Stop(provider, SUPERDISPATCH); return ret != kIOReturnSuccess ? ret : kIOReturnNoMemory; }
    ivars->inAddr = reinterpret_cast<uint8_t *>(range.address);

    ret = IOBufferMemoryDescriptor::Create(kIOMemoryDirectionOut,
                                           kOutBufferSize, 0, &ivars->outBuffer);
    if (ret != kIOReturnSuccess) { Log("Start: out buffer Create 0x%x", ret); LogEvent(ivars, kEvStartFail, ret, 10, 0); ReleaseHardware(this, ivars, true); Stop(provider, SUPERDISPATCH); return ret; }
    ret = ivars->outBuffer->GetAddressRange(&range);
    if (ret != kIOReturnSuccess || range.address == 0) { Log("Start: out GetAddressRange 0x%x addr=0x%llx", ret, range.address); LogEvent(ivars, kEvStartFail, ret, 11, 0); ReleaseHardware(this, ivars, true); Stop(provider, SUPERDISPATCH); return ret != kIOReturnSuccess ? ret : kIOReturnNoMemory; }
    ivars->outAddr = reinterpret_cast<uint8_t *>(range.address);

    LogEvent(ivars, kEvStartBuffers, 0,
             ivars->inAddr != nullptr, ivars->outAddr != nullptr);

    // --- Completion actions (iig-generated helpers). ---
    ret = CreateActionBulkInComplete(0, &ivars->inAction);
    if (ret != kIOReturnSuccess) { Log("Start: in action 0x%x", ret); ReleaseHardware(this, ivars, true); Stop(provider, SUPERDISPATCH); return ret; }
    ret = CreateActionBulkOutComplete(0, &ivars->outAction);
    if (ret != kIOReturnSuccess) { Log("Start: out action 0x%x", ret); ReleaseHardware(this, ivars, true); Stop(provider, SUPERDISPATCH); return ret; }

    // --- Best-effort SET_CONTROL_LINE_STATE (DTR|RTS → comm iface 0).
    // The radio may not need it; never fail Start over it. ---
    // SET_LINE_CODING first (standard host order): the value is
    // documented-ignored, but the request itself initializes the CDC
    // session on some stacks. Needs a 7-byte data-stage buffer.
    uint16_t transferred = 0;
    {
        IOBufferMemoryDescriptor *lineCoding = nullptr;
        ret = IOBufferMemoryDescriptor::Create(kIOMemoryDirectionOut,
                                               sizeof(kLineCoding115200_8N1), 0,
                                               &lineCoding);
        if (ret == kIOReturnSuccess && lineCoding) {
            IOAddressSegment lcRange = {};
            if (lineCoding->GetAddressRange(&lcRange) == kIOReturnSuccess
                    && lcRange.address != 0) {
                memcpy(reinterpret_cast<void *>(lcRange.address),
                       kLineCoding115200_8N1, sizeof(kLineCoding115200_8N1));
                ret = ivars->interface->DeviceRequest(
                    kCdcReqTypeClassInterfaceOut, kCdcSetLineCoding, 0,
                    /*wIndex*/ 0, /*wLength*/ sizeof(kLineCoding115200_8N1),
                    lineCoding, &transferred, /*completionTimeoutMs*/ 1000);
            } else {
                ret = kIOReturnNoMemory;
            }
            OSSafeReleaseNULL(lineCoding);
        }
        Log("Start: SET_LINE_CODING -> 0x%x (best-effort)", ret);
        LogEvent(ivars, kEvStartLineCoding, ret, 0, 0);
    }

    ret = ivars->interface->DeviceRequest(
        kCdcReqTypeClassInterfaceOut, kCdcSetControlLineState, kCdcDtrRts,
        /*wIndex*/ 0, /*wLength*/ 0, /*dataBuffer*/ nullptr,
        &transferred, /*completionTimeoutMs*/ 1000);
    Log("Start: SET_CONTROL_LINE_STATE -> 0x%x (best-effort)", ret);
    LogEvent(ivars, kEvStartClsResult, ret, 0, 0);

    // --- Arm the first bulk-IN read. ---
    ret = ivars->inPipe->AsyncIO(ivars->inBuffer, kInBufferSize, ivars->inAction, 0);
    if (ret != kIOReturnSuccess) {
        Log("Start: initial AsyncIO failed 0x%x", ret);
        ReleaseHardware(this, ivars, true);
        Stop(provider, SUPERDISPATCH);
        return ret;
    }

    ivars->linkUp = true;
    ret = RegisterService();
    if (ret != kIOReturnSuccess) {
        Log("Start: RegisterService failed 0x%x", ret);
        return ret;
    }
    Log("Start ok: bulk IN 0x%02x, bulk OUT 0x%02x", bulkIn, bulkOut);
    LogEvent(ivars, kEvStartOk, 0, bulkIn, bulkOut);
    return kIOReturnSuccess;
}

kern_return_t IMPL(LodestarUSBSerialDriver, Stop)
{
    Log("Stop");
    ivars->stopping = true;

    if (ivars->inPipe) ivars->inPipe->Abort(kIOUSBAbortSynchronous, kIOReturnAborted, nullptr);
    if (ivars->outPipe) ivars->outPipe->Abort(kIOUSBAbortSynchronous, kIOReturnAborted, nullptr);

    // Unblock the app: abort any armed doorbell, then release hardware.
    LinkFailed(ivars, "Stop", 1);
    ivars->userClient = nullptr;
    ReleaseHardware(this, ivars, true);

    return Stop(provider, SUPERDISPATCH);
}

// Runs on the driver queue.
void IMPL(LodestarUSBSerialDriver, BulkInComplete)
{
    (void)action; (void)completionTimestamp;
    if (status == kIOReturnAborted || ivars->stopping) return;

    if (status != kIOReturnSuccess) {
        ivars->inErrorStreak++;
        Log("BulkInComplete: status 0x%x (streak %u)", status, ivars->inErrorStreak);
        LogEvent(ivars, kEvBulkInError, status, actualByteCount, ivars->inErrorStreak);
        if (ivars->inErrorStreak > 3) {
            // Give up — and WAKE THE APP: the armed doorbell is its only
            // signal, and a parked read makes no user-client calls.
            LinkFailed(ivars, "bulk-IN error streak", 2);
            return;
        }
        ivars->inPipe->ClearStall(true);
        if (ivars->inPipe->AsyncIO(ivars->inBuffer, kInBufferSize, ivars->inAction, 0)
                != kIOReturnSuccess) {
            // Submit failed: no completion will ever re-enter this pump.
            LinkFailed(ivars, "bulk-IN re-arm after stall", 3);
        }
        return;
    }
    ivars->inErrorStreak = 0;

    const bool wasEmpty = (RxCount(ivars) == 0);
    for (uint32_t i = 0; i < actualByteCount; i++) {
        if (RxCount(ivars) >= kRxRingSize) {
            ivars->rxOverflow += actualByteCount - i;
            break; // drop newest
        }
        ivars->rx[ivars->rxHead % kRxRingSize] = ivars->inAddr[i];
        ivars->rxHead++;
    }

    if (wasEmpty && RxCount(ivars) > 0) {
        LogEvent(ivars, kEvRxData, 0, actualByteCount, 0);
        if (ivars->doorbell && ivars->userClient) {
            LogEvent(ivars, kEvDoorbellFired, kIOReturnSuccess, 0, 0);
            ivars->userClient->FireDoorbell(ivars->doorbell, kIOReturnSuccess);
            OSSafeReleaseNULL(ivars->doorbell); // one-shot
        }
    }

    if (ivars->inPipe->AsyncIO(ivars->inBuffer, kInBufferSize, ivars->inAction, 0)
            != kIOReturnSuccess) {
        // A silently dead pump with linkUp still true is the worst
        // failure mode: reads park forever. Surface it.
        LinkFailed(ivars, "bulk-IN re-arm", 4);
    }
}

// Runs on the driver queue.
void IMPL(LodestarUSBSerialDriver, BulkOutComplete)
{
    (void)action; (void)actualByteCount; (void)completionTimestamp;
    ivars->txInFlight = false;
    if (status == kIOReturnAborted || ivars->stopping) return;
    if (status != kIOReturnSuccess) {
        Log("BulkOutComplete: status 0x%x", status);
        ivars->outPipe->ClearStall(true);
    }
    SendNextTx(ivars);
}

kern_return_t LodestarUSBSerialDriver::EnqueueWrite(const uint8_t *data, size_t length)
{
    __block kern_return_t result = kIOReturnSuccess;
    ivars->ioQueue->DispatchSync(^{
        if (!ivars->linkUp) { result = kIOReturnNotAttached; return; }
        if (ivars->txLen + length > kTxQueueSize) {
            result = kIOReturnNoResources;
            return;
        }
        memcpy(ivars->tx + ivars->txLen, data, length);
        ivars->txLen += length;
        SendNextTx(ivars);
        // A submit failure inside SendNextTx marks the link failed and
        // fires the doorbell; report it to this caller too.
        if (!ivars->linkUp) result = kIOReturnNotAttached;
        LogEvent(ivars, kEvEnqueueWrite, result, length, ivars->txLen);
    });
    return result;
}

kern_return_t LodestarUSBSerialDriver::CopyBufferedBytes(uint8_t *out, size_t capacity, size_t *actual)
{
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
        if (copied > 0) LogEvent(ivars, kEvReadCopy, 0, capacity, copied);
    });
    *actual = copied;
    return result;
}

void LodestarUSBSerialDriver::RegisterDoorbell(LodestarUserClient *client, OSAction *action)
{
    action->retain();
    ivars->ioQueue->DispatchSync(^{
        if (ivars->doorbell && ivars->userClient) {
            ivars->userClient->FireDoorbell(ivars->doorbell, kIOReturnAborted);
            OSSafeReleaseNULL(ivars->doorbell);
        }
        ivars->userClient = client;
        if (!ivars->linkUp) {
            LogEvent(ivars, kEvArmDoorbell, 0, 2, 0);
            client->FireDoorbell(action, kIOReturnAborted);
            action->release();
            return;
        }
        if (RxCount(ivars) > 0) {
            // Armed while data pending: fire immediately (contract).
            LogEvent(ivars, kEvArmDoorbell, 0, 1, 0);
            client->FireDoorbell(action, kIOReturnSuccess);
            action->release();
            return;
        }
        LogEvent(ivars, kEvArmDoorbell, 0, 0, 0);
        ivars->doorbell = action; // keep the retain
    });
}

void LodestarUSBSerialDriver::UnregisterUserClient(LodestarUserClient *client)
{
    ivars->ioQueue->DispatchSync(^{
        if (ivars->userClient != client) return;
        if (ivars->doorbell) {
            ivars->userClient->FireDoorbell(ivars->doorbell, kIOReturnAborted);
            OSSafeReleaseNULL(ivars->doorbell);
        }
        ivars->userClient = nullptr;
    });
}

void LodestarUSBSerialDriver::CopyStatus(uint64_t out[4])
{
    ivars->ioQueue->DispatchSync(^{
        out[0] = RxCount(ivars);
        out[1] = ivars->rxOverflow;
        out[2] = ivars->linkUp ? 1 : 0;
        out[3] = ivars->doorbell ? 1 : 0;
    });
}

void LodestarUSBSerialDriver::CopyLogEntries(uint8_t *out, size_t capacity, size_t *actual)
{
    __block size_t copied = 0;
    ivars->ioQueue->DispatchSync(^{
        const uint32_t count = ivars->logSeq < kLogEntries
            ? ivars->logSeq : static_cast<uint32_t>(kLogEntries);
        const uint32_t first = ivars->logSeq - count;
        for (uint32_t i = 0; i < count; i++) {
            if (copied + sizeof(LodestarLogEntry) > capacity) break;
            const LodestarLogEntry &e =
                ivars->logRing[(first + i) % kLogEntries];
            memcpy(out + copied, &e, sizeof(LodestarLogEntry));
            copied += sizeof(LodestarLogEntry);
        }
    });
    *actual = copied;
}

kern_return_t IMPL(LodestarUSBSerialDriver, NewUserClient)
{
    (void)type;
    IOService *client = nullptr;
    kern_return_t ret = Create(this, "LodestarUserClientProperties", &client);
    if (ret != kIOReturnSuccess || client == nullptr) {
        Log("NewUserClient: Create failed 0x%x", ret);
        return ret;
    }
    *userClient = OSDynamicCast(IOUserClient, client);
    if (*userClient == nullptr) {
        OSSafeReleaseNULL(client);
        return kIOReturnError;
    }
    return kIOReturnSuccess;
}
