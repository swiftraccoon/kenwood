/*
 * SPDX-FileCopyrightText: 2026 Swift Raccoon
 * SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later
 */

#include <os/log.h>

#include <DriverKit/IOLib.h>
#include <DriverKit/IOService.h>
#include <DriverKit/IOBufferMemoryDescriptor.h>
#include <DriverKit/OSAction.h>

#include <USBDriverKit/IOUSBHostInterface.h>
#include <USBDriverKit/IOUSBHostPipe.h>
#include <USBDriverKit/AppleUSBDescriptorParsing.h>
#include <USBDriverKit/USBDriverKitDefs.h>

#include "LodestarUSBCommDriver.h"

#define Log(fmt, ...) os_log(OS_LOG_DEFAULT, "[Lodestar comm] " fmt, ##__VA_ARGS__)

namespace {
constexpr uint32_t kNotifyBufferSize = 64;
} // namespace

struct LodestarUSBCommDriver_IVars
{
    IOUSBHostInterface *interface = nullptr;
    IOUSBHostPipe *notifyPipe = nullptr;
    IOBufferMemoryDescriptor *notifyBuffer = nullptr;
    OSAction *notifyAction = nullptr;
    bool stopping = false;
};

bool LodestarUSBCommDriver::init()
{
    if (!super::init()) return false;
    ivars = IONewZero(LodestarUSBCommDriver_IVars, 1);
    return ivars != nullptr;
}

void LodestarUSBCommDriver::free()
{
    if (ivars) IOSafeDeleteNULL(ivars, LodestarUSBCommDriver_IVars, 1);
    super::free();
}

kern_return_t IMPL(LodestarUSBCommDriver, Start)
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

    ret = ivars->interface->Open(this, 0, nullptr);
    if (ret != kIOReturnSuccess) {
        Log("Start: Open failed 0x%x", ret);
        OSSafeReleaseNULL(ivars->interface);
        Stop(provider, SUPERDISPATCH);
        return ret;
    }

    // Find the interrupt-IN (notification) endpoint. Its absence is
    // not fatal — merely claiming the interface may be what matters.
    uint8_t notifyEp = 0;
    const IOUSBConfigurationDescriptor *config =
        ivars->interface->CopyConfigurationDescriptor();
    if (config) {
        const IOUSBInterfaceDescriptor *ifaceDesc =
            ivars->interface->GetInterfaceDescriptor(config);
        if (ifaceDesc) {
            const IOUSBEndpointDescriptor *ep = nullptr;
            while ((ep = IOUSBGetNextEndpointDescriptor(config, ifaceDesc,
                           reinterpret_cast<const IOUSBDescriptorHeader *>(ep))) != nullptr) {
                const uint8_t addr = IOUSBGetEndpointAddress(ep);
                const uint8_t type = IOUSBGetEndpointType(ep);
                Log("Start: endpoint 0x%02x type %u", addr, type);
                if (type == kIOUSBEndpointTypeInterrupt && (addr & 0x80)) {
                    notifyEp = addr;
                }
            }
        }
        IOUSBHostFreeDescriptor(config);
    }

    if (notifyEp != 0) {
        ret = ivars->interface->CopyPipe(notifyEp, &ivars->notifyPipe);
        if (ret == kIOReturnSuccess && ivars->notifyPipe) {
            IOAddressSegment range = {};
            if (IOBufferMemoryDescriptor::Create(kIOMemoryDirectionIn,
                    kNotifyBufferSize, 0, &ivars->notifyBuffer) == kIOReturnSuccess
                && ivars->notifyBuffer->GetAddressRange(&range) == kIOReturnSuccess
                && range.address != 0
                && CreateActionInterruptComplete(0, &ivars->notifyAction) == kIOReturnSuccess) {
                // Interrupt endpoints require completionTimeoutMs == 0.
                ret = ivars->notifyPipe->AsyncIO(
                    ivars->notifyBuffer, kNotifyBufferSize, ivars->notifyAction, 0);
                Log("Start: interrupt pump armed on 0x%02x -> 0x%x", notifyEp, ret);
            } else {
                Log("Start: interrupt pump setup incomplete (non-fatal)");
            }
        } else {
            Log("Start: CopyPipe(0x%02x) failed 0x%x (non-fatal)", notifyEp, ret);
        }
    } else {
        Log("Start: no interrupt-IN endpoint found (claim-only mode)");
    }

    ret = RegisterService();
    if (ret != kIOReturnSuccess) {
        Log("Start: RegisterService failed 0x%x", ret);
        return ret;
    }
    Log("Start ok: control interface claimed");
    return kIOReturnSuccess;
}

kern_return_t IMPL(LodestarUSBCommDriver, Stop)
{
    Log("Stop");
    ivars->stopping = true;
    if (ivars->notifyPipe) {
        ivars->notifyPipe->Abort(kIOUSBAbortSynchronous, kIOReturnAborted, nullptr);
    }
    OSSafeReleaseNULL(ivars->notifyAction);
    OSSafeReleaseNULL(ivars->notifyBuffer);
    OSSafeReleaseNULL(ivars->notifyPipe);
    if (ivars->interface) {
        ivars->interface->Close(this, 0);
        OSSafeReleaseNULL(ivars->interface);
    }
    return Stop(provider, SUPERDISPATCH);
}

// Runs on this service's default queue.
void IMPL(LodestarUSBCommDriver, InterruptComplete)
{
    (void)action; (void)completionTimestamp;
    if (status == kIOReturnAborted || ivars->stopping) return;
    Log("notification: status 0x%x bytes=%u", status, actualByteCount);
    ivars->notifyPipe->AsyncIO(ivars->notifyBuffer, kNotifyBufferSize,
                               ivars->notifyAction, 0);
}
