// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

/// Resume a staged startup only if the owning session survived its suspended
/// preparation step. Keeping the check and side effect in one helper prevents a
/// caller from accidentally activating a system resource before noticing that
/// Stop/background invalidated the session.
@MainActor
func resumeIFDSPStartupIfCurrent<Prepared>(
    after suspendedPreparation: () async throws -> Void,
    isCurrent: () -> Bool,
    prepare: () throws -> Prepared
) async rethrows -> Prepared? {
    try await suspendedPreparation()
    guard isCurrent() else { return nil }
    return try prepare()
}

#if os(iOS)
@preconcurrency import AVFAudio

/// Captures the TH-D75 USB Audio Class input and feeds physical PCM to Rust.
/// The DriverKit extension owns only the two CDC interfaces, so Apple's audio
/// driver can expose this interface while CAT control remains connected.
@MainActor
final class IFDSPAudioStreamService: IFDSPLiveStreaming {
    private(set) var currentState: IFDSPLiveStreamState {
        didSet { stateContinuation.yield(currentState) }
    }

    private(set) var configuration: IFDSPConfiguration

    let monitoringState: IFDSPMonitoringState = .unavailable(
        reason: "Demodulated playback stays off until Azimuth can verify an output route that is not the radio's paired USB audio output."
    )

    let updates: AsyncStream<IFDSPLiveStreamState>

    private let stateContinuation: AsyncStream<IFDSPLiveStreamState>.Continuation
    private let processor: IfDspProcessor?
    private let processorStartupError: String?
    private var audioEngine: AVAudioEngine?
    private var inputTapInstalled = false
    private var audioSessionIsActive = false
    private var pcmContinuation: AsyncStream<IFDSPSourcePCMBlock>.Continuation?
    private var workerTask: Task<Void, Never>?
    private var routeObserver: NSObjectProtocol?
    private var mediaResetObserver: NSObjectProtocol?
    private var activeSessionID: UUID?
    private var selectedInputUID: String?

    init(configuration: IFDSPConfiguration = .standard) {
        self.configuration = configuration
        let (updates, stateContinuation) = AsyncStream<IFDSPLiveStreamState>.makeStream(
            bufferingPolicy: .bufferingNewest(1)
        )
        self.updates = updates
        self.stateContinuation = stateContinuation

        do {
            processor = try IfDspProcessor(configuration: configuration.coreValue)
            processorStartupError = nil
            currentState = .idle
        } catch {
            processor = nil
            processorStartupError = error.localizedDescription
            currentState = .failed(
                message: "The IF DSP engine could not start: \(error.localizedDescription)",
                lastFrame: nil
            )
        }
        stateContinuation.yield(currentState)
    }

    func start() async {
        guard !currentState.isStreaming, activeSessionID == nil else { return }
        guard let processor else {
            currentState = .failed(
                message: "The IF DSP engine is unavailable: \(processorStartupError ?? "unknown error")",
                lastFrame: currentState.latestFrame
            )
            return
        }

        let sessionID = UUID()
        activeSessionID = sessionID
        currentState = .requestingPermission

        let permissionGranted = await audioPermissionIsGranted()
        guard activeSessionID == sessionID else { return }
        guard permissionGranted else {
            activeSessionID = nil
            currentState = .failed(
                message: "Audio-input permission is off. Allow Azimuth in Settings to capture the radio's USB IF stream.",
                lastFrame: currentState.latestFrame
            )
            return
        }

        do {
            guard let selectedInput = try await resumeIFDSPStartupIfCurrent(
                after: {
                    try await Task.detached { try processor.reset() }.value
                },
                isCurrent: { self.activeSessionID == sessionID },
                prepare: { try self.prepareAudioSession() }
            ) else {
                return
            }
            try await verifySelectedRoute(selectedInput)
            guard activeSessionID == sessionID else { return }
            currentState = .starting(routeName: selectedInput.portName)
            try startCapture(
                from: selectedInput,
                processor: processor,
                sessionID: sessionID
            )
        } catch let error as IFDSPAudioStreamError {
            guard activeSessionID == sessionID else { return }
            finishCapture(publishIdle: false)
            switch error {
            case .noUSBAudioInput(let availableInputs):
                currentState = .waitingForUSBAudio(availableInputs: availableInputs)
            default:
                currentState = .failed(
                    message: error.localizedDescription,
                    lastFrame: currentState.latestFrame
                )
            }
        } catch {
            guard activeSessionID == sessionID else { return }
            finishCapture(publishIdle: false)
            currentState = .failed(
                message: "Live IF capture failed: \(error.localizedDescription)",
                lastFrame: currentState.latestFrame
            )
        }
    }

    func stop() {
        finishCapture(publishIdle: true)
    }

    func setConfiguration(_ configuration: IFDSPConfiguration) async {
        guard let processor else {
            currentState = .failed(
                message: "The IF DSP engine is unavailable: \(processorStartupError ?? "unknown error")",
                lastFrame: currentState.latestFrame
            )
            return
        }

        do {
            try await Task.detached {
                try processor.setConfiguration(configuration: configuration.coreValue)
            }.value
            self.configuration = configuration
        } catch {
            currentState = .failed(
                message: "The IF DSP configuration was rejected: \(error.localizedDescription)",
                lastFrame: currentState.latestFrame
            )
        }
    }

    private func audioPermissionIsGranted() async -> Bool {
        switch AVAudioApplication.shared.recordPermission {
        case .granted:
            return true
        case .denied:
            return false
        case .undetermined:
            return await withCheckedContinuation { continuation in
                AVAudioApplication.requestRecordPermission { granted in
                    continuation.resume(returning: granted)
                }
            }
        @unknown default:
            return false
        }
    }

    private func prepareAudioSession() throws -> AVAudioSessionPortDescription {
        let session = AVAudioSession.sharedInstance()
        try session.setCategory(.record, mode: .measurement)
        try session.setPreferredSampleRate(48_000)
        try session.setPreferredIOBufferDuration(0.02)
        try session.setActive(true)
        audioSessionIsActive = true

        let availableInputs = session.availableInputs ?? []
        let usbInputs = availableInputs.filter { $0.portType == .usbAudio }
        guard let selected = Self.selectRadioInput(from: usbInputs) else {
            throw IFDSPAudioStreamError.noUSBAudioInput(
                availableInputs: availableInputs.map(\.portName).sorted()
            )
        }
        try session.setPreferredInput(selected)
        selectedInputUID = selected.uid
        return selected
    }

    private func verifySelectedRoute(
        _ selectedInput: AVAudioSessionPortDescription
    ) async throws {
        let session = AVAudioSession.sharedInstance()
        for attempt in 0..<10 {
            if session.currentRoute.inputs.contains(where: { $0.uid == selectedInput.uid }) {
                return
            }
            if attempt < 9 {
                try await Task.sleep(for: .milliseconds(50))
            }
        }
        throw IFDSPAudioStreamError.routeSelectionFailed(
            selected: selectedInput.portName,
            currentInputs: session.currentRoute.inputs.map(\.portName)
        )
    }

    nonisolated static func selectRadioInput(
        from usbInputs: [AVAudioSessionPortDescription]
    ) -> AVAudioSessionPortDescription? {
        let knownRadioInput = usbInputs.first { input in
            let normalized = input.portName.lowercased()
            return normalized.contains("adc stream in")
                || normalized.contains("th-d75")
                || normalized.contains("thd75")
                || normalized.contains("kenwood")
        }
        if let knownRadioInput { return knownRadioInput }
        // One USB input connected alongside the same TH-D75 CDC device is an
        // unambiguous operator choice. Multiple unnamed USB inputs are not.
        return usbInputs.count == 1 ? usbInputs.first : nil
    }

    private func startCapture(
        from selectedInput: AVAudioSessionPortDescription,
        processor: IfDspProcessor,
        sessionID: UUID
    ) throws {
        let engine = AVAudioEngine()
        audioEngine = engine
        let inputNode = engine.inputNode
        let sourceFormat = inputNode.outputFormat(forBus: 0)
        guard sourceFormat.sampleRate > 0, sourceFormat.channelCount > 0 else {
            throw IFDSPAudioStreamError.invalidInputFormat
        }
        guard let tapFormat = AVAudioFormat(
            commonFormat: .pcmFormatFloat32,
            sampleRate: sourceFormat.sampleRate,
            channels: sourceFormat.channelCount,
            interleaved: false
        ) else {
            throw IFDSPAudioStreamError.invalidInputFormat
        }

        let route = IFDSPInputRoute(
            name: selectedInput.portName,
            kind: .usbAudio,
            sourceSampleRate: tapFormat.sampleRate,
            sourceChannelCount: Int(tapFormat.channelCount)
        )
        let captureCounter = IFDSPCaptureCounter()
        let frameMailbox = IFDSPFrameMailbox()
        let (pcmStream, continuation) = AsyncStream<IFDSPSourcePCMBlock>.makeStream(
            bufferingPolicy: .bufferingNewest(8)
        )
        pcmContinuation = continuation
        workerTask = makeWorker(
            stream: pcmStream,
            processor: processor,
            route: route,
            captureCounter: captureCounter,
            frameMailbox: frameMailbox,
            sessionID: sessionID
        )

        inputNode.installTap(
            onBus: 0,
            bufferSize: 4_800,
            format: tapFormat
        ) { buffer, _ in
            guard let block = IFDSPSourcePCMBlock(buffer: buffer) else {
                continuation.finish()
                return
            }
            captureCounter.recordSourceBlock(sampleCount: block.samples.count)
            switch continuation.yield(block) {
            case .enqueued:
                break
            case .dropped(let droppedBlock):
                captureCounter.recordDroppedBlock(sampleCount: droppedBlock.samples.count)
            case .terminated:
                break
            @unknown default:
                break
            }
        }
        inputTapInstalled = true

        engine.prepare()
        do {
            try engine.start()
        } catch {
            inputNode.removeTap(onBus: 0)
            inputTapInstalled = false
            throw error
        }

        installAudioObservers(sessionID: sessionID)
        currentState = .streaming(route: route, frame: nil)
    }

    private func makeWorker(
        stream: AsyncStream<IFDSPSourcePCMBlock>,
        processor: IfDspProcessor,
        route: IFDSPInputRoute,
        captureCounter: IFDSPCaptureCounter,
        frameMailbox: IFDSPFrameMailbox,
        sessionID: UUID
    ) -> Task<Void, Never> {
        Task.detached(priority: .userInitiated) { [weak self] in
            var rateConverter: IFDSPPCMRateConverter?
            var latestSpectrum: IFDSPSpectrum?
            var batcher = IFDSPPCMBlockBatcher(targetSampleCount: 4_800)
            do {
                for await sourceBlock in stream {
                    guard !Task.isCancelled else { return }
                    let samples = try IFDSPPCMRateConverter.convert(
                        sourceBlock,
                        cachedConverter: &rateConverter
                    )
                    guard !samples.isEmpty else { continue }
                    batcher.append(samples)
                    while let batch = batcher.nextBatch() {
                        guard !Task.isCancelled else { return }
                        let coreFrame = try processor.processPcm(samples: batch)
                        if let spectrum = coreFrame.spectrum {
                            latestSpectrum = spectrum.domainValue
                        }
                        let capture = captureCounter.snapshot
                        let frame = IFDSPLiveFrame(
                            sequence: coreFrame.sequence,
                            inputSampleCount: coreFrame.inputSampleCount,
                            inputLevelDBFS: Double(coreFrame.inputLevelDbfs),
                            outputLevelDBFS: Double(coreFrame.outputLevelDbfs),
                            spectrum: latestSpectrum,
                            clippedSampleCount: coreFrame.clippedSampleCount,
                            sourceBlockCount: capture.sourceBlockCount,
                            sourceSampleCount: capture.sourceSampleCount,
                            droppedBlockCount: capture.droppedBlockCount,
                            droppedSampleCount: capture.droppedSampleCount,
                            capturedAt: Date()
                        )
                        guard frameMailbox.offer(frame) else { continue }
                        Task { @MainActor [weak self] in
                            guard let frame = frameMailbox.takeLatest(),
                                  self?.activeSessionID == sessionID else { return }
                            self?.currentState = .streaming(route: route, frame: frame)
                        }
                    }
                }
                guard !Task.isCancelled else { return }
                Task { @MainActor [weak self] in
                    guard self?.activeSessionID == sessionID else { return }
                    let lastFrame = self?.currentState.latestFrame
                    self?.finishCapture(publishIdle: false)
                    self?.currentState = .paused(
                        reason: "The USB audio input stopped delivering PCM.",
                        lastFrame: lastFrame
                    )
                }
            } catch {
                Task { @MainActor [weak self] in
                    guard self?.activeSessionID == sessionID else { return }
                    let lastFrame = self?.currentState.latestFrame
                    self?.finishCapture(publishIdle: false)
                    self?.currentState = .failed(
                        message: "IF DSP processing stopped: \(error.localizedDescription)",
                        lastFrame: lastFrame
                    )
                }
            }
        }
    }

    private func installAudioObservers(sessionID: UUID) {
        routeObserver = NotificationCenter.default.addObserver(
            forName: AVAudioSession.routeChangeNotification,
            object: AVAudioSession.sharedInstance(),
            queue: .main
        ) { [weak self] _ in
            Task { @MainActor [weak self] in
                self?.validateRoute(sessionID: sessionID)
            }
        }
        mediaResetObserver = NotificationCenter.default.addObserver(
            forName: AVAudioSession.mediaServicesWereResetNotification,
            object: AVAudioSession.sharedInstance(),
            queue: .main
        ) { [weak self] _ in
            Task { @MainActor [weak self] in
                guard self?.activeSessionID == sessionID else { return }
                let lastFrame = self?.currentState.latestFrame
                self?.finishCapture(publishIdle: false)
                self?.currentState = .paused(
                    reason: "iPad audio services restarted. Start IF capture again.",
                    lastFrame: lastFrame
                )
            }
        }
    }

    private func validateRoute(sessionID: UUID) {
        guard activeSessionID == sessionID else { return }
        let currentInputs = AVAudioSession.sharedInstance().currentRoute.inputs
        let hasSelectedInput = selectedInputUID.map { selectedUID in
            currentInputs.contains { input in
                input.portType == .usbAudio && input.uid == selectedUID
            }
        } ?? false
        guard hasSelectedInput else {
            let lastFrame = currentState.latestFrame
            finishCapture(publishIdle: false)
            currentState = .paused(
                reason: "The TH-D75 USB audio input disconnected or the system route changed.",
                lastFrame: lastFrame
            )
            return
        }
    }

    private func finishCapture(publishIdle: Bool) {
        activeSessionID = nil
        pcmContinuation?.finish()
        pcmContinuation = nil
        workerTask?.cancel()
        workerTask = nil

        if inputTapInstalled, let audioEngine {
            audioEngine.inputNode.removeTap(onBus: 0)
        }
        inputTapInstalled = false
        audioEngine?.stop()
        audioEngine = nil

        if let routeObserver {
            NotificationCenter.default.removeObserver(routeObserver)
            self.routeObserver = nil
        }
        if let mediaResetObserver {
            NotificationCenter.default.removeObserver(mediaResetObserver)
            self.mediaResetObserver = nil
        }

        if audioSessionIsActive {
            try? AVAudioSession.sharedInstance().setActive(
                false,
                options: .notifyOthersOnDeactivation
            )
            audioSessionIsActive = false
        }
        selectedInputUID = nil
        if publishIdle { currentState = .idle }
    }
}

private struct IFDSPSourcePCMBlock: Sendable {
    let samples: [Float]
    let sampleRate: Double

    init?(buffer: AVAudioPCMBuffer) {
        guard buffer.format.commonFormat == .pcmFormatFloat32,
              !buffer.format.isInterleaved,
              let channelData = buffer.floatChannelData else {
            return nil
        }
        let frameCount = Int(buffer.frameLength)
        let channelCount = Int(buffer.format.channelCount)
        guard frameCount > 0, channelCount > 0 else { return nil }

        if channelCount == 1 {
            samples = Array(UnsafeBufferPointer(start: channelData[0], count: frameCount))
        } else {
            var mono = [Float](repeating: 0, count: frameCount)
            let scale = 1 / Float(channelCount)
            for channel in 0..<channelCount {
                let source = channelData[channel]
                for frame in 0..<frameCount {
                    mono[frame] += source[frame] * scale
                }
            }
            samples = mono
        }
        sampleRate = buffer.format.sampleRate
    }
}

private final class IFDSPCaptureCounter: @unchecked Sendable {
    private let lock = NSLock()
    private var statistics = IFDSPCaptureStatistics()

    var snapshot: IFDSPCaptureStatistics {
        lock.withLock { statistics }
    }

    func recordSourceBlock(sampleCount: Int) {
        lock.withLock { statistics.recordSourceBlock(sampleCount: sampleCount) }
    }

    func recordDroppedBlock(sampleCount: Int) {
        lock.withLock { statistics.recordDroppedBlock(sampleCount: sampleCount) }
    }
}

/// Keeps at most one pending main-actor publication. DSP and capture continue
/// when rendering is temporarily busy instead of accumulating UI work.
private final class IFDSPFrameMailbox: @unchecked Sendable {
    private let lock = NSLock()
    private var latestFrame: IFDSPLiveFrame?
    private var deliveryScheduled = false

    func offer(_ frame: IFDSPLiveFrame) -> Bool {
        lock.withLock {
            latestFrame = frame
            guard !deliveryScheduled else { return false }
            deliveryScheduled = true
            return true
        }
    }

    func takeLatest() -> IFDSPLiveFrame? {
        lock.withLock {
            let frame = latestFrame
            latestFrame = nil
            deliveryScheduled = false
            return frame
        }
    }
}

private final class IFDSPPCMRateConverter {
    private let converter: AVAudioConverter
    private let sourceFormat: AVAudioFormat
    private let targetFormat: AVAudioFormat

    init(sourceSampleRate: Double) throws {
        guard let sourceFormat = AVAudioFormat(
            commonFormat: .pcmFormatFloat32,
            sampleRate: sourceSampleRate,
            channels: 1,
            interleaved: false
        ), let targetFormat = AVAudioFormat(
            commonFormat: .pcmFormatFloat32,
            sampleRate: 48_000,
            channels: 1,
            interleaved: false
        ), let converter = AVAudioConverter(from: sourceFormat, to: targetFormat) else {
            throw IFDSPAudioStreamError.conversionUnavailable
        }
        self.sourceFormat = sourceFormat
        self.targetFormat = targetFormat
        self.converter = converter
    }

    static func convert(
        _ block: IFDSPSourcePCMBlock,
        cachedConverter: inout IFDSPPCMRateConverter?
    ) throws -> [Float] {
        guard block.sampleRate.isFinite, block.sampleRate > 0 else {
            throw IFDSPAudioStreamError.invalidInputFormat
        }
        if abs(block.sampleRate - 48_000) < 0.5 { return block.samples }

        let converter: IFDSPPCMRateConverter
        if let cachedConverter,
           abs(cachedConverter.sourceFormat.sampleRate - block.sampleRate) < 0.5 {
            converter = cachedConverter
        } else {
            converter = try IFDSPPCMRateConverter(sourceSampleRate: block.sampleRate)
            cachedConverter = converter
        }
        return try converter.convert(block.samples)
    }

    private func convert(_ samples: [Float]) throws -> [Float] {
        guard let source = AVAudioPCMBuffer(
            pcmFormat: sourceFormat,
            frameCapacity: AVAudioFrameCount(samples.count)
        ), let sourceData = source.floatChannelData else {
            throw IFDSPAudioStreamError.conversionFailed
        }
        source.frameLength = AVAudioFrameCount(samples.count)
        samples.withUnsafeBufferPointer { pointer in
            if let baseAddress = pointer.baseAddress {
                sourceData[0].update(from: baseAddress, count: pointer.count)
            }
        }

        let estimatedFrames = ceil(Double(samples.count) * 48_000 / sourceFormat.sampleRate) + 32
        guard estimatedFrames <= Double(UInt32.max),
              let output = AVAudioPCMBuffer(
                pcmFormat: targetFormat,
                frameCapacity: AVAudioFrameCount(estimatedFrames)
              ) else {
            throw IFDSPAudioStreamError.conversionFailed
        }

        let inputProvider = IFDSPConverterInputProvider(source: source)
        var conversionError: NSError?
        let status = converter.convert(to: output, error: &conversionError) { _, inputStatus in
            inputProvider.nextBuffer(inputStatus: inputStatus)
        }
        if let conversionError { throw conversionError }
        switch status {
        case .haveData, .inputRanDry:
            break
        case .endOfStream, .error:
            throw IFDSPAudioStreamError.conversionFailed
        @unknown default:
            throw IFDSPAudioStreamError.conversionFailed
        }

        guard let outputData = output.floatChannelData else {
            throw IFDSPAudioStreamError.conversionFailed
        }
        return Array(
            UnsafeBufferPointer(start: outputData[0], count: Int(output.frameLength))
        )
    }
}

/// AVAudioConverter may invoke its input block from a concurrently executing
/// context. This provider makes the one-shot source handoff explicit and safe.
private final class IFDSPConverterInputProvider: @unchecked Sendable {
    private let lock = NSLock()
    private var source: AVAudioPCMBuffer?

    init(source: AVAudioPCMBuffer) {
        self.source = source
    }

    func nextBuffer(
        inputStatus: UnsafeMutablePointer<AVAudioConverterInputStatus>
    ) -> AVAudioBuffer? {
        lock.lock()
        defer { lock.unlock() }
        guard let source else {
            inputStatus.pointee = .noDataNow
            return nil
        }
        self.source = nil
        inputStatus.pointee = .haveData
        return source
    }
}

private extension IFDSPConfiguration {
    var coreValue: IfDspConfiguration {
        IfDspConfiguration(
            mode: mode.coreValue,
            filterHz: filterHz.map(Float.init)
        )
    }
}

private extension IFDSPMode {
    var coreValue: IfDspMode {
        switch self {
        case .usb: return .usb
        case .lsb: return .lsb
        case .cw: return .cw
        case .am: return .am
        }
    }
}

private extension IfDspSpectrum {
    var domainValue: IFDSPSpectrum {
        IFDSPSpectrum(
            firstBinOffsetHz: Double(firstBinOffsetHz),
            binWidthHz: Double(binWidthHz),
            levelsDBFS: levelsDbfs,
            peakOffsetHz: Double(peakOffsetHz),
            peakLevelDBFS: Double(peakLevelDbfs)
        )
    }
}

private enum IFDSPAudioStreamError: LocalizedError {
    case noUSBAudioInput(availableInputs: [String])
    case routeSelectionFailed(selected: String, currentInputs: [String])
    case invalidInputFormat
    case conversionUnavailable
    case conversionFailed

    var errorDescription: String? {
        switch self {
        case .noUSBAudioInput:
            return "No TH-D75 USB audio input is available. Connect the radio with a data cable and keep USB Function on COM + AF/IF Output."
        case .routeSelectionFailed(let selected, let currentInputs):
            let current = currentInputs.isEmpty ? "none" : currentInputs.joined(separator: ", ")
            return "iPadOS did not route audio to \(selected); the active input is \(current). Capture did not start."
        case .invalidInputFormat:
            return "The selected USB audio input did not expose valid PCM."
        case .conversionUnavailable:
            return "The USB audio format cannot be converted to the required 48 kHz mono IF stream."
        case .conversionFailed:
            return "A USB audio block could not be converted to 48 kHz mono PCM."
        }
    }
}

#else

/// macOS needs explicit CoreAudio device selection and an audio-input sandbox
/// entitlement before it can safely avoid the built-in microphone. This
/// implementation reports that blocker rather than analyzing the wrong input.
@MainActor
final class IFDSPAudioStreamService: IFDSPLiveStreaming {
    private(set) var currentState: IFDSPLiveStreamState
    private(set) var configuration: IFDSPConfiguration
    let monitoringState: IFDSPMonitoringState
    let updates: AsyncStream<IFDSPLiveStreamState>

    init(configuration: IFDSPConfiguration = .standard) {
        self.configuration = configuration
        let reason = "Live IF capture on macOS requires explicit CoreAudio device selection and is not enabled in this build."
        currentState = .failed(message: reason, lastFrame: nil)
        monitoringState = .unavailable(reason: reason)
        updates = AsyncStream { continuation in
            continuation.yield(.failed(message: reason, lastFrame: nil))
            continuation.finish()
        }
    }

    func start() async {}
    func stop() {}
    func setConfiguration(_ configuration: IFDSPConfiguration) async {
        self.configuration = configuration
    }
}

#endif
