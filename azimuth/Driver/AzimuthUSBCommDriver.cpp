/*
 * SPDX-FileCopyrightText: 2026 Swift Raccoon
 * SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later
 */

#include <os/log.h>

#include <DriverKit/IOBufferMemoryDescriptor.h>
#include <DriverKit/IOLib.h>
#include <DriverKit/IOService.h>
#include <DriverKit/OSAction.h>
#include <USBDriverKit/AppleUSBDescriptorParsing.h>
#include <USBDriverKit/IOUSBHostInterface.h>
#include <USBDriverKit/IOUSBHostPipe.h>
#include <USBDriverKit/USBDriverKitDefs.h>

#include "AzimuthUSBCommDriver.h"

#define Log(fmt, ...) os_log(OS_LOG_DEFAULT, "[Azimuth comm] " fmt, ##__VA_ARGS__)

namespace {
constexpr uint32_t kNotifyBufferSize = 64;
}

struct AzimuthUSBCommDriver_IVars
{
    IOUSBHostInterface *interface = nullptr;
    IOUSBHostPipe *notifyPipe = nullptr;
    IOBufferMemoryDescriptor *notifyBuffer = nullptr;
    OSAction *notifyAction = nullptr;
    bool stopping = false;
};

static void ReleaseCommHardware(
    AzimuthUSBCommDriver *self,
    AzimuthUSBCommDriver_IVars *ivars,
    bool interfaceWasOpened)
{
    if (ivars->notifyPipe) {
        ivars->notifyPipe->Abort(
            kIOUSBAbortSynchronous, kIOReturnAborted, nullptr);
    }
    OSSafeReleaseNULL(ivars->notifyAction);
    OSSafeReleaseNULL(ivars->notifyBuffer);
    OSSafeReleaseNULL(ivars->notifyPipe);
    if (ivars->interface) {
        if (interfaceWasOpened) ivars->interface->Close(self, 0);
        OSSafeReleaseNULL(ivars->interface);
    }
}

bool AzimuthUSBCommDriver::init()
{
    if (!super::init()) return false;
    ivars = IONewZero(AzimuthUSBCommDriver_IVars, 1);
    return ivars != nullptr;
}

void AzimuthUSBCommDriver::free()
{
    if (ivars) IOSafeDeleteNULL(ivars, AzimuthUSBCommDriver_IVars, 1);
    super::free();
}

kern_return_t IMPL(AzimuthUSBCommDriver, Start)
{
    kern_return_t result = Start(provider, SUPERDISPATCH);
    if (result != kIOReturnSuccess) return result;

    ivars->interface = OSDynamicCast(IOUSBHostInterface, provider);
    if (!ivars->interface) {
        Stop(provider, SUPERDISPATCH);
        return kIOReturnNoDevice;
    }
    ivars->interface->retain();
    result = ivars->interface->Open(this, 0, nullptr);
    if (result != kIOReturnSuccess) {
        OSSafeReleaseNULL(ivars->interface);
        Stop(provider, SUPERDISPATCH);
        return result;
    }

    ivars->stopping = false;
    uint8_t notifyEndpoint = 0;
    const IOUSBConfigurationDescriptor *configuration =
        ivars->interface->CopyConfigurationDescriptor();
    if (!configuration) {
        result = kIOReturnNotFound;
    } else {
        const IOUSBInterfaceDescriptor *interfaceDescriptor =
            ivars->interface->GetInterfaceDescriptor(configuration);
        if (!interfaceDescriptor) {
            result = kIOReturnNotFound;
        } else {
            const IOUSBEndpointDescriptor *endpoint = nullptr;
            while ((endpoint = IOUSBGetNextEndpointDescriptor(
                        configuration,
                        interfaceDescriptor,
                        reinterpret_cast<const IOUSBDescriptorHeader *>(endpoint))) != nullptr) {
                const uint8_t address = IOUSBGetEndpointAddress(endpoint);
                const uint8_t type = IOUSBGetEndpointType(endpoint);
                Log("endpoint 0x%02x type %u", address, type);
                if (type == kIOUSBEndpointTypeInterrupt && (address & 0x80)) {
                    notifyEndpoint = address;
                }
            }
        }
        IOUSBHostFreeDescriptor(configuration);
    }

    if (result == kIOReturnSuccess && notifyEndpoint == 0) {
        result = kIOReturnNotFound;
    }
    if (result == kIOReturnSuccess) {
        result = ivars->interface->CopyPipe(
            notifyEndpoint, &ivars->notifyPipe);
        if (result == kIOReturnSuccess && !ivars->notifyPipe) {
            result = kIOReturnNotFound;
        }
    }
    if (result == kIOReturnSuccess) {
        result = IOBufferMemoryDescriptor::Create(
            kIOMemoryDirectionIn,
            kNotifyBufferSize,
            0,
            &ivars->notifyBuffer);
    }
    if (result == kIOReturnSuccess && !ivars->notifyBuffer) {
        result = kIOReturnNoMemory;
    }
    if (result == kIOReturnSuccess) {
        result = ivars->notifyBuffer->SetLength(kNotifyBufferSize);
    }
    IOAddressSegment address = {};
    if (result == kIOReturnSuccess) {
        result = ivars->notifyBuffer->GetAddressRange(&address);
        if (result == kIOReturnSuccess
            && (address.address == 0 || address.length < kNotifyBufferSize)) {
            result = kIOReturnNoMemory;
        }
    }
    if (result == kIOReturnSuccess) {
        result = CreateActionInterruptComplete(0, &ivars->notifyAction);
        if (result == kIOReturnSuccess && !ivars->notifyAction) {
            result = kIOReturnNoMemory;
        }
    }
    if (result == kIOReturnSuccess) {
        result = ivars->notifyPipe->AsyncIO(
            ivars->notifyBuffer,
            kNotifyBufferSize,
            ivars->notifyAction,
            0);
        Log("interrupt pump 0x%02x -> 0x%x", notifyEndpoint, result);
    }
    if (result != kIOReturnSuccess) {
        Log("control interface setup failed 0x%x", result);
        ReleaseCommHardware(this, ivars, true);
        Stop(provider, SUPERDISPATCH);
        return result;
    }

    result = RegisterService();
    if (result != kIOReturnSuccess) {
        Log("RegisterService failed 0x%x", result);
        ReleaseCommHardware(this, ivars, true);
        Stop(provider, SUPERDISPATCH);
        return result;
    }
    Log("control interface claimed");
    return kIOReturnSuccess;
}

kern_return_t IMPL(AzimuthUSBCommDriver, Stop)
{
    ivars->stopping = true;
    ReleaseCommHardware(this, ivars, true);
    return Stop(provider, SUPERDISPATCH);
}

void IMPL(AzimuthUSBCommDriver, InterruptComplete)
{
    (void)action;
    (void)completionTimestamp;
    if (status == kIOReturnAborted || ivars->stopping) return;
    Log("notification status=0x%x bytes=%u", status, actualByteCount);
    if (status != kIOReturnSuccess) {
        ivars->notifyPipe->ClearStall(true);
    }
    const kern_return_t result = ivars->notifyPipe->AsyncIO(
        ivars->notifyBuffer, kNotifyBufferSize, ivars->notifyAction, 0);
    if (result != kIOReturnSuccess) {
        Log("notification re-arm failed 0x%x", result);
    }
}
