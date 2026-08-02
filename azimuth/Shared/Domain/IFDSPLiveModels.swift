// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

/// Demodulation modes supported by the real 12 kHz low-IF pipeline.
enum IFDSPMode: String, CaseIterable, Identifiable, Equatable, Sendable {
    case usb
    case lsb
    case cw
    case am

    var id: String { rawValue }

    var title: String { rawValue.uppercased() }

    var defaultFilterHz: Double {
        switch self {
        case .usb, .lsb: return 2_600
        case .cw: return 500
        case .am: return 4_500
        }
    }
}

/// Operator-controlled live DSP configuration.
struct IFDSPConfiguration: Equatable, Sendable {
    var mode: IFDSPMode
    /// `nil` selects the mode default in the Rust pipeline.
    var filterHz: Double?

    static let standard = IFDSPConfiguration(mode: .usb, filterHz: nil)

    var effectiveFilterHz: Double { filterHz ?? mode.defaultFilterHz }
}

/// Physical audio route currently supplying samples.
struct IFDSPInputRoute: Equatable, Sendable {
    enum Kind: Equatable, Sendable {
        case usbAudio
        case systemDefault
    }

    let name: String
    let kind: Kind
    let sourceSampleRate: Double
    let sourceChannelCount: Int
}

/// Calibrated spectrum from physical radio PCM, never synthesized by the UI.
struct IFDSPSpectrum: Equatable, Sendable {
    let firstBinOffsetHz: Double
    let binWidthHz: Double
    let levelsDBFS: [Float]
    let peakOffsetHz: Double
    let peakLevelDBFS: Double

    var frequencyOffsetsHz: [Double] {
        levelsDBFS.indices.map { index in
            firstBinOffsetHz + Double(index) * binWidthHz
        }
    }
}

/// Latest measurements produced by the live PCM worker.
struct IFDSPLiveFrame: Equatable, Sendable {
    let sequence: UInt64
    let inputSampleCount: UInt64
    let inputLevelDBFS: Double
    let outputLevelDBFS: Double
    /// Retained latest spectrum. It becomes non-nil only after physical PCM
    /// has filled at least one FFT publication interval.
    let spectrum: IFDSPSpectrum?
    let clippedSampleCount: UInt64
    /// Audio-tap blocks and samples observed during this capture, including
    /// blocks later evicted by bounded backpressure.
    let sourceBlockCount: UInt64
    let sourceSampleCount: UInt64
    /// Old source blocks evicted to keep visualization latency bounded.
    let droppedBlockCount: UInt64
    let droppedSampleCount: UInt64
    let capturedAt: Date

    var captureLossFraction: Double {
        guard sourceSampleCount > 0 else { return 0 }
        return min(Double(droppedSampleCount) / Double(sourceSampleCount), 1)
    }
}

/// Accumulates arbitrary physical audio-tap chunks into stable DSP windows.
/// The worker owns this value; it never runs on the real-time audio callback.
struct IFDSPPCMBlockBatcher: Sendable {
    let targetSampleCount: Int
    private var pendingSamples: [Float]

    init(targetSampleCount: Int) {
        precondition(targetSampleCount > 0, "IF DSP batches require at least one sample")
        self.targetSampleCount = targetSampleCount
        var pendingSamples: [Float] = []
        pendingSamples.reserveCapacity(targetSampleCount * 2)
        self.pendingSamples = pendingSamples
    }

    var bufferedSampleCount: Int { pendingSamples.count }

    mutating func append(_ samples: [Float]) {
        pendingSamples.append(contentsOf: samples)
    }

    mutating func nextBatch() -> [Float]? {
        guard pendingSamples.count >= targetSampleCount else { return nil }
        let batch = Array(pendingSamples.prefix(targetSampleCount))
        pendingSamples.removeFirst(targetSampleCount)
        return batch
    }
}

/// Truthful source-side accounting, kept separate from Rust DSP batch counts.
struct IFDSPCaptureStatistics: Equatable, Sendable {
    private(set) var sourceBlockCount: UInt64 = 0
    private(set) var sourceSampleCount: UInt64 = 0
    private(set) var droppedBlockCount: UInt64 = 0
    private(set) var droppedSampleCount: UInt64 = 0

    mutating func recordSourceBlock(sampleCount: Int) {
        sourceBlockCount = sourceBlockCount.saturatingAdd(1)
        sourceSampleCount = sourceSampleCount.saturatingAdd(UInt64(max(sampleCount, 0)))
    }

    mutating func recordDroppedBlock(sampleCount: Int) {
        droppedBlockCount = droppedBlockCount.saturatingAdd(1)
        droppedSampleCount = droppedSampleCount.saturatingAdd(UInt64(max(sampleCount, 0)))
    }
}

private extension UInt64 {
    func saturatingAdd(_ other: UInt64) -> UInt64 {
        let (result, overflow) = addingReportingOverflow(other)
        return overflow ? .max : result
    }
}

/// Monitoring is modeled independently so a visualization cannot imply that
/// demodulated audio is audible.
enum IFDSPMonitoringState: Equatable, Sendable {
    case unavailable(reason: String)
    case disabled
    case active(output: String)
    case failed(message: String)

    var isActive: Bool {
        if case .active = self { return true }
        return false
    }
}

/// Complete truthful lifecycle of the live USB IF stream.
enum IFDSPLiveStreamState: Equatable, Sendable {
    case idle
    case requestingPermission
    case waitingForUSBAudio(availableInputs: [String])
    case starting(routeName: String)
    case streaming(route: IFDSPInputRoute, frame: IFDSPLiveFrame?)
    case paused(reason: String, lastFrame: IFDSPLiveFrame?)
    case failed(message: String, lastFrame: IFDSPLiveFrame?)

    var isStreaming: Bool {
        if case .streaming = self { return true }
        return false
    }

    var latestFrame: IFDSPLiveFrame? {
        switch self {
        case .streaming(_, let frame), .paused(_, let frame), .failed(_, let frame):
            return frame
        case .idle, .requestingPermission, .waitingForUSBAudio, .starting:
            return nil
        }
    }
}

/// Integration boundary for a real Apple audio route feeding the Rust DSP.
@MainActor
protocol IFDSPLiveStreaming: AnyObject {
    var currentState: IFDSPLiveStreamState { get }
    var updates: AsyncStream<IFDSPLiveStreamState> { get }
    var configuration: IFDSPConfiguration { get }
    var monitoringState: IFDSPMonitoringState { get }

    func start() async
    func stop()
    func setConfiguration(_ configuration: IFDSPConfiguration) async
}

/// Preview/test default that cannot accidentally imply live PCM.
@MainActor
final class UnavailableIFDSPLiveStream: IFDSPLiveStreaming {
    let currentState: IFDSPLiveStreamState
    let configuration: IFDSPConfiguration
    let monitoringState: IFDSPMonitoringState

    init(reason: String = "Live IF audio is unavailable in this environment.") {
        currentState = .failed(message: reason, lastFrame: nil)
        configuration = .standard
        monitoringState = .unavailable(reason: reason)
    }

    var updates: AsyncStream<IFDSPLiveStreamState> {
        let state = currentState
        return AsyncStream { continuation in
            continuation.yield(state)
            continuation.finish()
        }
    }

    func start() async {}
    func stop() {}
    func setConfiguration(_ configuration: IFDSPConfiguration) async {}
}
