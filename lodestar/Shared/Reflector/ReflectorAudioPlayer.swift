// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import AVFoundation
import OSLog

private let log = Logger(subsystem: "org.swiftraccoon.lodestar", category: "audio")

/// Plays the 8 kHz mono PCM produced by `RxAudioPipeline` through the
/// system output. `AVAudioEngine` handles the sample-rate conversion
/// from 8 kHz to the hardware rate; enhanced PCM buffers are scheduled
/// as soon as the causal model finalizes them.
@MainActor
public final class ReflectorAudioPlayer {
    private let engine = AVAudioEngine()
    private let player = AVAudioPlayerNode()
    private var started = false

    /// While `true`, playback is inaudible (used when the radio relay
    /// owns the audio path, or the user muted the monitor). The render
    /// pipeline keeps running either way: a stopped engine would make
    /// the app suspension-eligible in the background, taking the
    /// reflector link and USB relay down with it.
    public var isSuspended = false {
        didSet { engine.mainMixerNode.outputVolume = isSuspended ? 0 : 1 }
    }

    /// 8 kHz mono Float32: valid parameters, so construction cannot
    /// fail; checked once at startup.
    private let format = AVAudioFormat(
        commonFormat: .pcmFormatFloat32,
        sampleRate: 8000,
        channels: 1,
        interleaved: false
    )

    public init() {
        NotificationCenter.default.addObserver(
            forName: .AVAudioEngineConfigurationChange,
            object: engine,
            queue: nil
        ) { [weak self] _ in
            Task { @MainActor [weak self] in
                self?.handleEngineConfigurationChange()
            }
        }
        #if os(iOS)
        NotificationCenter.default.addObserver(
            forName: AVAudioSession.interruptionNotification,
            object: nil,
            queue: nil
        ) { [weak self] note in
            let ended = (note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt)
                .flatMap(AVAudioSession.InterruptionType.init) == .ended
            guard ended else { return }
            Task { @MainActor [weak self] in
                self?.handleEngineConfigurationChange()
            }
        }
        #endif
    }

    /// Route changes (headphones in/out) and media-services resets stop
    /// the engine out from under us. Drop the latch so the next enqueue
    /// rebuilds the graph and restarts; re-attaching an already
    /// attached node and re-connecting are documented no-ops/replacements.
    private func handleEngineConfigurationChange() {
        log.info("Audio engine configuration changed; will restart on next enqueue")
        player.stop()
        engine.stop()
        started = false
    }

    /// Spin up the audio session + engine before any PCM arrives.
    /// While the engine renders (silence when idle or muted), iOS's
    /// audio background mode keeps the process (and with it the
    /// reflector session and the USB user client) alive when
    /// backgrounded. Idempotent.
    public func beginKeepAlive() {
        guard let format else { return }
        do {
            try ensureEngine(format: format)
        } catch {
            log.error("Audio keep-alive start failed: \(error)")
        }
    }

    /// Stop rendering and release the audio session so an idle app can
    /// suspend normally. Called when the reflector link ends for good
    /// (user disconnect, terminal end, reconnect backoff exhausted).
    public func endKeepAlive() {
        player.stop()
        engine.stop()
        started = false
        #if os(iOS)
        try? AVAudioSession.sharedInstance().setActive(
            false, options: .notifyOthersOnDeactivation
        )
        #endif
    }

    /// Schedule one batch of PCM samples for playback. Scheduling
    /// continues while suspended: the mixer is at zero volume, and
    /// keeping the render pipeline fed preserves background
    /// execution. No-op when empty or if the engine fails to start.
    public func enqueue(_ pcm: [Int16]) {
        guard !pcm.isEmpty, let format else { return }
        do {
            try ensureEngine(format: format)
        } catch {
            log.error("Audio engine start failed: \(error)")
            return
        }
        guard let buffer = AVAudioPCMBuffer(
            pcmFormat: format,
            frameCapacity: AVAudioFrameCount(pcm.count)
        ) else { return }
        buffer.frameLength = AVAudioFrameCount(pcm.count)
        if let channel = buffer.floatChannelData?[0] {
            for (i, sample) in pcm.enumerated() {
                channel[i] = Float(sample) / 32768.0
            }
        }
        player.scheduleBuffer(buffer)
    }

    private func ensureEngine(format: AVAudioFormat) throws {
        guard !started else { return }
        #if os(iOS)
        try AVAudioSession.sharedInstance().setCategory(.playback, mode: .spokenAudio)
        try AVAudioSession.sharedInstance().setActive(true)
        #endif
        engine.attach(player)
        engine.connect(player, to: engine.mainMixerNode, format: format)
        try engine.start()
        // Re-apply after every engine (re)build: the didSet on
        // isSuspended may have fired against a stopped graph.
        engine.mainMixerNode.outputVolume = isSuspended ? 0 : 1
        player.play()
        started = true
    }
}
