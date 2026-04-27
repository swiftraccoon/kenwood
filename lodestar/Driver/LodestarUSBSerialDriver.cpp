/*
 * SPDX-FileCopyrightText: 2026 Swift Raccoon
 * SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later
 *
 * LodestarUSBSerialDriver — DriverKit dext that pumps bytes between an
 * iPad app and a USB-CDC TH-D75 radio.
 *
 * ## Status: scaffolded skeleton
 *
 * This source compiles into a `.dext` that matches the TH-D75 by
 * VID/PID, opens the USB interface, and registers as a service. It
 * does NOT yet implement the bulk-IN/bulk-OUT pump — endpoint
 * discovery and async I/O against the DriverKit 25.5 USBDriverKit
 * surface are the next iteration step, best done against the actual
 * SDK headers + real hardware. Stubs below return kIOReturnUnsupported
 * so the build is green and the project structure is verified
 * end-to-end.
 *
 * ## Wire protocol
 *
 * Wire bytes are identical to the macOS Bluetooth SPP path: same CAT
 * ASCII, same MMDVM 0xE0-prefixed frames. `MmdvmReader`,
 * `MmdvmWriter`, `RadioModeProber`, and the entire CAT stack reuse
 * unchanged once the byte pump is wired up.
 */

#include <os/log.h>

#include <DriverKit/IOLib.h>
#include <DriverKit/IOUserServer.h>
#include <DriverKit/IOService.h>
#include <DriverKit/IOTypes.h>
#include <DriverKit/OSAction.h>
#include <DriverKit/OSData.h>
#include <DriverKit/OSDictionary.h>
#include <DriverKit/OSNumber.h>

#include <USBDriverKit/IOUSBHostInterface.h>

#include "LodestarUSBSerialDriver.h"
#include "LodestarUserClient.h"

#define Log(fmt, ...) os_log(OS_LOG_DEFAULT, "[Lodestar dext] " fmt, ##__VA_ARGS__)

struct LodestarUSBSerialDriver_IVars
{
    IOUSBHostInterface *interface = nullptr;
};

bool LodestarUSBSerialDriver::init()
{
    bool ok = super::init();
    if (!ok) {
        Log("init: super::init failed");
        return false;
    }
    ivars = IONewZero(LodestarUSBSerialDriver_IVars, 1);
    if (ivars == nullptr) {
        Log("init: IONewZero failed");
        return false;
    }
    Log("init ok");
    return true;
}

void LodestarUSBSerialDriver::free()
{
    if (ivars) {
        IOSafeDeleteNULL(ivars, LodestarUSBSerialDriver_IVars, 1);
    }
    super::free();
}

kern_return_t IMPL(LodestarUSBSerialDriver, Start)
{
    kern_return_t ret = Start(provider, SUPERDISPATCH);
    if (ret != kIOReturnSuccess) {
        Log("Start: super::Start failed 0x%x", ret);
        return ret;
    }

    ivars->interface = OSDynamicCast(IOUSBHostInterface, provider);
    if (ivars->interface == nullptr) {
        Log("Start: provider is not IOUSBHostInterface");
        Stop(provider, SUPERDISPATCH);
        return kIOReturnNoDevice;
    }
    ivars->interface->retain();

    ret = ivars->interface->Open(this, 0, nullptr);
    if (ret != kIOReturnSuccess) {
        Log("Start: interface Open failed 0x%x", ret);
        Stop(provider, SUPERDISPATCH);
        return ret;
    }

    // TODO: enumerate config + interface descriptors via the DriverKit
    // 25.5 USBDriverKit API (`CopyConfigurationDescriptor`,
    // `CopyInterfaceDescriptor`, etc.) and pick out the bulk IN/OUT
    // endpoints. The exact API names need on-device verification.
    // Then schedule async IN reads via IOUSBHostPipe::AsyncIO and a
    // CreateActionbulkInRead action.

    ret = RegisterService();
    if (ret != kIOReturnSuccess) {
        Log("Start: RegisterService failed 0x%x", ret);
        return ret;
    }

    Log("Start ok (skeleton — bulk endpoints not yet wired)");
    return kIOReturnSuccess;
}

kern_return_t IMPL(LodestarUSBSerialDriver, Stop)
{
    Log("Stop");

    if (ivars && ivars->interface) {
        ivars->interface->Close(this, 0);
        OSSafeReleaseNULL(ivars->interface);
    }

    return Stop(provider, SUPERDISPATCH);
}

void IMPL(LodestarUSBSerialDriver, bulkInRead)
{
    Log("bulkInRead: status=0x%x bytes=%u (skeleton — pump not wired)",
        status, actualByteCount);
}

kern_return_t LodestarUSBSerialDriver::WriteToRadio(const uint8_t *data, size_t length)
{
    (void)data; (void)length;
    return kIOReturnUnsupported;
}

kern_return_t LodestarUSBSerialDriver::ReadFromRadio(uint8_t *out, size_t maxBytes, size_t *actualBytes)
{
    (void)out; (void)maxBytes;
    if (actualBytes) *actualBytes = 0;
    return kIOReturnUnsupported;
}

kern_return_t IMPL(LodestarUSBSerialDriver, NewUserClient)
{
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
