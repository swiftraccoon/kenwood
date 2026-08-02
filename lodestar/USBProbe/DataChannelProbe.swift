import CoreMIDI
import Foundation
import ImageCaptureCore

/// Probes the two PUBLIC iOS frameworks that can carry bidirectional bytes
/// over USB without MFi or DriverKit. Both are ordinary App-Store APIs; the
/// question this answers empirically is whether they function on iPhone and
/// what they see with hardware attached.
///
///  * `CoreMIDI`: class-compliant USB MIDI devices. SysEx messages carry
///    arbitrary payloads, so a USB-MIDI-class bridge is a legal data pipe.
///  * `ImageCaptureCore`: `ICDeviceBrowser` finds USB PTP devices
///    (`ICTransportTypeUSB` is `ios(13.0)`), and `ICCameraDevice`'s
///    `requestSendPTPCommand:outData:…` (`ios(15.2)`) sends arbitrary PTP
///    commands with in/out data phases. PTP reserves opcodes 0x9000-0x9FFF
///    for vendors, so a PTP-class bridge is also a legal data pipe.
///
/// The TH-D75 speaks neither protocol, so nothing here talks to the radio
/// today. The point is to establish whether these channels are real on this
/// hardware, because they define what a bridge could present to an iPhone.
@MainActor
@Observable
final class DataChannelProbe: NSObject {
    private(set) var lines: [String] = []

    private var browser: ICDeviceBrowser?

    /// Runs the MIDI probe immediately and starts the (asynchronous) Image
    /// Capture browser; `lines` grows as devices arrive.
    func start() {
        var out = ["=== bidirectional data-channel probe ==="]
        out += Self.probeCoreMIDI()
        out += probeImageCapture()
        lines = out
        emit(out)
    }

    // MARK: CoreMIDI

    private static func probeCoreMIDI() -> [String] {
        var out = ["[CoreMIDI] USB MIDI class (SysEx = arbitrary payload):"]
        var client = MIDIClientRef()
        let status = MIDIClientCreate("USBProbe" as CFString, nil, nil, &client)
        out.append("  MIDIClientCreate status=\(status) \(status == noErr ? "(ok)" : "(FAILED)")")
        let devices = MIDIGetNumberOfDevices()
        let sources = MIDIGetNumberOfSources()
        let destinations = MIDIGetNumberOfDestinations()
        out.append("  devices=\(devices) sources=\(sources) destinations=\(destinations)")
        for index in 0..<devices {
            let device = MIDIGetDevice(index)
            out.append("  device[\(index)]: \(name(of: device))")
        }
        for index in 0..<destinations {
            let endpoint = MIDIGetDestination(index)
            // A destination is a host -> device path: this is what makes
            // MIDI a *bidirectional* option, unlike USB audio capture.
            out.append("  destination[\(index)] (host->device): \(name(of: endpoint))")
        }
        if client != 0 {
            MIDIClientDispose(client)
        }
        return out
    }

    private static func name(of object: MIDIObjectRef) -> String {
        var cfName: Unmanaged<CFString>?
        let status = MIDIObjectGetStringProperty(object, kMIDIPropertyDisplayName, &cfName)
        guard status == noErr, let value = cfName?.takeRetainedValue() else {
            return "<unnamed status=\(status)>"
        }
        return value as String
    }

    // MARK: ImageCaptureCore / PTP

    private func probeImageCapture() -> [String] {
        var out = ["[ImageCaptureCore] USB PTP pass-through (vendor opcodes 0x9000-0x9FFF):"]
        let browser = ICDeviceBrowser()
        browser.delegate = self
        // ICTransportTypeUSB is ios(13.0); mask to USB-attached devices only.
        browser.browsedDeviceTypeMask = ICDeviceTypeMask(
            rawValue: ICDeviceTypeMask.camera.rawValue
                | ICDeviceLocationTypeMask.local.rawValue
        ) ?? .camera
        browser.start()
        self.browser = browser
        out.append("  ICDeviceBrowser started (delegate set, mask=camera|local)")
        out.append("  devices appear below as they enumerate; none = no PTP device attached")
        return out
    }

    private func append(_ line: String) {
        lines.append(line)
        emit([line])
    }

    private func emit(_ newLines: [String]) {
        print(newLines.joined(separator: "\n"))
    }
}

extension DataChannelProbe: ICDeviceBrowserDelegate {
    nonisolated func deviceBrowser(
        _ browser: ICDeviceBrowser,
        didAdd device: ICDevice,
        moreComing: Bool
    ) {
        let name = device.name ?? "<unnamed>"
        let transport = device.transportType ?? "<none>"
        let capabilities = (device as? ICCameraDevice)?.capabilities ?? []
        let acceptsPTP = capabilities.contains(
            ICDeviceCapability.cameraDeviceCanAcceptPTPCommands.rawValue
        )
        Task { @MainActor [weak self] in
            self?.append(
                "  + \(name) transport=\(transport) PTP-passthrough=\(acceptsPTP ? "YES" : "no")"
            )
        }
    }

    nonisolated func deviceBrowser(
        _ browser: ICDeviceBrowser,
        didRemove device: ICDevice,
        moreGoing: Bool
    ) {
        let name = device.name ?? "<unnamed>"
        Task { @MainActor [weak self] in
            self?.append("  - \(name) removed")
        }
    }
}
