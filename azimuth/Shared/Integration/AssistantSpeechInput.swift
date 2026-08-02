// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

/// Keeps volatile speech-recognition updates separate from finalized text so an
/// interim result replaces the previous interim result instead of duplicating it.
struct AssistantSpeechTranscript: Equatable {
    private(set) var originalText: String
    private(set) var finalizedSegments: [String] = []
    private(set) var volatileText = ""

    init(originalText: String) {
        self.originalText = originalText
    }

    var composedText: String {
        joined([originalText] + finalizedSegments + [volatileText])
    }

    var cancelledText: String { originalText }

    mutating func accept(_ text: String, isFinal: Bool) {
        let cleaned = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if isFinal {
            if !cleaned.isEmpty {
                finalizedSegments.append(cleaned)
            }
            volatileText = ""
        } else {
            volatileText = cleaned
        }
    }

    private func joined(_ components: [String]) -> String {
        components
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
            .joined(separator: " ")
    }
}

#if os(iOS)
@preconcurrency import AVFAudio
import Observation
import Speech

enum AssistantSpeechInputPhase: Equatable {
    case idle
    case preparing(String)
    case recording
    case finalizing
    case unavailable(String)
    case failed(String)
}

@Observable
@MainActor
final class AssistantSpeechInput {
    private(set) var phase: AssistantSpeechInputPhase = .idle

    var isActive: Bool {
        switch phase {
        case .preparing, .recording, .finalizing:
            return true
        case .idle, .unavailable, .failed:
            return false
        }
    }

    var isRecording: Bool {
        if case .recording = phase { return true }
        return false
    }

    var statusMessage: String? {
        switch phase {
        case .idle:
            return nil
        case .preparing(let message):
            return message
        case .recording:
            return "Listening…"
        case .finalizing:
            return "Finishing transcription…"
        case .unavailable(let message), .failed(let message):
            return message
        }
    }

    @ObservationIgnored private var activeSessionID: UUID?
    @ObservationIgnored private var transcript = AssistantSpeechTranscript(originalText: "")
    @ObservationIgnored private var onTextChange: ((String) -> Void)?
    @ObservationIgnored private var analyzer: SpeechAnalyzer?
    @ObservationIgnored private var analyzerInputContinuation: AsyncStream<AnalyzerInput>.Continuation?
    @ObservationIgnored private var resultsTask: Task<Void, Never>?
    @ObservationIgnored private var audioEngine: AVAudioEngine?
    @ObservationIgnored private var inputTapInstalled = false
    @ObservationIgnored private var audioSessionIsActive = false

    func start(
        currentText: String,
        onTextChange: @escaping (String) -> Void
    ) async {
        guard !isActive else { return }

        let sessionID = UUID()
        activeSessionID = sessionID
        transcript = AssistantSpeechTranscript(originalText: currentText)
        self.onTextChange = onTextChange
        phase = .preparing("Checking microphone access…")

        do {
            guard await microphonePermissionIsGranted() else {
                throw AssistantSpeechInputError.microphonePermissionDenied
            }
            try requireActiveSession(sessionID)

            phase = .preparing("Preparing on-device dictation…")
            let transcriber = try await selectTranscriber(for: .current)
            try requireActiveSession(sessionID)

            let modules = transcriber.modules
            try await installAssetsIfNeeded(for: modules)
            try requireActiveSession(sessionID)

            guard let analyzerFormat = await SpeechAnalyzer.bestAvailableAudioFormat(
                compatibleWith: modules
            ) else {
                throw AssistantSpeechInputError.noCompatibleAudioFormat
            }
            try requireActiveSession(sessionID)

            let analyzer = SpeechAnalyzer(modules: modules)
            self.analyzer = analyzer
            try await analyzer.prepareToAnalyze(in: analyzerFormat)
            try requireActiveSession(sessionID)

            let (inputSequence, inputContinuation) = AsyncStream<AnalyzerInput>.makeStream()
            analyzerInputContinuation = inputContinuation
            resultsTask = observeResults(from: transcriber, sessionID: sessionID)

            try await analyzer.start(inputSequence: inputSequence)
            try requireActiveSession(sessionID)
            try startAudioCapture(
                analyzerFormat: analyzerFormat,
                inputContinuation: inputContinuation,
                sessionID: sessionID
            )
            phase = .recording
        } catch is CancellationError {
            // A user-initiated cancel owns cleanup and restores the typed text.
            if activeSessionID == sessionID {
                await cancel()
            }
        } catch {
            await fail(error, sessionID: sessionID)
        }
    }

    func stop() async {
        guard case .recording = phase, let sessionID = activeSessionID else { return }
        phase = .finalizing
        finishAudioCapture()

        do {
            guard let analyzer else { throw AssistantSpeechInputError.analyzerUnavailable }
            try await analyzer.finalizeAndFinishThroughEndOfInput()
            await resultsTask?.value
            guard activeSessionID == sessionID else { return }

            clearResources(cancelResults: false)
            activeSessionID = nil
            onTextChange = nil
            phase = .idle
        } catch {
            await fail(error, sessionID: sessionID)
        }
    }

    func cancel() async {
        guard activeSessionID != nil else { return }

        activeSessionID = nil
        let analyzerToCancel = analyzer
        let originalText = transcript.cancelledText
        let textCallback = onTextChange

        finishAudioCapture()
        resultsTask?.cancel()
        await analyzerToCancel?.cancelAndFinishNow()
        clearResources(cancelResults: true)
        onTextChange = nil
        textCallback?(originalText)
        phase = .idle
    }

    private func microphonePermissionIsGranted() async -> Bool {
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

    private func selectTranscriber(for requestedLocale: Locale) async throws -> SelectedTranscriber {
        if SpeechTranscriber.isAvailable,
           let locale = await SpeechTranscriber.supportedLocale(equivalentTo: requestedLocale) {
            return .speech(
                SpeechTranscriber(
                    locale: locale,
                    transcriptionOptions: [],
                    reportingOptions: [.volatileResults],
                    attributeOptions: []
                )
            )
        }

        if let locale = await DictationTranscriber.supportedLocale(equivalentTo: requestedLocale) {
            return .dictation(
                DictationTranscriber(locale: locale, preset: .progressiveShortDictation)
            )
        }

        throw AssistantSpeechInputError.unsupportedLocale(requestedLocale.identifier)
    }

    private func installAssetsIfNeeded(for modules: [any SpeechModule]) async throws {
        switch await AssetInventory.status(forModules: modules) {
        case .unsupported:
            throw AssistantSpeechInputError.speechAssetsUnsupported
        case .installed:
            return
        case .supported, .downloading:
            phase = .preparing("Downloading the on-device language model…")
            if let request = try await AssetInventory.assetInstallationRequest(supporting: modules) {
                try await request.downloadAndInstall()
            }
        @unknown default:
            throw AssistantSpeechInputError.speechAssetsUnsupported
        }
    }

    private func observeResults(
        from transcriber: SelectedTranscriber,
        sessionID: UUID
    ) -> Task<Void, Never> {
        switch transcriber {
        case .speech(let speechTranscriber):
            return Task { @MainActor [weak self] in
                do {
                    for try await result in speechTranscriber.results {
                        guard !Task.isCancelled else { return }
                        self?.acceptResult(
                            String(result.text.characters),
                            isFinal: result.isFinal,
                            sessionID: sessionID
                        )
                    }
                } catch is CancellationError {
                    return
                } catch {
                    guard !Task.isCancelled else { return }
                    await self?.fail(error, sessionID: sessionID)
                }
            }
        case .dictation(let dictationTranscriber):
            return Task { @MainActor [weak self] in
                do {
                    for try await result in dictationTranscriber.results {
                        guard !Task.isCancelled else { return }
                        self?.acceptResult(
                            String(result.text.characters),
                            isFinal: result.isFinal,
                            sessionID: sessionID
                        )
                    }
                } catch is CancellationError {
                    return
                } catch {
                    guard !Task.isCancelled else { return }
                    await self?.fail(error, sessionID: sessionID)
                }
            }
        }
    }

    private func acceptResult(_ text: String, isFinal: Bool, sessionID: UUID) {
        guard activeSessionID == sessionID else { return }
        transcript.accept(text, isFinal: isFinal)
        onTextChange?(transcript.composedText)
    }

    private func startAudioCapture(
        analyzerFormat: AVAudioFormat,
        inputContinuation: AsyncStream<AnalyzerInput>.Continuation,
        sessionID: UUID
    ) throws {
        let audioSession = AVAudioSession.sharedInstance()
        try audioSession.setCategory(
            .playAndRecord,
            mode: .spokenAudio,
            options: [.allowBluetoothHFP]
        )
        try audioSession.setActive(true)
        audioSessionIsActive = true

        let engine = AVAudioEngine()
        audioEngine = engine
        let inputNode = engine.inputNode
        let inputFormat = inputNode.outputFormat(forBus: 0)
        guard inputFormat.sampleRate > 0, inputFormat.channelCount > 0 else {
            throw AssistantSpeechInputError.noAudioInput
        }
        guard let converter = AVAudioConverter(from: inputFormat, to: analyzerFormat) else {
            throw AssistantSpeechInputError.noCompatibleAudioFormat
        }

        inputNode.installTap(
            onBus: 0,
            bufferSize: 4_096,
            format: inputFormat
        ) { [weak self] buffer, _ in
            do {
                let converted = try Self.convert(
                    buffer,
                    using: converter,
                    to: analyzerFormat
                )
                inputContinuation.yield(AnalyzerInput(buffer: converted))
            } catch {
                Task { @MainActor [weak self] in
                    await self?.fail(error, sessionID: sessionID)
                }
            }
        }
        inputTapInstalled = true

        engine.prepare()
        do {
            try engine.start()
        } catch {
            finishAudioCapture()
            throw error
        }
    }

    nonisolated private static func convert(
        _ input: AVAudioPCMBuffer,
        using converter: AVAudioConverter,
        to outputFormat: AVAudioFormat
    ) throws -> AVAudioPCMBuffer {
        let rateRatio = outputFormat.sampleRate / input.format.sampleRate
        let estimatedFrames = ceil(Double(input.frameLength) * rateRatio) + 32
        let frameCapacity = AVAudioFrameCount(max(estimatedFrames, 1))
        guard let output = AVAudioPCMBuffer(
            pcmFormat: outputFormat,
            frameCapacity: frameCapacity
        ) else {
            throw AssistantSpeechInputError.audioConversionFailed
        }

        var conversionError: NSError?
        let inputProvider = AssistantAudioConversionInput(buffer: input)
        let status = converter.convert(to: output, error: &conversionError) { _, inputStatus in
            inputProvider.nextBuffer(status: inputStatus)
        }

        if let conversionError { throw conversionError }
        guard output.frameLength > 0 else {
            throw AssistantSpeechInputError.audioConversionFailed
        }
        switch status {
        case .haveData, .inputRanDry:
            return output
        case .endOfStream, .error:
            throw AssistantSpeechInputError.audioConversionFailed
        @unknown default:
            throw AssistantSpeechInputError.audioConversionFailed
        }
    }

    private func finishAudioCapture() {
        if inputTapInstalled, let audioEngine {
            audioEngine.inputNode.removeTap(onBus: 0)
        }
        inputTapInstalled = false
        audioEngine?.stop()
        audioEngine = nil
        analyzerInputContinuation?.finish()
        analyzerInputContinuation = nil

        if audioSessionIsActive {
            try? AVAudioSession.sharedInstance().setActive(
                false,
                options: .notifyOthersOnDeactivation
            )
            audioSessionIsActive = false
        }
    }

    private func fail(_ error: Error, sessionID: UUID) async {
        guard activeSessionID == sessionID else { return }

        activeSessionID = nil
        let analyzerToCancel = analyzer
        let originalText = transcript.cancelledText
        let textCallback = onTextChange

        finishAudioCapture()
        resultsTask?.cancel()
        await analyzerToCancel?.cancelAndFinishNow()
        clearResources(cancelResults: true)
        onTextChange = nil
        textCallback?(originalText)

        if let speechError = error as? AssistantSpeechInputError,
           speechError.representsUnavailability {
            phase = .unavailable(speechError.localizedDescription)
        } else {
            phase = .failed(
                "Dictation stopped: \(error.localizedDescription) You can keep typing your request."
            )
        }
    }

    private func clearResources(cancelResults: Bool) {
        finishAudioCapture()
        if cancelResults { resultsTask?.cancel() }
        resultsTask = nil
        analyzer = nil
    }

    private func requireActiveSession(_ sessionID: UUID) throws {
        guard activeSessionID == sessionID else { throw CancellationError() }
    }
}

private final class AssistantAudioConversionInput: @unchecked Sendable {
    private let buffer: AVAudioPCMBuffer
    private var wasSupplied = false

    init(buffer: AVAudioPCMBuffer) {
        self.buffer = buffer
    }

    func nextBuffer(
        status: UnsafeMutablePointer<AVAudioConverterInputStatus>
    ) -> AVAudioBuffer? {
        guard !wasSupplied else {
            status.pointee = .noDataNow
            return nil
        }
        wasSupplied = true
        status.pointee = .haveData
        return buffer
    }
}

private enum SelectedTranscriber {
    case speech(SpeechTranscriber)
    case dictation(DictationTranscriber)

    var modules: [any SpeechModule] {
        switch self {
        case .speech(let transcriber): return [transcriber]
        case .dictation(let transcriber): return [transcriber]
        }
    }
}

private enum AssistantSpeechInputError: LocalizedError {
    case microphonePermissionDenied
    case unsupportedLocale(String)
    case speechAssetsUnsupported
    case noCompatibleAudioFormat
    case noAudioInput
    case analyzerUnavailable
    case audioConversionFailed

    var representsUnavailability: Bool {
        switch self {
        case .microphonePermissionDenied, .unsupportedLocale, .speechAssetsUnsupported,
             .noCompatibleAudioFormat, .noAudioInput:
            return true
        case .analyzerUnavailable, .audioConversionFailed:
            return false
        }
    }

    var errorDescription: String? {
        switch self {
        case .microphonePermissionDenied:
            return "Microphone access is off. Allow it in Settings to dictate, or keep typing."
        case .unsupportedLocale(let identifier):
            return "On-device dictation is not available for \(identifier). You can keep typing."
        case .speechAssetsUnsupported:
            return "This iPad cannot install the on-device dictation model. You can keep typing."
        case .noCompatibleAudioFormat:
            return "On-device dictation cannot use the current microphone format. You can keep typing."
        case .noAudioInput:
            return "No microphone input is available. You can keep typing."
        case .analyzerUnavailable:
            return "The on-device speech analyzer ended unexpectedly."
        case .audioConversionFailed:
            return "Microphone audio could not be prepared for on-device dictation."
        }
    }
}
#endif
