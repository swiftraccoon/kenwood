// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

#if os(macOS)

import CoreAudio
import Foundation
import IOKit
import Synchronization

struct IFDSPMacUSBIdentity: Hashable, Sendable {
    let vendorID: UInt16
    let productID: UInt16
    let registryEntryID: UInt64

    var isTHD75: Bool {
        vendorID == POSIXAzimuthUSBSerialLink.thD75VendorID
            && productID == POSIXAzimuthUSBSerialLink.thD75ProductID
    }

    func matches(usbDeviceRegistryEntryID expected: UInt64) -> Bool {
        expected != 0 && registryEntryID == expected
    }
}

/// Descriptor facts read from one current physical USB-device registry node.
/// The CoreAudio UID's fourth field is either this descriptor serial or this
/// device's USB location ID. The registry entry ID remains the final exact
/// join to the CAT-qualified CDC interface.
struct IFDSPMacUSBDeviceDescriptor: Equatable, Sendable {
    let identity: IFDSPMacUSBIdentity
    let serialNumber: String?
    let locationID: UInt32?
}

/// Resolves Apple's documented USB Audio device UID without using the visible
/// audio-device name. macOS 26 and later no longer publish the legacy
/// AppleUSBAudioEngine IORegistry service, so the UID's serial-or-location
/// field is the supported association back to the physical USB device.
enum IFDSPAppleUSBAudioUIDResolver {
    private static let prefix = "AppleUSBAudioEngine"

    static func identity(
        for audioDeviceUID: String,
        among devices: [IFDSPMacUSBDeviceDescriptor]
    ) -> IFDSPMacUSBIdentity? {
        guard let token = identityToken(in: audioDeviceUID) else { return nil }
        let matches = devices.filter { device in
            let matchesDescriptorSerial = device.serialNumber.map {
                !$0.isEmpty && token == $0
            } ?? false
            let matchesLocation = device.locationID.map { locationID in
                parseLocationID(token) == locationID
            } ?? false
            return matchesDescriptorSerial || matchesLocation
        }
        return matches.count == 1 ? matches[0].identity : nil
    }

    private static func identityToken(in uid: String) -> String? {
        let fields = uid.split(
            separator: ":",
            maxSplits: 4,
            omittingEmptySubsequences: false
        )
        guard fields.count == 5,
              fields[0] == Substring(prefix),
              !fields[1].isEmpty,
              !fields[2].isEmpty,
              !fields[3].isEmpty,
              validInterfaceList(fields[4]) else {
            return nil
        }
        return String(fields[3])
    }

    private static func validInterfaceList(_ value: Substring) -> Bool {
        let interfaces = value.split(
            separator: ",",
            omittingEmptySubsequences: false
        )
        return !interfaces.isEmpty && interfaces.allSatisfy { field in
            guard !field.isEmpty,
                  field.allSatisfy(\.isNumber),
                  let number = UInt16(field) else {
                return false
            }
            return number <= UInt16(UInt8.max)
        }
    }

    private static func parseLocationID(_ value: String) -> UInt32? {
        let digits = value.hasPrefix("0x") || value.hasPrefix("0X")
            ? String(value.dropFirst(2))
            : value
        guard !digits.isEmpty,
              digits.allSatisfy(\.isHexDigit) else {
            return nil
        }
        return UInt32(digits, radix: 16)
    }
}

struct IFDSPMacPCMFormat: Equatable, Sendable {
    let sampleRate: Double
    let formatID: AudioFormatID
    let formatFlags: AudioFormatFlags
    let bytesPerPacket: UInt32
    let framesPerPacket: UInt32
    let bytesPerFrame: UInt32
    let channelsPerFrame: UInt32
    let bitsPerChannel: UInt32

    init(_ format: AudioStreamBasicDescription) {
        sampleRate = format.mSampleRate
        formatID = format.mFormatID
        formatFlags = format.mFormatFlags
        bytesPerPacket = format.mBytesPerPacket
        framesPerPacket = format.mFramesPerPacket
        bytesPerFrame = format.mBytesPerFrame
        channelsPerFrame = format.mChannelsPerFrame
        bitsPerChannel = format.mBitsPerChannel
    }

    var bytesPerSample: UInt32 { bitsPerChannel / 8 }

    var isNonInterleaved: Bool {
        formatFlags & kAudioFormatFlagIsNonInterleaved != 0
    }
}

struct IFDSPMacAudioDevice: Sendable {
    let audioDeviceID: AudioDeviceID
    let uid: String
    let name: String
    let transportType: UInt32
    let inputChannelCount: Int
    let bufferFrameSize: UInt32
    let sampleRate: Double
    let streamFormat: IFDSPMacPCMFormat
    let usbIdentity: IFDSPMacUSBIdentity?
    let isAlive: Bool

    var isVerifiedTHD75Input: Bool {
        captureRejectionReasons.isEmpty
            && usbIdentity?.isTHD75 == true
            && usbIdentity?.registryEntryID != 0
    }

    var captureRejectionReasons: [String] {
        var reasons: [String] = []
        if transportType != kAudioDeviceTransportTypeUSB {
            reasons.append("CoreAudio did not report USB transport")
        }
        if inputChannelCount <= 0 {
            reasons.append("the device has no input channels")
        } else {
            if inputChannelCount != Int(streamFormat.channelsPerFrame) {
                reasons.append(
                    "the input channel count does not match its PCM stream format"
                )
            }
            if bufferFrameSize == 0 {
                reasons.append("CoreAudio reported a zero frame-buffer size")
            }
            if !sampleRate.isFinite || sampleRate <= 0 {
                reasons.append("CoreAudio reported an invalid sample rate")
            }
            if !IFDSPMacPCMDecoder.supports(streamFormat) {
                reasons.append("the PCM stream format is unsupported")
            }
        }
        if !isAlive {
            reasons.append("CoreAudio reported the device is not alive")
        }
        return reasons
    }

    func hasSamePhysicalIdentity(as expected: Self) -> Bool {
        audioDeviceID == expected.audioDeviceID
            && uid == expected.uid
            && transportType == expected.transportType
            && usbIdentity == expected.usbIdentity
    }

    func hasSameCaptureContract(as expected: Self) -> Bool {
        hasSamePhysicalIdentity(as: expected)
            && inputChannelCount == expected.inputChannelCount
            && bufferFrameSize == expected.bufferFrameSize
            && streamFormat == expected.streamFormat
            && isVerifiedTHD75Input
    }
}

enum IFDSPMacAudioDeviceSelector {
    static func selectTHD75Input(
        from devices: [IFDSPMacAudioDevice],
        expectedUSBDeviceRegistryEntryID: UInt64,
        expectedCATSerialNumber: String
    ) throws -> IFDSPMacAudioDevice {
        guard expectedUSBDeviceRegistryEntryID != 0 else {
            throw IFDSPMacAudioError.invalidExpectedUSBDeviceRegistryEntryID
        }
        let identityMatches = devices.filter {
            $0.usbIdentity?.isTHD75 == true
                && $0.usbIdentity?.matches(
                    usbDeviceRegistryEntryID: expectedUSBDeviceRegistryEntryID
                ) == true
        }
        guard identityMatches.count <= 1 else {
            throw IFDSPMacAudioError.ambiguousExpectedRadioAudio(
                expectedCATSerialNumber: expectedCATSerialNumber,
                matchCount: identityMatches.count
            )
        }
        if let selected = identityMatches.first {
            let reasons = selected.captureRejectionReasons
            guard reasons.isEmpty else {
                throw IFDSPMacAudioError.expectedRadioAudioNotReady(
                    name: selected.name,
                    reasons: reasons
                )
            }
            return selected
        }

        let candidates = devices.filter {
            $0.usbIdentity?.isTHD75 == true
                && $0.usbIdentity?.registryEntryID != 0
        }
        guard candidates.isEmpty else {
            throw IFDSPMacAudioError.expectedRadioAudioUnavailable(
                expectedCATSerialNumber: expectedCATSerialNumber,
                expectedUSBDeviceRegistryEntryID: expectedUSBDeviceRegistryEntryID,
                verifiedDeviceCount: candidates.count
            )
        }

        let inputs = devices.filter { $0.inputChannelCount > 0 }
        let rejections = inputs.compactMap { device -> String? in
            guard device.transportType == kAudioDeviceTransportTypeUSB else {
                return nil
            }
            if device.uid.hasPrefix("AppleUSBAudioEngine:") {
                return "\(device.name): its Apple USB Audio UID did not resolve to "
                    + "the current CAT-qualified TH-D75 USB device"
            }
            return "\(device.name): CoreAudio did not expose an Apple USB Audio UID"
        }
        guard !inputs.isEmpty else {
            throw IFDSPMacAudioError.noVerifiedTHD75Input(
                availableInputs: [],
                candidateRejections: []
            )
        }
        throw IFDSPMacAudioError.noVerifiedTHD75Input(
            availableInputs: inputNames(in: devices),
            candidateRejections: rejections
        )
    }

    private static func inputNames(in devices: [IFDSPMacAudioDevice]) -> [String] {
        devices
            .filter { $0.inputChannelCount > 0 }
            .map(\.name)
            .sorted()
    }
}

enum IFDSPMacAudioError: LocalizedError, Equatable {
    case noVerifiedTHD75Input(
        availableInputs: [String],
        candidateRejections: [String]
    )
    case missingMacOSUSBDeviceRegistryEntryID
    case invalidExpectedUSBDeviceRegistryEntryID
    case expectedRadioAudioUnavailable(
        expectedCATSerialNumber: String,
        expectedUSBDeviceRegistryEntryID: UInt64,
        verifiedDeviceCount: Int
    )
    case ambiguousExpectedRadioAudio(
        expectedCATSerialNumber: String,
        matchCount: Int
    )
    case expectedRadioAudioNotReady(name: String, reasons: [String])
    case audioDeviceInventoryIncomplete(queryFailures: [String])
    case deviceIdentityChanged
    case unsupportedInputFormat
    case coreAudio(operation: String, status: OSStatus)
    case usbDeviceInventoryUnavailable(
        operation: String,
        status: kern_return_t?
    )

    var isRetryableAudioReadinessFailure: Bool {
        switch self {
        case .noVerifiedTHD75Input,
             .expectedRadioAudioUnavailable,
             .expectedRadioAudioNotReady,
             .audioDeviceInventoryIncomplete,
             .usbDeviceInventoryUnavailable:
            return true
        case .missingMacOSUSBDeviceRegistryEntryID,
             .invalidExpectedUSBDeviceRegistryEntryID,
             .ambiguousExpectedRadioAudio,
             .deviceIdentityChanged,
             .unsupportedInputFormat,
             .coreAudio:
            return false
        }
    }

    var errorDescription: String? {
        switch self {
        case .noVerifiedTHD75Input(_, let rejections):
            let detail = rejections.isEmpty
                ? ""
                : " CoreAudio candidates were rejected: \(rejections.joined(separator: "; "))."
            return "No input-capable CoreAudio device could be proved as the connected "
                + "TH-D75 by exact USB identity. Azimuth did not use the Mac's default input."
                + detail
        case .missingMacOSUSBDeviceRegistryEntryID:
            return "The CAT-qualified USB-C connection did not retain its current macOS "
                + "USB device identity. USB IF capture did not start."
        case .invalidExpectedUSBDeviceRegistryEntryID:
            return "The CAT-qualified USB-C connection reported an invalid macOS USB "
                + "device identity. USB IF capture did not start."
        case .expectedRadioAudioUnavailable(let serialNumber, _, let count):
            return "Azimuth found \(count) verified TH-D75 USB audio input(s), but none "
                + "belonged to the same physical USB device as CAT radio \(serialNumber). "
                + "Capture did not start; no other input was used."
        case .ambiguousExpectedRadioAudio(let serialNumber, let count):
            return "Azimuth found \(count) CoreAudio inputs tied to the same physical "
                + "USB device as CAT radio \(serialNumber). Capture did not start because "
                + "the input was ambiguous."
        case .expectedRadioAudioNotReady(let name, let reasons):
            return "The exact \(name) input belongs to the CAT-qualified TH-D75 but is not "
                + "capture-ready: \(reasons.joined(separator: "; "))."
        case .audioDeviceInventoryIncomplete(let failures):
            return "CoreAudio could not inspect every current audio device, so Azimuth "
                + "could not safely prove the TH-D75 input: "
                + failures.joined(separator: "; ")
        case .deviceIdentityChanged:
            return "The verified TH-D75 CoreAudio device changed before capture could start. "
                + "Azimuth did not fall back to another input."
        case .unsupportedInputFormat:
            return "The verified TH-D75 USB audio input exposed a PCM format Azimuth cannot "
                + "decode safely. Capture did not start."
        case .coreAudio(let operation, let status):
            return "CoreAudio failed while \(operation) (OSStatus \(status))."
        case .usbDeviceInventoryUnavailable(let operation, let status):
            let statusDescription = status.map { " (kernel status \($0))" } ?? ""
            return "macOS could not inspect the current physical USB devices while "
                + "\(operation)\(statusDescription). Azimuth could not prove which audio "
                + "input belongs to the CAT-qualified TH-D75."
        }
    }
}

protocol IFDSPMacAudioCaptureSession: AnyObject, Sendable {
    func stop()
}

struct IFDSPMacAudioDeviceInventory: Sendable {
    let devices: [IFDSPMacAudioDevice]
    let queryFailures: [String]
}

@MainActor
protocol IFDSPMacAudioBackend: AnyObject, Sendable {
    func availableDeviceInventory() throws -> IFDSPMacAudioDeviceInventory
    func revalidate(_ device: IFDSPMacAudioDevice) throws -> IFDSPMacAudioDevice
    func startCapture(
        device: IFDSPMacAudioDevice,
        receive: @escaping @Sendable (IFDSPSourcePCMBlock) -> Void,
        overrun: @escaping @Sendable (_ blockCount: Int, _ sampleCount: Int) -> Void,
        deviceLost: @escaping @Sendable (String) -> Void,
        captureFailed: @escaping @Sendable (String) -> Void
    ) throws -> any IFDSPMacAudioCaptureSession
}

@MainActor
final class IFDSPSystemMacAudioBackend: IFDSPMacAudioBackend, @unchecked Sendable {
    func availableDeviceInventory() throws -> IFDSPMacAudioDeviceInventory {
        try IFDSPMacAudioHardware.availableDeviceInventory()
    }

    func revalidate(_ device: IFDSPMacAudioDevice) throws -> IFDSPMacAudioDevice {
        let current = try IFDSPMacAudioHardware.device(id: device.audioDeviceID)
        guard current.hasSamePhysicalIdentity(as: device) else {
            throw IFDSPMacAudioError.deviceIdentityChanged
        }
        return current
    }

    func startCapture(
        device: IFDSPMacAudioDevice,
        receive: @escaping @Sendable (IFDSPSourcePCMBlock) -> Void,
        overrun: @escaping @Sendable (_ blockCount: Int, _ sampleCount: Int) -> Void,
        deviceLost: @escaping @Sendable (String) -> Void,
        captureFailed: @escaping @Sendable (String) -> Void
    ) throws -> any IFDSPMacAudioCaptureSession {
        let session = try IFDSPSystemMacAudioCaptureSession(
            device: device,
            receive: receive,
            overrun: overrun,
            deviceLost: deviceLost,
            captureFailed: captureFailed
        )
        try session.start()
        return session
    }
}

private enum IFDSPMacAudioHardware {
    static func availableDeviceInventory() throws
        -> IFDSPMacAudioDeviceInventory {
        let usbDevices = try currentTHD75USBDevices()
        var devices: [IFDSPMacAudioDevice] = []
        var failures: [String] = []
        for id in try deviceIDs() {
            do {
                if let device = try inventoryDevice(
                    id: id,
                    usbDevices: usbDevices
                ) {
                    devices.append(device)
                }
            } catch {
                failures.append(
                    "CoreAudio device ID \(id): \(error.localizedDescription)"
                )
            }
        }
        return IFDSPMacAudioDeviceInventory(
            devices: devices,
            queryFailures: failures
        )
    }

    static func device(id: AudioDeviceID) throws -> IFDSPMacAudioDevice {
        try captureReadyDevice(
            id: id,
            usbDevices: currentTHD75USBDevices()
        )
    }

    /// Queries only properties relevant to deciding whether a current HAL
    /// object could be the radio input. A proven non-USB output is skipped
    /// without asking it for an input format or buffer size it cannot expose.
    private static func inventoryDevice(
        id: AudioDeviceID,
        usbDevices: [IFDSPMacUSBDeviceDescriptor]
    ) throws -> IFDSPMacAudioDevice? {
        let transportType = try valueProperty(
            objectID: id,
            selector: kAudioDevicePropertyTransportType,
            initialValue: UInt32(0)
        )
        guard transportType == kAudioDeviceTransportTypeUSB else {
            // Once HAL proves this object is not USB, it cannot be the radio.
            // Its input properties are best-effort UI context and must not
            // invalidate the exact USB-device proof.
            guard let inputChannelCount = try? inputChannelCount(deviceID: id),
                  inputChannelCount > 0 else {
                return nil
            }
            return IFDSPMacAudioDevice(
                audioDeviceID: id,
                uid: "non-usb-coreaudio-\(id)",
                name: bestEffortDeviceName(id: id),
                transportType: transportType,
                inputChannelCount: inputChannelCount,
                bufferFrameSize: 0,
                sampleRate: 0,
                streamFormat: IFDSPMacPCMFormat(
                    AudioStreamBasicDescription()
                ),
                usbIdentity: nil,
                isAlive: true
            )
        }

        // Every remaining object is USB and could be the expected radio, so
        // failures in these identity/readiness properties remain fail-closed.
        let inputChannelCount = try inputChannelCount(deviceID: id)
        let uid = try stringProperty(
            objectID: id,
            selector: kAudioDevicePropertyDeviceUID
        )
        let usbIdentity = usbIdentity(
            audioDeviceUID: uid,
            currentUSBDevices: usbDevices
        )

        if inputChannelCount == 0 {
            guard usbIdentity?.isTHD75 == true else { return nil }
            return IFDSPMacAudioDevice(
                audioDeviceID: id,
                uid: uid,
                name: bestEffortDeviceName(id: id),
                transportType: transportType,
                inputChannelCount: 0,
                bufferFrameSize: 0,
                sampleRate: 0,
                streamFormat: IFDSPMacPCMFormat(
                    AudioStreamBasicDescription()
                ),
                usbIdentity: usbIdentity,
                isAlive: try isAlive(deviceID: id)
            )
        }

        guard usbIdentity?.isTHD75 == true else {
            return IFDSPMacAudioDevice(
                audioDeviceID: id,
                uid: uid,
                name: bestEffortDeviceName(id: id),
                transportType: transportType,
                inputChannelCount: inputChannelCount,
                bufferFrameSize: 0,
                sampleRate: 0,
                streamFormat: IFDSPMacPCMFormat(
                    AudioStreamBasicDescription()
                ),
                usbIdentity: nil,
                isAlive: true
            )
        }
        return try captureReadyDevice(
            id: id,
            uid: uid,
            transportType: transportType,
            inputChannelCount: inputChannelCount,
            usbIdentity: usbIdentity
        )
    }

    private static func captureReadyDevice(
        id: AudioDeviceID,
        usbDevices: [IFDSPMacUSBDeviceDescriptor]
    ) throws -> IFDSPMacAudioDevice {
        let uid = try stringProperty(
            objectID: id,
            selector: kAudioDevicePropertyDeviceUID
        )
        let transportType = try valueProperty(
            objectID: id,
            selector: kAudioDevicePropertyTransportType,
            initialValue: UInt32(0)
        )
        let inputChannelCount = try inputChannelCount(deviceID: id)
        return try captureReadyDevice(
            id: id,
            uid: uid,
            transportType: transportType,
            inputChannelCount: inputChannelCount,
            usbIdentity: transportType == kAudioDeviceTransportTypeUSB
                ? usbIdentity(
                    audioDeviceUID: uid,
                    currentUSBDevices: usbDevices
                )
                : nil
        )
    }

    private static func captureReadyDevice(
        id: AudioDeviceID,
        uid: String,
        transportType: UInt32,
        inputChannelCount: Int,
        usbIdentity: IFDSPMacUSBIdentity?
    ) throws -> IFDSPMacAudioDevice {
        let format = try valueProperty(
            objectID: id,
            selector: kAudioDevicePropertyStreamFormat,
            scope: kAudioObjectPropertyScopeInput,
            initialValue: AudioStreamBasicDescription()
        )
        return IFDSPMacAudioDevice(
            audioDeviceID: id,
            uid: uid,
            name: bestEffortDeviceName(id: id),
            transportType: transportType,
            inputChannelCount: inputChannelCount,
            bufferFrameSize: try valueProperty(
                objectID: id,
                selector: kAudioDevicePropertyBufferFrameSize,
                scope: kAudioObjectPropertyScopeInput,
                initialValue: UInt32(0)
            ),
            sampleRate: format.mSampleRate,
            streamFormat: IFDSPMacPCMFormat(format),
            usbIdentity: usbIdentity,
            isAlive: try isAlive(deviceID: id)
        )
    }

    private static func bestEffortDeviceName(id: AudioDeviceID) -> String {
        (try? stringProperty(
            objectID: id,
            selector: kAudioObjectPropertyName
        )) ?? "CoreAudio device ID \(id)"
    }

    private static func isAlive(deviceID: AudioDeviceID) throws -> Bool {
        try valueProperty(
            objectID: deviceID,
            selector: kAudioDevicePropertyDeviceIsAlive,
            initialValue: UInt32(0)
        ) != 0
    }

    static func deviceStillMatches(_ expected: IFDSPMacAudioDevice) -> Bool {
        guard let current = try? device(id: expected.audioDeviceID) else { return false }
        return current.hasSameCaptureContract(as: expected)
    }

    private static func deviceIDs() throws -> [AudioDeviceID] {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDevices,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var byteCount: UInt32 = 0
        try requireNoError(
            AudioObjectGetPropertyDataSize(
                AudioObjectID(kAudioObjectSystemObject),
                &address,
                0,
                nil,
                &byteCount
            ),
            operation: "enumerating audio devices"
        )
        guard byteCount.isMultiple(of: UInt32(MemoryLayout<AudioDeviceID>.stride)) else {
            throw IFDSPMacAudioError.coreAudio(
                operation: "validating the audio-device list",
                status: kAudioHardwareUnspecifiedError
            )
        }
        guard byteCount > 0 else { return [] }
        var devices = [AudioDeviceID](
            repeating: kAudioObjectUnknown,
            count: Int(byteCount) / MemoryLayout<AudioDeviceID>.stride
        )
        try devices.withUnsafeMutableBytes { bytes in
            guard let baseAddress = bytes.baseAddress else {
                throw IFDSPMacAudioError.coreAudio(
                    operation: "allocating the audio-device list",
                    status: kAudioHardwareUnspecifiedError
                )
            }
            try requireNoError(
                AudioObjectGetPropertyData(
                    AudioObjectID(kAudioObjectSystemObject),
                    &address,
                    0,
                    nil,
                    &byteCount,
                    baseAddress
                ),
                operation: "reading the audio-device list"
            )
        }
        return devices.filter { $0 != kAudioObjectUnknown }
    }

    private static func inputChannelCount(deviceID: AudioDeviceID) throws -> Int {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyStreamConfiguration,
            mScope: kAudioObjectPropertyScopeInput,
            mElement: kAudioObjectPropertyElementMain
        )
        var byteCount: UInt32 = 0
        try requireNoError(
            AudioObjectGetPropertyDataSize(
                deviceID,
                &address,
                0,
                nil,
                &byteCount
            ),
            operation: "reading input stream configuration size"
        )
        let storage = UnsafeMutableRawPointer.allocate(
            byteCount: Int(byteCount),
            alignment: MemoryLayout<AudioBufferList>.alignment
        )
        defer { storage.deallocate() }
        try requireNoError(
            AudioObjectGetPropertyData(
                deviceID,
                &address,
                0,
                nil,
                &byteCount,
                storage
            ),
            operation: "reading input stream configuration"
        )
        let list = storage.bindMemory(to: AudioBufferList.self, capacity: 1)
        return UnsafeMutableAudioBufferListPointer(list).reduce(into: 0) {
            $0 += Int($1.mNumberChannels)
        }
    }

    private static func stringProperty(
        objectID: AudioObjectID,
        selector: AudioObjectPropertySelector
    ) throws -> String {
        var address = AudioObjectPropertyAddress(
            mSelector: selector,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var value: Unmanaged<CFString>?
        var byteCount = UInt32(MemoryLayout<CFString?>.size)
        try requireNoError(
            withUnsafeMutablePointer(to: &value) { pointer in
                AudioObjectGetPropertyData(
                    objectID,
                    &address,
                    0,
                    nil,
                    &byteCount,
                    pointer
                )
            },
            operation: "reading CoreAudio string property \(selector)"
        )
        guard let value else {
            throw IFDSPMacAudioError.coreAudio(
                operation: "reading CoreAudio string property \(selector)",
                status: kAudioHardwareUnspecifiedError
            )
        }
        return value.takeRetainedValue() as String
    }

    private static func valueProperty<Value>(
        objectID: AudioObjectID,
        selector: AudioObjectPropertySelector,
        scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal,
        initialValue: Value
    ) throws -> Value {
        var address = AudioObjectPropertyAddress(
            mSelector: selector,
            mScope: scope,
            mElement: kAudioObjectPropertyElementMain
        )
        var value = initialValue
        var byteCount = UInt32(MemoryLayout<Value>.size)
        try requireNoError(
            withUnsafeMutablePointer(to: &value) { pointer in
                AudioObjectGetPropertyData(
                    objectID,
                    &address,
                    0,
                    nil,
                    &byteCount,
                    pointer
                )
            },
            operation: "reading CoreAudio property \(selector)"
        )
        return value
    }

    private static func usbIdentity(
        audioDeviceUID: String,
        currentUSBDevices: [IFDSPMacUSBDeviceDescriptor]
    ) -> IFDSPMacUSBIdentity? {
        IFDSPAppleUSBAudioUIDResolver.identity(
            for: audioDeviceUID,
            among: currentUSBDevices
        )
    }

    private static func currentTHD75USBDevices() throws
        -> [IFDSPMacUSBDeviceDescriptor] {
        // Azimuth deploys on macOS 26+, whose USB host stack publishes the
        // physical composite device as IOUSBHostDevice. The serial transport
        // uses this same class and ancestor resolver for its retained proof.
        guard let matching = IOServiceMatching("IOUSBHostDevice") else {
            throw IFDSPMacAudioError.usbDeviceInventoryUnavailable(
                operation: "creating the USB-device query",
                status: nil
            )
        }
        var iterator: io_iterator_t = 0
        let status = IOServiceGetMatchingServices(
            kIOMainPortDefault,
            matching,
            &iterator
        )
        guard status == KERN_SUCCESS else {
            throw IFDSPMacAudioError.usbDeviceInventoryUnavailable(
                operation: "enumerating USB devices",
                status: status
            )
        }
        defer { IOObjectRelease(iterator) }

        var devicesByRegistryID: [UInt64: IFDSPMacUSBDeviceDescriptor] = [:]
        while case let entry = IOIteratorNext(iterator), entry != 0 {
            defer { IOObjectRelease(entry) }
            guard let registered = POSIXAzimuthUSBSerialLink
                    .thD75USBDeviceAncestor(startingAt: entry) else {
                continue
            }
            let identity = IFDSPMacUSBIdentity(
                vendorID: registered.identity.vendorID,
                productID: registered.identity.productID,
                registryEntryID: registered.usbDeviceRegistryEntryID
            )
            devicesByRegistryID[identity.registryEntryID] =
                IFDSPMacUSBDeviceDescriptor(
                    identity: identity,
                    serialNumber: registered.identity.serialNumber,
                    locationID: localNumberProperty(
                        entry: entry,
                        key: "locationID"
                    )?.uint32Value
                )
        }
        return devicesByRegistryID.values.sorted {
            $0.identity.registryEntryID < $1.identity.registryEntryID
        }
    }

    private static func localNumberProperty(
        entry: io_registry_entry_t,
        key: String
    ) -> NSNumber? {
        IORegistryEntryCreateCFProperty(
            entry,
            key as CFString,
            kCFAllocatorDefault,
            0
        )?.takeRetainedValue() as? NSNumber
    }

    static func requireNoError(_ status: OSStatus, operation: String) throws {
        guard status == noErr else {
            throw IFDSPMacAudioError.coreAudio(
                operation: operation,
                status: status
            )
        }
    }
}

struct IFDSPMacRawBufferDescriptor {
    var byteOffset: Int
    var byteCount: Int
    var channelCount: Int

    static let empty = Self(byteOffset: 0, byteCount: 0, channelCount: 0)
}

struct IFDSPMacRawAudioBlockView {
    let data: UnsafeRawPointer
    let buffers: UnsafePointer<IFDSPMacRawBufferDescriptor>
    let bufferCount: Int
    let frameCount: Int
}

private enum IFDSPMacRawCaptureFailure: UInt8 {
    case invalidBufferLayout = 1
    case callbackExceededDeviceBufferSize = 2

    var message: String {
        switch self {
        case .invalidBufferLayout:
            return "The verified TH-D75 audio device delivered an invalid PCM buffer layout. Capture stopped."
        case .callbackExceededDeviceBufferSize:
            return "The verified TH-D75 audio device exceeded its validated HAL buffer size. Capture stopped."
        }
    }
}

/// Fixed-storage single-producer/single-consumer handoff. The HAL callback
/// performs only validation, memcpy, and atomic publication; decoding and all
/// Swift async work happen on the consumer thread.
private final class IFDSPMacRawAudioRing: @unchecked Sendable {
    private enum Validation {
        case empty
        case invalid
        case valid(byteCount: Int, frameCount: Int, bufferCount: Int)
    }

    private static let usableSlotCount = 16
    private static let maximumSlotByteCount = 8 * 1_024 * 1_024

    private let slotCount: Int
    private let slotByteCapacity: Int
    private let bufferCapacity: Int
    private let audioStorage: UnsafeMutableRawPointer
    private let bufferStorage: UnsafeMutablePointer<IFDSPMacRawBufferDescriptor>
    private let bufferCounts: UnsafeMutablePointer<Int>
    private let frameCounts: UnsafeMutablePointer<Int>
    private let format: IFDSPMacPCMFormat
    private let maximumFrameCount: Int
    private let producerIndex = Atomic<Int>(0)
    private let consumerIndex = Atomic<Int>(0)
    private let droppedBlockCount = Atomic<Int>(0)
    private let droppedSampleCount = Atomic<Int>(0)
    private let terminalFailureCode = Atomic<UInt8>(0)

    init(device: IFDSPMacAudioDevice) throws {
        let bytesPerSample = Int(device.streamFormat.bytesPerSample)
        let channels = device.inputChannelCount
        let frames = Int(device.bufferFrameSize)
        let (bytesPerFrame, frameOverflow) = bytesPerSample.multipliedReportingOverflow(
            by: channels
        )
        let (slotByteCapacity, slotOverflow) = bytesPerFrame.multipliedReportingOverflow(
            by: frames
        )
        guard IFDSPMacPCMDecoder.supports(device.streamFormat),
              bytesPerSample > 0,
              channels > 0,
              frames > 0,
              !frameOverflow,
              !slotOverflow,
              slotByteCapacity > 0,
              slotByteCapacity <= Self.maximumSlotByteCount else {
            throw IFDSPMacAudioError.unsupportedInputFormat
        }

        slotCount = Self.usableSlotCount + 1
        self.slotByteCapacity = slotByteCapacity
        bufferCapacity = channels
        format = device.streamFormat
        maximumFrameCount = frames

        let (audioByteCount, audioOverflow) = self.slotCount
            .multipliedReportingOverflow(by: slotByteCapacity)
        let (descriptorCount, descriptorOverflow) = self.slotCount
            .multipliedReportingOverflow(by: channels)
        guard !audioOverflow, !descriptorOverflow else {
            throw IFDSPMacAudioError.unsupportedInputFormat
        }

        audioStorage = UnsafeMutableRawPointer.allocate(
            byteCount: audioByteCount,
            alignment: 64
        )
        bufferStorage = .allocate(capacity: descriptorCount)
        bufferStorage.initialize(repeating: .empty, count: descriptorCount)
        bufferCounts = .allocate(capacity: self.slotCount)
        bufferCounts.initialize(repeating: 0, count: self.slotCount)
        frameCounts = .allocate(capacity: self.slotCount)
        frameCounts.initialize(repeating: 0, count: self.slotCount)
    }

    deinit {
        let descriptorCount = slotCount * bufferCapacity
        bufferStorage.deinitialize(count: descriptorCount)
        bufferStorage.deallocate()
        bufferCounts.deinitialize(count: slotCount)
        bufferCounts.deallocate()
        frameCounts.deinitialize(count: slotCount)
        frameCounts.deallocate()
        audioStorage.deallocate()
    }

    /// Called only by the HAL IOProc.
    func push(_ inputData: UnsafePointer<AudioBufferList>) {
        guard terminalFailureCode.load(ordering: .relaxed) == 0 else { return }
        let buffers = UnsafeMutableAudioBufferListPointer(
            UnsafeMutablePointer(mutating: inputData)
        )
        switch validate(buffers) {
        case .empty:
            return
        case .invalid:
            recordTerminalFailure(.invalidBufferLayout)
        case .valid(let byteCount, let frameCount, let bufferCount):
            guard byteCount <= slotByteCapacity else {
                recordTerminalFailure(.callbackExceededDeviceBufferSize)
                return
            }

            let producer = producerIndex.load(ordering: .relaxed)
            let nextProducer = incremented(producer)
            guard nextProducer != consumerIndex.load(ordering: .acquiring) else {
                droppedBlockCount.wrappingAdd(1, ordering: .relaxed)
                droppedSampleCount.wrappingAdd(frameCount, ordering: .relaxed)
                return
            }

            let slotData = audioStorage.advanced(by: producer * slotByteCapacity)
            let slotBuffers = bufferStorage.advanced(by: producer * bufferCapacity)
            var byteOffset = 0
            for index in 0..<bufferCount {
                let source = buffers[index]
                let sourceByteCount = Int(source.mDataByteSize)
                guard let sourceData = source.mData else {
                    recordTerminalFailure(.invalidBufferLayout)
                    return
                }
                memcpy(
                    slotData.advanced(by: byteOffset),
                    sourceData,
                    sourceByteCount
                )
                slotBuffers.advanced(by: index).pointee = IFDSPMacRawBufferDescriptor(
                    byteOffset: byteOffset,
                    byteCount: sourceByteCount,
                    channelCount: Int(source.mNumberChannels)
                )
                byteOffset += sourceByteCount
            }
            bufferCounts.advanced(by: producer).pointee = bufferCount
            frameCounts.advanced(by: producer).pointee = frameCount
            producerIndex.store(nextProducer, ordering: .releasing)
        }
    }

    /// Called only by the dedicated consumer thread.
    func consumeNext(_ body: (IFDSPMacRawAudioBlockView) -> Void) -> Bool {
        let consumer = consumerIndex.load(ordering: .relaxed)
        guard consumer != producerIndex.load(ordering: .acquiring) else {
            return false
        }
        body(
            IFDSPMacRawAudioBlockView(
                data: UnsafeRawPointer(
                    audioStorage.advanced(by: consumer * slotByteCapacity)
                ),
                buffers: UnsafePointer(
                    bufferStorage.advanced(by: consumer * bufferCapacity)
                ),
                bufferCount: bufferCounts.advanced(by: consumer).pointee,
                frameCount: frameCounts.advanced(by: consumer).pointee
            )
        )
        consumerIndex.store(incremented(consumer), ordering: .releasing)
        return true
    }

    func takeDroppedCounts() -> (blocks: Int, samples: Int) {
        (
            droppedBlockCount.exchange(0, ordering: .acquiringAndReleasing),
            droppedSampleCount.exchange(0, ordering: .acquiringAndReleasing)
        )
    }

    var terminalFailure: IFDSPMacRawCaptureFailure? {
        IFDSPMacRawCaptureFailure(
            rawValue: terminalFailureCode.load(ordering: .acquiring)
        )
    }

    private func validate(
        _ buffers: UnsafeMutableAudioBufferListPointer
    ) -> Validation {
        let bufferCount = buffers.count
        guard bufferCount > 0, bufferCount <= bufferCapacity else {
            return .invalid
        }
        if !format.isNonInterleaved, bufferCount != 1 { return .invalid }

        var totalByteCount = 0
        var totalChannelCount = 0
        var validatedFrameCount: Int?
        for buffer in buffers {
            let byteCount = Int(buffer.mDataByteSize)
            let channelCount = Int(buffer.mNumberChannels)
            if byteCount == 0 { continue }
            guard buffer.mData != nil, channelCount > 0 else { return .invalid }
            let (bytesPerFrame, overflow) = Int(format.bytesPerSample)
                .multipliedReportingOverflow(by: channelCount)
            guard !overflow,
                  bytesPerFrame > 0,
                  byteCount.isMultiple(of: bytesPerFrame) else {
                return .invalid
            }
            let frameCount = byteCount / bytesPerFrame
            if let validatedFrameCount, validatedFrameCount != frameCount {
                return .invalid
            }
            validatedFrameCount = frameCount
            let (newByteCount, byteOverflow) = totalByteCount
                .addingReportingOverflow(byteCount)
            let (newChannelCount, channelOverflow) = totalChannelCount
                .addingReportingOverflow(channelCount)
            guard !byteOverflow, !channelOverflow else { return .invalid }
            totalByteCount = newByteCount
            totalChannelCount = newChannelCount
        }

        guard let frameCount = validatedFrameCount else { return .empty }
        guard frameCount > 0,
              frameCount <= maximumFrameCount,
              totalChannelCount == Int(format.channelsPerFrame) else {
            return .invalid
        }
        return .valid(
            byteCount: totalByteCount,
            frameCount: frameCount,
            bufferCount: bufferCount
        )
    }

    private func recordTerminalFailure(_ failure: IFDSPMacRawCaptureFailure) {
        _ = terminalFailureCode.compareExchange(
            expected: 0,
            desired: failure.rawValue,
            ordering: .acquiringAndReleasing
        )
    }

    private func incremented(_ index: Int) -> Int {
        let next = index + 1
        return next == slotCount ? 0 : next
    }
}

private final class IFDSPMacCaptureControl: @unchecked Sendable {
    let stopRequested = Atomic<Bool>(false)
    let cleanupStarted = Atomic<Bool>(false)
    let lossReported = Atomic<Bool>(false)
}

private final class IFDSPSystemMacAudioCaptureSession:
    IFDSPMacAudioCaptureSession,
    @unchecked Sendable
{
    private let device: IFDSPMacAudioDevice
    private let ring: IFDSPMacRawAudioRing
    private let control = IFDSPMacCaptureControl()
    private let receive: @Sendable (IFDSPSourcePCMBlock) -> Void
    private let overrun: @Sendable (_ blockCount: Int, _ sampleCount: Int) -> Void
    private let deviceLost: @Sendable (String) -> Void
    private let captureFailed: @Sendable (String) -> Void
    private let workerFinished = DispatchSemaphore(value: 0)
    private let listenerQueue = DispatchQueue(
        label: "org.swiftraccoon.azimuth.ifdsp-coreaudio-events"
    )
    private var ioProcID: AudioDeviceIOProcID?
    private var started = false
    private var workerStarted = false
    private var workerThread: Thread?
    private var aliveListener: AudioObjectPropertyListenerBlock?
    private var deviceListListener: AudioObjectPropertyListenerBlock?
    private var streamFormatListener: AudioObjectPropertyListenerBlock?
    private var bufferFrameSizeListener: AudioObjectPropertyListenerBlock?

    init(
        device: IFDSPMacAudioDevice,
        receive: @escaping @Sendable (IFDSPSourcePCMBlock) -> Void,
        overrun: @escaping @Sendable (_ blockCount: Int, _ sampleCount: Int) -> Void,
        deviceLost: @escaping @Sendable (String) -> Void,
        captureFailed: @escaping @Sendable (String) -> Void
    ) throws {
        self.device = device
        ring = try IFDSPMacRawAudioRing(device: device)
        self.receive = receive
        self.overrun = overrun
        self.deviceLost = deviceLost
        self.captureFailed = captureFailed
    }

    deinit {
        stop()
    }

    func start() throws {
        guard IFDSPMacAudioHardware.deviceStillMatches(device) else {
            throw IFDSPMacAudioError.deviceIdentityChanged
        }

        var createdIOProcID: AudioDeviceIOProcID?
        let ring = ring
        let control = control
        let status = AudioDeviceCreateIOProcIDWithBlock(
            &createdIOProcID,
            device.audioDeviceID,
            nil
        ) { _, inputData, _, _, _ in
            guard !control.stopRequested.load(ordering: .relaxed) else { return }
            ring.push(inputData)
        }
        try IFDSPMacAudioHardware.requireNoError(
            status,
            operation: "binding the verified TH-D75 input device"
        )
        guard let createdIOProcID else {
            throw IFDSPMacAudioError.coreAudio(
                operation: "binding the verified TH-D75 input device",
                status: kAudioHardwareUnspecifiedError
            )
        }
        ioProcID = createdIOProcID

        do {
            try installDeviceListeners()
            guard IFDSPMacAudioHardware.deviceStillMatches(device) else {
                throw IFDSPMacAudioError.deviceIdentityChanged
            }
            startWorker()
            try IFDSPMacAudioHardware.requireNoError(
                AudioDeviceStart(device.audioDeviceID, createdIOProcID),
                operation: "starting the verified TH-D75 input device"
            )
            started = true
        } catch {
            stop()
            throw error
        }
    }

    func stop() {
        guard !control.cleanupStarted.exchange(
            true,
            ordering: .acquiringAndReleasing
        ) else {
            return
        }
        control.stopRequested.store(true, ordering: .releasing)
        if started, let ioProcID {
            _ = AudioDeviceStop(device.audioDeviceID, ioProcID)
        }
        removeDeviceListeners()
        if let ioProcID {
            _ = AudioDeviceDestroyIOProcID(device.audioDeviceID, ioProcID)
            self.ioProcID = nil
        }
        started = false
        if workerStarted {
            workerFinished.wait()
            workerStarted = false
            workerThread = nil
        }
    }

    private func startWorker() {
        let ring = ring
        let control = control
        let receive = receive
        let overrun = overrun
        let captureFailed = captureFailed
        let workerFinished = workerFinished
        let format = device.streamFormat
        let sampleRate = device.sampleRate
        let worker = Thread {
            defer { workerFinished.signal() }
            Self.runWorker(
                ring: ring,
                control: control,
                format: format,
                sampleRate: sampleRate,
                receive: receive,
                overrun: overrun,
                captureFailed: captureFailed
            )
        }
        worker.name = "org.swiftraccoon.azimuth.ifdsp-coreaudio-consumer"
        worker.qualityOfService = .userInitiated
        workerThread = worker
        workerStarted = true
        worker.start()
    }

    private static func runWorker(
        ring: IFDSPMacRawAudioRing,
        control: IFDSPMacCaptureControl,
        format: IFDSPMacPCMFormat,
        sampleRate: Double,
        receive: @escaping @Sendable (IFDSPSourcePCMBlock) -> Void,
        overrun: @escaping @Sendable (_ blockCount: Int, _ sampleCount: Int) -> Void,
        captureFailed: @escaping @Sendable (String) -> Void
    ) {
        while !control.stopRequested.load(ordering: .acquiring) {
            if let failure = ring.terminalFailure {
                reportFailureOnce(
                    failure.message,
                    control: control,
                    captureFailed: captureFailed
                )
                return
            }

            var decodingFailed = false
            let consumed = ring.consumeNext { rawBlock in
                guard let samples = IFDSPMacPCMDecoder.decode(
                    rawBlock,
                    format: format
                ) else {
                    decodingFailed = true
                    return
                }
                guard !samples.isEmpty else { return }
                receive(
                    IFDSPSourcePCMBlock(
                        samples: samples,
                        sampleRate: sampleRate
                    )
                )
            }
            if decodingFailed {
                reportFailureOnce(
                    IFDSPMacAudioError.unsupportedInputFormat.localizedDescription,
                    control: control,
                    captureFailed: captureFailed
                )
                return
            }

            let dropped = ring.takeDroppedCounts()
            if dropped.blocks > 0 || dropped.samples > 0 {
                overrun(dropped.blocks, dropped.samples)
            }
            if !consumed { Thread.sleep(forTimeInterval: 0.001) }
        }
    }

    private static func reportFailureOnce(
        _ reason: String,
        control: IFDSPMacCaptureControl,
        captureFailed: @escaping @Sendable (String) -> Void
    ) {
        guard !control.lossReported.exchange(
            true,
            ordering: .acquiringAndReleasing
        ) else {
            return
        }
        control.stopRequested.store(true, ordering: .releasing)
        captureFailed(reason)
    }

    private func installDeviceListeners() throws {
        let alive: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
            self?.validateSelectedDevice()
        }
        let devices: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
            self?.validateSelectedDevice()
        }
        let streamFormat: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
            self?.validateSelectedDevice()
        }
        let bufferFrameSize: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
            self?.validateSelectedDevice()
        }
        var aliveAddress = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyDeviceIsAlive,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var devicesAddress = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDevices,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var streamFormatAddress = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyStreamFormat,
            mScope: kAudioObjectPropertyScopeInput,
            mElement: kAudioObjectPropertyElementMain
        )
        var bufferFrameSizeAddress = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyBufferFrameSize,
            mScope: kAudioObjectPropertyScopeInput,
            mElement: kAudioObjectPropertyElementMain
        )
        try IFDSPMacAudioHardware.requireNoError(
            AudioObjectAddPropertyListenerBlock(
                device.audioDeviceID,
                &aliveAddress,
                listenerQueue,
                alive
            ),
            operation: "observing the verified TH-D75 input"
        )
        aliveListener = alive
        do {
            try IFDSPMacAudioHardware.requireNoError(
                AudioObjectAddPropertyListenerBlock(
                    AudioObjectID(kAudioObjectSystemObject),
                    &devicesAddress,
                    listenerQueue,
                    devices
                ),
                operation: "observing CoreAudio device changes"
            )
            deviceListListener = devices
            try IFDSPMacAudioHardware.requireNoError(
                AudioObjectAddPropertyListenerBlock(
                    device.audioDeviceID,
                    &streamFormatAddress,
                    listenerQueue,
                    streamFormat
                ),
                operation: "observing the verified TH-D75 input format"
            )
            streamFormatListener = streamFormat
            try IFDSPMacAudioHardware.requireNoError(
                AudioObjectAddPropertyListenerBlock(
                    device.audioDeviceID,
                    &bufferFrameSizeAddress,
                    listenerQueue,
                    bufferFrameSize
                ),
                operation: "observing the verified TH-D75 input buffer size"
            )
            bufferFrameSizeListener = bufferFrameSize
        } catch {
            removeDeviceListeners()
            throw error
        }
    }

    private func removeDeviceListeners() {
        var aliveAddress = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyDeviceIsAlive,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var devicesAddress = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDevices,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var streamFormatAddress = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyStreamFormat,
            mScope: kAudioObjectPropertyScopeInput,
            mElement: kAudioObjectPropertyElementMain
        )
        var bufferFrameSizeAddress = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyBufferFrameSize,
            mScope: kAudioObjectPropertyScopeInput,
            mElement: kAudioObjectPropertyElementMain
        )
        if let aliveListener {
            _ = AudioObjectRemovePropertyListenerBlock(
                device.audioDeviceID,
                &aliveAddress,
                listenerQueue,
                aliveListener
            )
            self.aliveListener = nil
        }
        if let deviceListListener {
            _ = AudioObjectRemovePropertyListenerBlock(
                AudioObjectID(kAudioObjectSystemObject),
                &devicesAddress,
                listenerQueue,
                deviceListListener
            )
            self.deviceListListener = nil
        }
        if let streamFormatListener {
            _ = AudioObjectRemovePropertyListenerBlock(
                device.audioDeviceID,
                &streamFormatAddress,
                listenerQueue,
                streamFormatListener
            )
            self.streamFormatListener = nil
        }
        if let bufferFrameSizeListener {
            _ = AudioObjectRemovePropertyListenerBlock(
                device.audioDeviceID,
                &bufferFrameSizeAddress,
                listenerQueue,
                bufferFrameSizeListener
            )
            self.bufferFrameSizeListener = nil
        }
    }

    private func validateSelectedDevice() {
        guard !IFDSPMacAudioHardware.deviceStillMatches(device) else { return }
        guard !control.stopRequested.load(ordering: .acquiring),
              !control.lossReported.exchange(
                  true,
                  ordering: .acquiringAndReleasing
              ) else {
            return
        }
        control.stopRequested.store(true, ordering: .releasing)
        deviceLost(
            "The verified TH-D75 USB audio input disconnected or changed identity. "
                + "Azimuth stopped capture and did not select another input."
        )
    }
}

enum IFDSPMacPCMDecoder {
    static func supports(_ format: IFDSPMacPCMFormat) -> Bool {
        guard format.formatID == kAudioFormatLinearPCM,
              format.sampleRate.isFinite,
              format.sampleRate > 0,
              format.channelsPerFrame > 0,
              format.bytesPerFrame > 0,
              format.formatFlags & kAudioFormatFlagIsPacked != 0 else {
            return false
        }
        let flags = format.formatFlags
        let isFloat = flags & kAudioFormatFlagIsFloat != 0
        let isSignedInteger = flags & kAudioFormatFlagIsSignedInteger != 0
        let isBigEndian = flags & kAudioFormatFlagIsBigEndian != 0
        guard !isBigEndian else { return false }
        let supportedRepresentation =
            (isFloat && (format.bitsPerChannel == 32 || format.bitsPerChannel == 64))
            || (isSignedInteger
                && (format.bitsPerChannel == 16 || format.bitsPerChannel == 32))
        guard supportedRepresentation else { return false }

        let bytesPerSample = format.bitsPerChannel / 8
        let isNonInterleaved = flags & kAudioFormatFlagIsNonInterleaved != 0
        let expectedBytesPerFrame = isNonInterleaved
            ? bytesPerSample
            : bytesPerSample * format.channelsPerFrame
        return format.bytesPerFrame == expectedBytesPerFrame
    }

    static func decode(
        _ block: IFDSPMacRawAudioBlockView,
        format: IFDSPMacPCMFormat
    ) -> [Float]? {
        guard supports(format) else { return nil }
        return format.isNonInterleaved
            ? decodeNonInterleaved(block, format: format)
            : decodeInterleaved(block, format: format)
    }

    private static func decodeInterleaved(
        _ block: IFDSPMacRawAudioBlockView,
        format: IFDSPMacPCMFormat
    ) -> [Float]? {
        guard block.bufferCount == 1,
              block.frameCount > 0,
              format.bytesPerFrame > 0 else {
            return nil
        }
        let buffer = block.buffers.pointee
        let data = block.data.advanced(by: buffer.byteOffset)
        let channelCount = Int(format.channelsPerFrame)
        guard buffer.channelCount == channelCount,
              buffer.byteCount == block.frameCount * Int(format.bytesPerFrame),
              channelCount > 0 else {
            return nil
        }
        return downmix(
            data: data,
            frameCount: block.frameCount,
            channelCount: channelCount,
            sampleStride: channelCount,
            format: format
        )
    }

    private static func decodeNonInterleaved(
        _ block: IFDSPMacRawAudioBlockView,
        format: IFDSPMacPCMFormat
    ) -> [Float]? {
        let bytesPerSample = Int(format.bitsPerChannel / 8)
        guard bytesPerSample > 0,
              block.bufferCount > 0,
              block.frameCount > 0 else {
            return nil
        }

        var mono = [Float](repeating: 0, count: block.frameCount)
        var channelsMixed = 0
        for index in 0..<block.bufferCount {
            let buffer = block.buffers.advanced(by: index).pointee
            let channels = buffer.channelCount
            guard channels > 0,
                  buffer.byteCount == block.frameCount * bytesPerSample * channels else {
                return nil
            }
            guard let values = downmix(
                data: block.data.advanced(by: buffer.byteOffset),
                frameCount: block.frameCount,
                channelCount: channels,
                sampleStride: channels,
                format: format
            ) else {
                return nil
            }
            for frame in mono.indices { mono[frame] += values[frame] * Float(channels) }
            channelsMixed += channels
        }
        guard channelsMixed > 0 else { return nil }
        let scale = 1 / Float(channelsMixed)
        for frame in mono.indices { mono[frame] *= scale }
        return mono
    }

    private static func downmix(
        data: UnsafeRawPointer,
        frameCount: Int,
        channelCount: Int,
        sampleStride: Int,
        format: IFDSPMacPCMFormat
    ) -> [Float]? {
        var mono = [Float](repeating: 0, count: frameCount)
        let scale = 1 / Float(channelCount)
        let flags = format.formatFlags
        if flags & kAudioFormatFlagIsFloat != 0, format.bitsPerChannel == 32 {
            let samples = data.assumingMemoryBound(to: Float.self)
            for frame in 0..<frameCount {
                for channel in 0..<channelCount {
                    mono[frame] += samples[frame * sampleStride + channel] * scale
                }
            }
            return mono
        }
        if flags & kAudioFormatFlagIsFloat != 0, format.bitsPerChannel == 64 {
            let samples = data.assumingMemoryBound(to: Double.self)
            for frame in 0..<frameCount {
                for channel in 0..<channelCount {
                    mono[frame] += Float(samples[frame * sampleStride + channel]) * scale
                }
            }
            return mono
        }
        if flags & kAudioFormatFlagIsSignedInteger != 0,
           format.bitsPerChannel == 16 {
            let samples = data.assumingMemoryBound(to: Int16.self)
            let normalization = scale / 32_768
            for frame in 0..<frameCount {
                for channel in 0..<channelCount {
                    mono[frame] += Float(samples[frame * sampleStride + channel]) * normalization
                }
            }
            return mono
        }
        if flags & kAudioFormatFlagIsSignedInteger != 0,
           format.bitsPerChannel == 32 {
            let samples = data.assumingMemoryBound(to: Int32.self)
            let normalization = scale / 2_147_483_648
            for frame in 0..<frameCount {
                for channel in 0..<channelCount {
                    mono[frame] += Float(samples[frame * sampleStride + channel]) * normalization
                }
            }
            return mono
        }
        return nil
    }
}

private extension String {
    var nilIfEmpty: String? { isEmpty ? nil : self }
}

#endif
