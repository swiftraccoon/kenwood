/*
 * SPDX-FileCopyrightText: 2026 Swift Raccoon
 * SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later
 */

#include <os/log.h>

#include <DriverKit/IOLib.h>
#include <DriverKit/IOService.h>
#include <DriverKit/IOUserClient.h>

#include "LodestarUserClient.h"
#include "LodestarUSBSerialDriver.h"

#define Log(fmt, ...) os_log(OS_LOG_DEFAULT, "[Lodestar uc] " fmt, ##__VA_ARGS__)

// External method selectors. Keep in sync with the `MethodSelector`
// enum on the Swift side (`USBSerialTransport.swift`).
enum {
    kMethodWrite = 0,
    kMethodRead  = 1,
};

#define kMaxIOSize 4096

struct LodestarUserClient_IVars
{
    LodestarUSBSerialDriver *provider = nullptr;
};

bool LodestarUserClient::init()
{
    if (!super::init()) return false;
    ivars = IONewZero(LodestarUserClient_IVars, 1);
    return ivars != nullptr;
}

void LodestarUserClient::free()
{
    if (ivars) IOSafeDeleteNULL(ivars, LodestarUserClient_IVars, 1);
    super::free();
}

kern_return_t IMPL(LodestarUserClient, Start)
{
    kern_return_t ret = Start(provider, SUPERDISPATCH);
    if (ret != kIOReturnSuccess) return ret;
    ivars->provider = OSDynamicCast(LodestarUSBSerialDriver, provider);
    if (ivars->provider == nullptr) return kIOReturnBadArgument;
    ivars->provider->retain();
    Log("Start ok");
    return kIOReturnSuccess;
}

kern_return_t IMPL(LodestarUserClient, Stop)
{
    if (ivars && ivars->provider) {
        OSSafeReleaseNULL(ivars->provider);
    }
    return Stop(provider, SUPERDISPATCH);
}

// External method dispatch deferred until on-hardware iteration —
// `iig` ExternalMethod argument-struct conventions need exercising
// against a real DriverKit build environment with the right SDK
// generator output. For now the user client exists so the app can
// open it via IOServiceOpen; method calls return kIOReturnUnsupported.
