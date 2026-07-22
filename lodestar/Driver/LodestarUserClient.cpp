/*
 * SPDX-FileCopyrightText: 2026 Swift Raccoon
 * SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later
 */

#include <os/log.h>

#include <DriverKit/IOLib.h>
#include <DriverKit/IOService.h>
#include <DriverKit/IOUserClient.h>
#include <DriverKit/OSAction.h>
#include <DriverKit/OSData.h>

#include "LodestarUserClient.h"
#include "LodestarUSBSerialDriver.h"

#define Log(fmt, ...) os_log(OS_LOG_DEFAULT, "[Lodestar uc] " fmt, ##__VA_ARGS__)

// Selector values: the other half of the contract in
// Shared/Transport/USBSerialLink.swift (USBSerialSelector).
enum LodestarSelector : uint64_t {
    kSelectorWrite = 0,
    kSelectorRead = 1,
    kSelectorArmDoorbell = 2,
    kSelectorStatus = 3,
    kSelectorCopyLog = 4,
    kSelectorCount = 5,
};

constexpr size_t kMaxIOSize = 4096;

struct LodestarUserClient_IVars
{
    LodestarUSBSerialDriver *provider = nullptr;
};

// Checked dispatch table (positional order = selector order 0..3):
// mismatched shapes fail kIOReturnBadArgument before the handler runs.
static const IOUserClientMethodDispatch sDispatch[kSelectorCount] = {
    // kSelectorWrite: variable-size struct in, nothing out.
    {
        &LodestarUserClient::HandleWrite,
        /*checkCompletionExists*/ false,
        /*checkScalarInputCount*/ 0,
        /*checkStructureInputSize*/ kIOUserClientVariableStructureSize,
        /*checkScalarOutputCount*/ 0,
        /*checkStructureOutputSize*/ 0,
    },
    // kSelectorRead: nothing in, variable-size struct out.
    {
        &LodestarUserClient::HandleRead,
        /*checkCompletionExists*/ false,
        /*checkScalarInputCount*/ 0,
        /*checkStructureInputSize*/ 0,
        /*checkScalarOutputCount*/ 0,
        /*checkStructureOutputSize*/ kIOUserClientVariableStructureSize,
    },
    // kSelectorArmDoorbell: async completion required, no payload.
    {
        &LodestarUserClient::HandleArmDoorbell,
        /*checkCompletionExists*/ true,
        /*checkScalarInputCount*/ 0,
        /*checkStructureInputSize*/ 0,
        /*checkScalarOutputCount*/ 0,
        /*checkStructureOutputSize*/ 0,
    },
    // kSelectorStatus: 4 scalars out.
    {
        &LodestarUserClient::HandleStatus,
        /*checkCompletionExists*/ false,
        /*checkScalarInputCount*/ 0,
        /*checkStructureInputSize*/ 0,
        /*checkScalarOutputCount*/ 4,
        /*checkStructureOutputSize*/ 0,
    },
    // kSelectorCopyLog: nothing in, variable-size struct out.
    {
        &LodestarUserClient::HandleCopyLog,
        /*checkCompletionExists*/ false,
        /*checkScalarInputCount*/ 0,
        /*checkStructureInputSize*/ 0,
        /*checkScalarOutputCount*/ 0,
        /*checkStructureOutputSize*/ kIOUserClientVariableStructureSize,
    },
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
    if (!ivars->provider) return kIOReturnBadArgument;
    ivars->provider->retain();
    Log("Start ok");
    return kIOReturnSuccess;
}

kern_return_t IMPL(LodestarUserClient, Stop)
{
    if (ivars && ivars->provider) {
        ivars->provider->UnregisterUserClient(this);
        OSSafeReleaseNULL(ivars->provider);
    }
    return Stop(provider, SUPERDISPATCH);
}

kern_return_t LodestarUserClient::ExternalMethod(
    uint64_t selector, IOUserClientMethodArguments *arguments,
    const IOUserClientMethodDispatch *dispatch, OSObject *target, void *reference)
{
    (void)dispatch; (void)target;
    if (selector >= kSelectorCount) return kIOReturnBadArgument;
    return super::ExternalMethod(selector, arguments, &sDispatch[selector],
                                 this, reference);
}

void LodestarUserClient::FireDoorbell(OSAction *action, IOReturn status)
{
    // Zero-payload completion: the doorbell carries no data, only the
    // status (success = data available, aborted = teardown/re-arm).
    IOUserClientAsyncArgumentsArray noArgs = {};
    AsyncCompletion(action, status, noArgs, 0);
}

kern_return_t LodestarUserClient::HandleWrite(
    OSObject *target, void *reference, IOUserClientMethodArguments *arguments)
{
    (void)reference;
    auto *self = OSDynamicCast(LodestarUserClient, target);
    if (!self || !self->ivars->provider) return kIOReturnNotAttached;
    OSData *input = arguments->structureInput;
    if (!input) return kIOReturnBadArgument;
    const size_t len = input->getLength();
    if (len == 0 || len > kMaxIOSize) return kIOReturnBadArgument;
    const uint8_t *bytes =
        reinterpret_cast<const uint8_t *>(input->getBytesNoCopy());
    if (!bytes) return kIOReturnBadArgument;
    return self->ivars->provider->EnqueueWrite(bytes, len);
}

kern_return_t LodestarUserClient::HandleRead(
    OSObject *target, void *reference, IOUserClientMethodArguments *arguments)
{
    (void)reference;
    auto *self = OSDynamicCast(LodestarUserClient, target);
    if (!self || !self->ivars->provider) return kIOReturnNotAttached;
    uint8_t tmp[kMaxIOSize];
    size_t requested = kMaxIOSize;
    if (arguments->structureOutputMaximumSize > 0 &&
        arguments->structureOutputMaximumSize != kIOUserClientVariableStructureSize &&
        arguments->structureOutputMaximumSize < requested) {
        requested = arguments->structureOutputMaximumSize;
    }
    size_t actual = 0;
    kern_return_t ret =
        self->ivars->provider->CopyBufferedBytes(tmp, requested, &actual);
    if (ret != kIOReturnSuccess) return ret;
    arguments->structureOutput = OSData::withBytes(tmp, static_cast<uint32_t>(actual));
    return arguments->structureOutput ? kIOReturnSuccess : kIOReturnNoMemory;
}

kern_return_t LodestarUserClient::HandleArmDoorbell(
    OSObject *target, void *reference, IOUserClientMethodArguments *arguments)
{
    (void)reference;
    auto *self = OSDynamicCast(LodestarUserClient, target);
    if (!self || !self->ivars->provider) return kIOReturnNotAttached;
    if (!arguments->completion) return kIOReturnBadArgument;
    self->ivars->provider->RegisterDoorbell(self, arguments->completion);
    return kIOReturnSuccess;
}

kern_return_t LodestarUserClient::HandleStatus(
    OSObject *target, void *reference, IOUserClientMethodArguments *arguments)
{
    (void)reference;
    auto *self = OSDynamicCast(LodestarUserClient, target);
    if (!self || !self->ivars->provider) return kIOReturnNotAttached;
    uint64_t status[4] = {0, 0, 0, 0};
    self->ivars->provider->CopyStatus(status);
    for (int i = 0; i < 4; i++) arguments->scalarOutput[i] = status[i];
    arguments->scalarOutputCount = 4;
    return kIOReturnSuccess;
}

kern_return_t LodestarUserClient::HandleCopyLog(
    OSObject *target, void *reference, IOUserClientMethodArguments *arguments)
{
    (void)reference;
    auto *self = OSDynamicCast(LodestarUserClient, target);
    if (!self || !self->ivars->provider) return kIOReturnNotAttached;
    uint8_t tmp[kMaxIOSize];
    size_t requested = kMaxIOSize;
    if (arguments->structureOutputMaximumSize > 0 &&
        arguments->structureOutputMaximumSize != kIOUserClientVariableStructureSize &&
        arguments->structureOutputMaximumSize < requested) {
        requested = arguments->structureOutputMaximumSize;
    }
    size_t actual = 0;
    self->ivars->provider->CopyLogEntries(tmp, requested, &actual);
    arguments->structureOutput = OSData::withBytes(tmp, static_cast<uint32_t>(actual));
    return arguments->structureOutput ? kIOReturnSuccess : kIOReturnNoMemory;
}
