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

#include "AzimuthUSBSerialDriver.h"
#include "AzimuthUserClient.h"

#define Log(fmt, ...) os_log(OS_LOG_DEFAULT, "[Azimuth user client] " fmt, ##__VA_ARGS__)

// External-method ABI v1 + append-only v2. Values must exactly mirror the
// versioned selector enums in Shared/Transport/USBSerialLink.swift.
enum AzimuthSelectorV1 : uint64_t {
    kAzimuthSelectorV1Write = 0,
    kAzimuthSelectorV1Read = 1,
    kAzimuthSelectorV1ArmDoorbell = 2,
    kAzimuthSelectorV1Status = 3,
    kAzimuthSelectorV1CopyLog = 4,
    kAzimuthSelectorV1Count = 5,
};

enum AzimuthSelectorV2 : uint64_t {
    kAzimuthSelectorV2SetBaudRate = 5,
    kAzimuthSelectorV2Count = 6,
};

constexpr size_t kMaximumTransferSize = 4096;
static_assert(kAzimuthSelectorV1CopyLog + 1 == kAzimuthSelectorV1Count,
              "selector v1 table must be contiguous");
static_assert(static_cast<uint64_t>(kAzimuthSelectorV2SetBaudRate)
                  == static_cast<uint64_t>(kAzimuthSelectorV1Count),
              "selector v2 must append to v1");

struct AzimuthUserClient_IVars
{
    AzimuthUSBSerialDriver *provider = nullptr;
};

static const IOUserClientMethodDispatch
sAzimuthDispatchV2[kAzimuthSelectorV2Count] = {
    {
        &AzimuthUserClient::HandleWrite,
        false,
        0,
        kIOUserClientVariableStructureSize,
        0,
        0,
    },
    {
        &AzimuthUserClient::HandleRead,
        false,
        0,
        0,
        0,
        kIOUserClientVariableStructureSize,
    },
    {
        &AzimuthUserClient::HandleArmDoorbell,
        true,
        0,
        0,
        0,
        0,
    },
    {
        &AzimuthUserClient::HandleStatus,
        false,
        0,
        0,
        4,
        0,
    },
    {
        &AzimuthUserClient::HandleCopyLog,
        false,
        0,
        0,
        0,
        kIOUserClientVariableStructureSize,
    },
    {
        &AzimuthUserClient::HandleSetBaudRate,
        false,
        1,
        0,
        0,
        0,
    },
};

bool AzimuthUserClient::init()
{
    if (!super::init()) return false;
    ivars = IONewZero(AzimuthUserClient_IVars, 1);
    return ivars != nullptr;
}

void AzimuthUserClient::free()
{
    if (ivars) IOSafeDeleteNULL(ivars, AzimuthUserClient_IVars, 1);
    super::free();
}

kern_return_t IMPL(AzimuthUserClient, Start)
{
    kern_return_t result = Start(provider, SUPERDISPATCH);
    if (result != kIOReturnSuccess) return result;
    ivars->provider = OSDynamicCast(AzimuthUSBSerialDriver, provider);
    if (!ivars->provider) return kIOReturnBadArgument;
    ivars->provider->retain();
    Log("started");
    return kIOReturnSuccess;
}

kern_return_t IMPL(AzimuthUserClient, Stop)
{
    if (ivars && ivars->provider) {
        ivars->provider->UnregisterUserClient(this);
        OSSafeReleaseNULL(ivars->provider);
    }
    return Stop(provider, SUPERDISPATCH);
}

kern_return_t AzimuthUserClient::ExternalMethod(
    uint64_t selector,
    IOUserClientMethodArguments *arguments,
    const IOUserClientMethodDispatch *dispatch,
    OSObject *target,
    void *reference)
{
    (void)dispatch;
    (void)target;
    if (selector >= kAzimuthSelectorV2Count) return kIOReturnBadArgument;
    return super::ExternalMethod(
        selector,
        arguments,
        &sAzimuthDispatchV2[selector],
        this,
        reference);
}

void AzimuthUserClient::FireDoorbell(OSAction *action, IOReturn status)
{
    IOUserClientAsyncArgumentsArray noArguments = {};
    AsyncCompletion(action, status, noArguments, 0);
}

kern_return_t AzimuthUserClient::HandleWrite(
    OSObject *target,
    void *reference,
    IOUserClientMethodArguments *arguments)
{
    (void)reference;
    auto *self = OSDynamicCast(AzimuthUserClient, target);
    if (!self || !self->ivars->provider) return kIOReturnNotAttached;
    OSData *input = arguments->structureInput;
    if (!input) return kIOReturnBadArgument;
    const size_t length = input->getLength();
    if (length == 0 || length > kMaximumTransferSize) return kIOReturnBadArgument;
    const uint8_t *bytes =
        reinterpret_cast<const uint8_t *>(input->getBytesNoCopy());
    if (!bytes) return kIOReturnBadArgument;
    return self->ivars->provider->EnqueueWrite(bytes, length);
}

kern_return_t AzimuthUserClient::HandleRead(
    OSObject *target,
    void *reference,
    IOUserClientMethodArguments *arguments)
{
    (void)reference;
    auto *self = OSDynamicCast(AzimuthUserClient, target);
    if (!self || !self->ivars->provider) return kIOReturnNotAttached;
    uint8_t bytes[kMaximumTransferSize];
    size_t requested = kMaximumTransferSize;
    if (arguments->structureOutputMaximumSize > 0
        && arguments->structureOutputMaximumSize
            != kIOUserClientVariableStructureSize
        && arguments->structureOutputMaximumSize < requested) {
        requested = arguments->structureOutputMaximumSize;
    }
    size_t actual = 0;
    kern_return_t result = self->ivars->provider->CopyBufferedBytes(
        bytes, requested, &actual);
    if (result != kIOReturnSuccess) return result;
    arguments->structureOutput =
        OSData::withBytes(bytes, static_cast<uint32_t>(actual));
    return arguments->structureOutput ? kIOReturnSuccess : kIOReturnNoMemory;
}

kern_return_t AzimuthUserClient::HandleArmDoorbell(
    OSObject *target,
    void *reference,
    IOUserClientMethodArguments *arguments)
{
    (void)reference;
    auto *self = OSDynamicCast(AzimuthUserClient, target);
    if (!self || !self->ivars->provider) return kIOReturnNotAttached;
    if (!arguments->completion) return kIOReturnBadArgument;
    return self->ivars->provider->RegisterDoorbell(
        self, arguments->completion);
}

kern_return_t AzimuthUserClient::HandleStatus(
    OSObject *target,
    void *reference,
    IOUserClientMethodArguments *arguments)
{
    (void)reference;
    auto *self = OSDynamicCast(AzimuthUserClient, target);
    if (!self || !self->ivars->provider) return kIOReturnNotAttached;
    uint64_t status[4] = {0, 0, 0, 0};
    self->ivars->provider->CopyStatus(status);
    for (int index = 0; index < 4; index++) {
        arguments->scalarOutput[index] = status[index];
    }
    arguments->scalarOutputCount = 4;
    return kIOReturnSuccess;
}

kern_return_t AzimuthUserClient::HandleCopyLog(
    OSObject *target,
    void *reference,
    IOUserClientMethodArguments *arguments)
{
    (void)reference;
    auto *self = OSDynamicCast(AzimuthUserClient, target);
    if (!self || !self->ivars->provider) return kIOReturnNotAttached;
    uint8_t bytes[kMaximumTransferSize];
    size_t requested = kMaximumTransferSize;
    if (arguments->structureOutputMaximumSize > 0
        && arguments->structureOutputMaximumSize
            != kIOUserClientVariableStructureSize
        && arguments->structureOutputMaximumSize < requested) {
        requested = arguments->structureOutputMaximumSize;
    }
    size_t actual = 0;
    self->ivars->provider->CopyLogEntries(bytes, requested, &actual);
    arguments->structureOutput =
        OSData::withBytes(bytes, static_cast<uint32_t>(actual));
    return arguments->structureOutput ? kIOReturnSuccess : kIOReturnNoMemory;
}

kern_return_t AzimuthUserClient::HandleSetBaudRate(
    OSObject *target,
    void *reference,
    IOUserClientMethodArguments *arguments)
{
    (void)reference;
    auto *self = OSDynamicCast(AzimuthUserClient, target);
    if (!self || !self->ivars->provider) return kIOReturnNotAttached;
    const uint64_t baudRate = arguments->scalarInput[0];
    if (baudRate != 9600 && baudRate != 115200) return kIOReturnBadArgument;
    return self->ivars->provider->SetBaudRate(static_cast<uint32_t>(baudRate));
}
