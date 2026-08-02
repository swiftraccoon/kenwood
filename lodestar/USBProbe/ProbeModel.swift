import AVFAudio
import ExternalAccessory
import Foundation
import Observation

/// One audio channel as reported by the session.
struct ChannelInfo: Identifiable {
    let id: Int
    let name: String
}

/// A snapshot of one `AVAudioSessionPortDescription`.
struct PortInfo: Identifiable {
    let id: String
    let name: String
    let type: String
    let isUSB: Bool
    let channels: [ChannelInfo]
    let dataSources: [String]

    init(port: AVAudioSessionPortDescription) {
        id = port.uid
        name = port.portName
        type = port.portType.rawValue
        isUSB = port.portType == .usbAudio
        channels = (port.channels ?? []).map {
            ChannelInfo(id: Int($0.channelNumber), name: $0.channelName)
        }
        dataSources = (port.dataSources ?? []).map(\.dataSourceName)
    }
}

/// Session-level numbers read after activation.
struct SessionInfo {
    let sampleRate: Double
    let inputChannels: Int
    let maxInputChannels: Int
    let inputLatencyMs: Double
    let ioBufferMs: Double
    let appFormat: String
    /// Whether iOS will let us WRITE input gain. A UAC feature-unit gain
    /// write is a host -> device control transfer, so a `true` here would
    /// mean the audio interface is not strictly one-way after all.
    let inputGainSettable: Bool
    let inputGain: Float
}

/// One `EAAccessory` (expected: none; the TH-D75 has no MFi chip).
struct AccessoryInfo: Identifiable {
    let id: Int
    let name: String
    let manufacturer: String
    let modelNumber: String
    let protocols: [String]
}

/// Owns the audio-session lifecycle and exposes read-only snapshots of
/// everything iOS reports about attached audio hardware.
///
/// No `AVAudioEngine` anywhere: a dormant engine's `inputNode` registers
/// hardware listeners on an internal queue and its dealloc races
/// configuration changes (crashed on device, reproduced on simulator, and
/// its reported format doesn't reliably track route changes anyway). The
/// activated session's `sampleRate`/`inputNumberOfChannels` are Apple's
/// sanctioned hardware readout.
@MainActor
@Observable
final class ProbeModel {
    private(set) var permission = "not requested"
    private(set) var activationError: String?
    private(set) var availableInputs: [PortInfo] = []
    private(set) var routeInputs: [PortInfo] = []
    private(set) var routeOutputs: [PortInfo] = []
    private(set) var sessionInfo: SessionInfo?
    private(set) var preferredInputResult = "session not started"
    private(set) var accessories: [AccessoryInfo] = []
    private(set) var lastChange = "none yet"
    private(set) var controlProbe: [String] = []
    let dataChannels = DataChannelProbe()

    /// Owns notification tokens and unregisters them on deallocation.
    /// A separate non-isolated type because Swift 5.10 deinits can't
    /// touch main-actor state; `removeObserver` is thread-safe.
    private final class ObserverBag {
        var tokens: [NSObjectProtocol] = []

        deinit {
            for token in tokens {
                NotificationCenter.default.removeObserver(token)
            }
        }
    }

    private let observerBag = ObserverBag()
    private var started = false
    private var lastPreferredUid: String?

    func start() async {
        guard !started else { return }
        started = true
        let granted = await AVAudioApplication.requestRecordPermission()
        permission = granted ? "granted" : "denied"
        configureSession()
        EAAccessoryManager.shared().registerForLocalNotifications()
        observeNotifications()
        refreshAndRoute()
        // Raw-USB control attempts run once; the sandbox verdict doesn't
        // change while the app lives, and each attempt is logged to stdout.
        controlProbe = ControlProbe.run()
        print(controlProbe.joined(separator: "\n"))
        dataChannels.start()
    }

    /// Category + activation, shared by launch, the retry path, and the
    /// media-services-reset / interruption-ended handlers (after a reset
    /// the session silently reverts to defaults and must be reconfigured).
    private func configureSession() {
        let session = AVAudioSession.sharedInstance()
        do {
            try session.setCategory(.playAndRecord)
            try session.setActive(true)
            activationError = nil
        } catch {
            activationError = String(describing: error)
        }
    }

    /// Routing policy + snapshot. Only called where routing is wanted
    /// (launch, manual Refresh, a newly attached device); never from
    /// passive notifications, so a route change this method itself induces
    /// can't re-trigger policy.
    func refreshAndRoute() {
        if activationError != nil {
            configureSession()
        }
        requestExternalRouting()
        refresh()
    }

    /// Read-only snapshot of everything; never mutates routing.
    func refresh() {
        let session = AVAudioSession.sharedInstance()
        let inputs = session.availableInputs ?? []
        availableInputs = inputs.map(PortInfo.init(port:))
        routeInputs = session.currentRoute.inputs.map(PortInfo.init(port:))
        routeOutputs = session.currentRoute.outputs.map(PortInfo.init(port:))
        if let external = Self.externalCandidate(in: inputs) {
            if session.currentRoute.inputs.contains(where: { $0.uid == external.uid }) {
                preferredInputResult = "external input \"\(external.portName)\" active"
            }
            // Otherwise keep the last policy outcome (e.g. a failure string).
        } else {
            preferredInputResult = "no external input present"
        }
        sessionInfo = SessionInfo(
            sampleRate: session.sampleRate,
            inputChannels: session.inputNumberOfChannels,
            maxInputChannels: session.maximumInputNumberOfChannels,
            inputLatencyMs: session.inputLatency * 1000,
            ioBufferMs: session.ioBufferDuration * 1000,
            appFormat: Self.appFormat(for: session),
            inputGainSettable: session.isInputGainSettable,
            inputGain: session.inputGain
        )
        accessories = EAAccessoryManager.shared().connectedAccessories.map {
            AccessoryInfo(
                id: $0.connectionID,
                name: $0.name,
                manufacturer: $0.manufacturer,
                modelNumber: $0.modelNumber,
                protocols: $0.protocolStrings
            )
        }
        dumpToConsole()
    }

    /// Ask iOS to route from the most interesting external input. USB
    /// audio first, but unusual devices can surface with port types other
    /// than `.usbAudio` (the framework itself models `USBInput`, `IDAM`,
    /// …), so any non-built-in input qualifies; the raw type still shows
    /// in the lists either way.
    private func requestExternalRouting() {
        let session = AVAudioSession.sharedInstance()
        // An inactive session ignores preferences; retrying there would
        // re-issue forever.
        guard activationError == nil else { return }
        guard let external = Self.externalCandidate(in: session.availableInputs ?? []) else {
            lastPreferredUid = nil
            return
        }
        if session.currentRoute.inputs.contains(where: { $0.uid == external.uid }) {
            return
        }
        guard external.uid != lastPreferredUid else {
            return
        }
        lastPreferredUid = external.uid
        do {
            try session.setPreferredInput(external)
            preferredInputResult = "setPreferredInput(\"\(external.portName)\") ok"
        } catch {
            preferredInputResult = "setPreferredInput failed: \(error.localizedDescription)"
        }
    }

    private static func externalCandidate(
        in inputs: [AVAudioSessionPortDescription]
    ) -> AVAudioSessionPortDescription? {
        inputs.first { $0.portType == .usbAudio }
            ?? inputs.first { $0.portType != .builtInMic }
    }

    private func observeNotifications() {
        let center = NotificationCenter.default
        // queue: .main gives FIFO delivery, and the main queue is the main
        // actor's executor, so assumeIsolated is sound; unstructured Task
        // hops can reorder a plug/unplug burst.
        observerBag.tokens.append(center.addObserver(
            forName: AVAudioSession.routeChangeNotification, object: nil, queue: .main
        ) { [weak self] note in
            let raw = note.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt
            let reason = raw.flatMap(AVAudioSession.RouteChangeReason.init(rawValue:)) ?? .unknown
            MainActor.assumeIsolated {
                guard let self else { return }
                self.noteEvent(Self.describe(reason))
                if reason == .newDeviceAvailable {
                    self.refreshAndRoute()
                } else {
                    self.refresh()
                }
            }
        })
        observerBag.tokens.append(center.addObserver(
            forName: AVAudioSession.mediaServicesWereResetNotification, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.noteEvent("media services reset")
                self.configureSession()
                self.refreshAndRoute()
            }
        })
        observerBag.tokens.append(center.addObserver(
            forName: AVAudioSession.interruptionNotification, object: nil, queue: .main
        ) { [weak self] note in
            let raw = note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt
            let type = raw.flatMap(AVAudioSession.InterruptionType.init(rawValue:))
            MainActor.assumeIsolated {
                guard let self else { return }
                switch type {
                case .began:
                    self.noteEvent("interruption began")
                    self.refresh()
                case .ended:
                    self.noteEvent("interruption ended")
                    self.configureSession()
                    self.refreshAndRoute()
                default:
                    self.noteEvent("interruption (unknown phase)")
                }
            }
        })
        observerBag.tokens.append(center.addObserver(
            forName: .EAAccessoryDidConnect, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.noteEvent("MFi accessory connected")
                self.refresh()
            }
        })
        observerBag.tokens.append(center.addObserver(
            forName: .EAAccessoryDidDisconnect, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.noteEvent("MFi accessory disconnected")
                self.refresh()
            }
        })
    }

    private func noteEvent(_ what: String) {
        lastChange = "\(what) at \(Date.now.formatted(date: .omitted, time: .standard))"
    }

    private static func describe(_ reason: AVAudioSession.RouteChangeReason) -> String {
        switch reason {
        case .newDeviceAvailable: "new device available"
        case .oldDeviceUnavailable: "old device unavailable"
        case .categoryChange: "category change"
        case .override: "override"
        case .wakeFromSleep: "wake from sleep"
        case .noSuitableRouteForCategory: "no suitable route"
        case .routeConfigurationChange: "route configuration change"
        case .unknown: "unknown"
        @unknown default: "unrecognized reason"
        }
    }

    /// The `AVAudioFormat` an app would render with, derived from the
    /// activated session (float32 deinterleaved is the standard-format
    /// contract); no engine required.
    private static func appFormat(for session: AVAudioSession) -> String {
        let channelCount = session.inputNumberOfChannels
        guard session.sampleRate > 0, channelCount > 0,
              let format = AVAudioFormat(
                  standardFormatWithSampleRate: session.sampleRate,
                  channels: AVAudioChannelCount(channelCount)
              )
        else {
            return "n/a (no active input)"
        }
        return "\(format.channelCount) ch @ \(Self.hertz(format.sampleRate)) float32 deinterleaved (derived)"
    }

    /// `Int(_: Double)` traps on NaN/infinity; never worth a crash in a
    /// diagnostic readout.
    private static func hertz(_ rate: Double) -> String {
        rate.isFinite ? "\(Int(rate)) Hz" : "\(rate) Hz"
    }

    /// The full snapshot also goes to stdout on every refresh so an
    /// attached Xcode console captures everything; reading results off
    /// the phone screen and retyping them is not a workflow.
    private func dumpToConsole() {
        var lines = ["=== USBProbe snapshot ==="]
        lines.append("permission: \(permission)")
        if let activationError {
            lines.append("activation error: \(activationError)")
        }
        if let info = sessionInfo {
            lines.append(
                "session: \(Self.hertz(info.sampleRate)), in \(info.inputChannels) ch (max \(info.maxInputChannels)), "
                    + String(format: "latency %.1f ms, buffer %.1f ms", info.inputLatencyMs, info.ioBufferMs)
            )
            lines.append("derived app format: \(info.appFormat)")
            lines.append(
                "input gain: \(info.inputGain) settable=\(info.inputGainSettable) "
                    + "(settable would mean a host->device UAC control path exists)"
            )
        }
        lines.append("preferred input: \(preferredInputResult)")
        lines.append("available inputs (\(availableInputs.count)):")
        for port in availableInputs {
            lines.append(contentsOf: Self.portLines(port))
        }
        lines.append("route inputs (\(routeInputs.count)):")
        for port in routeInputs {
            lines.append(contentsOf: Self.portLines(port))
        }
        lines.append("route outputs (\(routeOutputs.count)):")
        for port in routeOutputs {
            lines.append(contentsOf: Self.portLines(port))
        }
        lines.append("MFi accessories (\(accessories.count)):")
        for accessory in accessories {
            lines.append(
                "  \(accessory.name) – \(accessory.manufacturer) \(accessory.modelNumber) "
                    + "protocols: \(accessory.protocols.joined(separator: ","))"
            )
        }
        lines.append("last change: \(lastChange)")
        lines.append("=== end snapshot ===")
        print(lines.joined(separator: "\n"))
    }

    private static func portLines(_ port: PortInfo) -> [String] {
        var lines = ["  \(port.name) [\(port.type)]\(port.isUSB ? " USB" : "") uid=\(port.id)"]
        if !port.channels.isEmpty {
            lines.append("    channels: \(port.channels.map(\.name).joined(separator: ", "))")
        }
        if !port.dataSources.isEmpty {
            lines.append("    dataSources: \(port.dataSources.joined(separator: ", "))")
        }
        return lines
    }
}
