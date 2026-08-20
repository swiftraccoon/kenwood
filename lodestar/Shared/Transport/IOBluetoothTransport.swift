// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
#if os(macOS)
import Darwin
#endif

/// macOS Bluetooth Classic SPP transport.
///
/// The app process never constructs an `IOBluetooth` object. It re-executes
/// its own signed binary with a private environment handshake; an Objective-C
/// constructor takes over before SwiftUI starts and owns RFCOMM plus its main
/// run loop in that disposable helper process. Parent/child stdin and stdout
/// carry framed discovery records or the raw radio byte stream.
///
/// This process boundary is required because every IOBluetooth write API can
/// ultimately block inside the framework without a cancellation primitive.
/// A cancelled or timed-out parent write kills and reaps the helper, so an
/// uncertain partial command can never leave a reusable transport behind.
public actor IOBluetoothTransport: RadioTransport {
    public let device: BluetoothDevice
    public nonisolated let pcOutputInterface: PcOutputInterface = .bluetooth
    private var _state: RadioTransportState = .disconnected
    private let stateContinuation: AsyncStream<RadioTransportState>.Continuation
    public nonisolated let stateStream: AsyncStream<RadioTransportState>

    #if os(macOS)
    private let configuredHelperMode: BluetoothHelperMode
    private var helper: BluetoothHelperProcess?
    private var helperGeneration: UInt64 = 0
    private var openInProgress = false
    private var activeWriteGeneration: UInt64?
    private var poisoned = false
    private var pendingBytes: [UInt8] = []
    private var waitingReads: [WaitingRead] = []
    private var reservedReadID: UInt64?
    private var nextReadID: UInt64 = 1
    #endif

    public init(device: BluetoothDevice) {
        self.device = device
        #if os(macOS)
        self.configuredHelperMode = .radio
        #endif
        var continuation: AsyncStream<RadioTransportState>.Continuation!
        self.stateStream = AsyncStream { continuation = $0 }
        self.stateContinuation = continuation
    }

    public var state: RadioTransportState { _state }

    /// Enumerate every bounded paired device in a short-lived helper.
    ///
    /// Discovery is metadata-only and does not guess radio identity from a
    /// display name. The user chooses one exact address; connection setup
    /// proves the endpoint's wire protocol before publishing it as connected.
    public nonisolated static func pairedDevices() -> [BluetoothDevice] {
        #if os(macOS)
        return BluetoothHelperProcess.pairedDevices()
        #else
        return []
        #endif
    }

    #if DEBUG && os(macOS)
    private init(testHelperMode: BluetoothHelperMode) {
        self.device = .mockTHD75
        self.configuredHelperMode = testHelperMode
        var continuation: AsyncStream<RadioTransportState>.Continuation!
        self.stateStream = AsyncStream { continuation = $0 }
        self.stateContinuation = continuation
    }

    nonisolated static func helperTestTransport(
        wedged: Bool = false
    ) -> IOBluetoothTransport {
        IOBluetoothTransport(
            testHelperMode: wedged ? .hangTest : .echoTest
        )
    }

    /// No-radio integration probe for the exact self-reexec/READY/raw-pipe
    /// path used in production.
    nonisolated static func helperEchoProbe(_ payload: [UInt8]) throws -> [UInt8] {
        try BluetoothHelperProcess.echoProbe(payload)
    }

    /// Exercises spawn with every pipe endpoint above the old fixed-FD range;
    /// the dynamically reserved liveness destination must not alias any pipe.
    nonisolated static func helperHighDescriptorProbe(
        _ payload: [UInt8]
    ) throws -> [UInt8] {
        var descriptors: [Int32] = []
        defer { for descriptor in descriptors { Darwin.close(descriptor) } }
        while (descriptors.last ?? -1) < 220 {
            let descriptor = Darwin.open("/dev/null", O_RDONLY)
            guard descriptor >= 0 else {
                throw RadioTransportError.openFailed(
                    reason: "Could not reserve high descriptors for helper test"
                )
            }
            descriptors.append(descriptor)
        }
        return try BluetoothHelperProcess.echoProbe(payload)
    }

    /// No-radio integration probe for one-helper exclusion and post-reap
    /// slot release, including a child intentionally wedged after READY.
    nonisolated static func helperReapProbe() throws {
        try BluetoothHelperProcess.reapProbe()
    }

    /// Verifies that paired discovery uses production control mode while
    /// echo and hang probes remain isolated as test modes.
    nonisolated static func helperEnvironmentProtocolProbe() -> Bool {
        lodestar_bt_helper_environment_protocol_probe() == 1
    }

    /// Parses one complete paired-device helper payload for protocol tests.
    nonisolated static func helperParsePairedDevicePayload(
        _ payload: [UInt8]
    ) -> [BluetoothDevice]? {
        BluetoothHelperProcess.parsePairedDevicePayloadForTesting(payload)
    }

    /// Runs the signed discovery helper and distinguishes a complete
    /// empty list from spawn, timeout, framing, or termination failure.
    nonisolated static func helperPairedDevicesForTesting() -> [BluetoothDevice]? {
        BluetoothHelperProcess.pairedDevicesForTesting()
    }
    #endif

    public func open() async throws {
        #if os(macOS)
        guard BluetoothHelperProcess.isExactBluetoothAddress(device.address) else {
            throw RadioTransportError.openFailed(
                reason: "Bluetooth connection requires the selected device's exact address"
            )
        }
        guard !openInProgress, helper == nil else {
            throw RadioTransportError.openFailed(
                reason: "A Bluetooth helper is already opening or connected"
            )
        }
        switch _state {
        case .connecting, .connected:
            throw RadioTransportError.openFailed(
                reason: "Bluetooth transport is already opening or connected"
            )
        case .disconnected, .failed:
            break
        }

        openInProgress = true
        helperGeneration &+= 1
        let generation = helperGeneration
        updateState(.connecting)

        do {
            var process = try BluetoothHelperProcess.spawn(
                device: device.address,
                mode: configuredHelperMode
            )
            process.generation = generation
            helper = process
            try await awaitReady(generation: generation)
            try Task.checkCancellation()
            guard var current = helper, current.generation == generation else {
                throw RadioTransportError.notConnected
            }

            let outputFD = current.outputFD
            current.outputFD = -1
            current.reader = BluetoothHelperPipeReader(
                descriptor: outputFD,
                generation: generation,
                owner: self
            )
            process = current
            helper = process
            openInProgress = false
            poisoned = false
            updateState(.connected)
        } catch {
            await terminateHelper(
                generation: generation,
                graceful: false,
                markPoisoned: true
            )
            if generation == helperGeneration {
                openInProgress = false
                let reason = (error as? RadioTransportError)?.displayMessage
                    ?? error.localizedDescription
                updateState(.failed(message: reason))
            }
            throw error
        }
        #else
        throw RadioTransportError.notAvailableOnPlatform(
            reason: "Bluetooth Classic SPP is macOS-only. On iPad, use the USB-C transport."
        )
        #endif
    }

    public func close() async {
        #if os(macOS)
        helperGeneration &+= 1
        let closeRequestGeneration = helperGeneration
        let closingGeneration = helper?.generation
        openInProgress = false
        activeWriteGeneration = nil
        if let closingGeneration {
            await terminateHelper(
                generation: closingGeneration,
                graceful: true,
                markPoisoned: false
            )
        }
        guard helperGeneration == closeRequestGeneration else { return }
        poisoned = false
        finishReadsForClose()
        pendingBytes.removeAll(keepingCapacity: false)
        #endif
        updateState(.disconnected)
    }

    public func write(_ bytes: [UInt8]) async throws {
        #if os(macOS)
        guard !bytes.isEmpty else { return }
        guard case .connected = _state,
              !poisoned,
              let active = helper else {
            throw RadioTransportError.notConnected
        }
        let generation = active.generation
        guard activeWriteGeneration == nil else {
            throw RadioTransportError.writeFailed(
                reason: "Another Bluetooth write is already in progress"
            )
        }
        activeWriteGeneration = generation
        defer {
            if activeWriteGeneration == generation {
                activeWriteGeneration = nil
            }
        }
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))

        do {
            for chunk in bytes.chunked(maximumCount: 512) {
                while true {
                    try Task.checkCancellation()
                    guard let current = helper,
                          current.generation == generation,
                          current.inputFD >= 0,
                          !poisoned else {
                        throw RadioTransportError.notConnected
                    }
                    let count = chunk.withUnsafeBytes { rawBuffer -> Int in
                        guard let baseAddress = rawBuffer.baseAddress else {
                            return 0
                        }
                        return Darwin.write(
                            current.inputFD, baseAddress, rawBuffer.count
                        )
                    }
                    if count == chunk.count {
                        try Task.checkCancellation()
                        break
                    }
                    if count >= 0 {
                        throw RadioTransportError.writeFailed(
                            reason: "Partial helper-pipe write \(count)/\(chunk.count)"
                        )
                    }
                    let writeErrno = errno
                    if writeErrno == EINTR { continue }
                    if writeErrno == EAGAIN || writeErrno == EWOULDBLOCK {
                        guard ContinuousClock.now < deadline else {
                            throw RadioTransportError.writeFailed(
                                reason: "Bluetooth helper write remained backpressured for 5s"
                            )
                        }
                        try await Task.sleep(for: .milliseconds(5))
                        continue
                    }
                    throw RadioTransportError.writeFailed(
                        reason: String(cString: strerror(writeErrno))
                    )
                }
            }
        } catch {
            // A cancellation/error can occur after only a prefix reached the
            // helper. The only honest recovery is to destroy that byte stream.
            await terminateHelper(
                generation: generation,
                graceful: false,
                markPoisoned: true
            )
            if generation == helperGeneration {
                updateState(.failed(message:
                    "Bluetooth write outcome is unknown; reconnect required"
                ))
            }
            throw error
        }
        #else
        throw RadioTransportError.notAvailableOnPlatform(
            reason: "No IOBluetooth transport on iPad."
        )
        #endif
    }

    public func read(maxBytes: Int) async throws -> [UInt8] {
        #if os(macOS)
        guard maxBytes > 0 else { return [] }
        if Task.isCancelled { return [] }

        if reservedReadID == nil,
           waitingReads.isEmpty,
           !pendingBytes.isEmpty {
            return takePendingBytes(maximum: maxBytes)
        }
        guard case .connected = _state, helper != nil, !poisoned else {
            return []
        }

        let id = nextReadID
        nextReadID &+= 1
        let gate = ReadCancellationGate()
        let signaled = await withTaskCancellationHandler {
            await withCheckedContinuation {
                (continuation: CheckedContinuation<Bool, Never>) in
                if gate.isCancelled {
                    continuation.resume(returning: false)
                    return
                }
                waitingReads.append(WaitingRead(
                    id: id,
                    maximum: maxBytes,
                    gate: gate,
                    continuation: continuation
                ))
                wakeNextReaderIfPossible()
            }
        } onCancel: {
            _ = gate.cancel()
            Task { await self.cancelRead(id: id) }
        }

        guard signaled, !Task.isCancelled else {
            cancelRead(id: id)
            return []
        }
        guard reservedReadID == id else { return [] }
        reservedReadID = nil
        let result = takePendingBytes(maximum: maxBytes)
        wakeNextReaderIfPossible()
        return result
        #else
        throw RadioTransportError.notAvailableOnPlatform(
            reason: "No IOBluetooth transport on iPad."
        )
        #endif
    }

    #if os(macOS)
    fileprivate func receiveHelperEvent(
        generation: UInt64,
        bytes: [UInt8],
        reachedEOF: Bool
    ) async {
        guard let current = helper, current.generation == generation else {
            return
        }
        if !bytes.isEmpty {
            pendingBytes.append(contentsOf: bytes)
            wakeNextReaderIfPossible()
        }
        if reachedEOF {
            if activeWriteGeneration == generation {
                activeWriteGeneration = nil
            }
            await terminateHelper(
                generation: generation,
                graceful: false,
                markPoisoned: true
            )
            guard generation == helperGeneration else { return }
            finishReadsForClose()
            updateState(.failed(message: "Bluetooth helper exited"))
        }
    }

    private func awaitReady(generation: UInt64) async throws {
        var ready = [UInt8](repeating: 0, count: bluetoothHelperReadyMagic.count)
        var offset = 0
        let deadline = ContinuousClock.now.advanced(by: .seconds(22))

        while offset < ready.count {
            try Task.checkCancellation()
            guard let current = helper,
                  current.generation == generation,
                  current.outputFD >= 0 else {
                throw RadioTransportError.notConnected
            }
            let count = ready.withUnsafeMutableBytes { rawBuffer -> Int in
                guard let baseAddress = rawBuffer.baseAddress else { return 0 }
                return Darwin.read(
                    current.outputFD,
                    baseAddress.advanced(by: offset),
                    rawBuffer.count - offset
                )
            }
            if count > 0 {
                offset += count
                continue
            }
            if count == 0 {
                throw RadioTransportError.openFailed(
                    reason: "Bluetooth helper exited before READY"
                )
            }
            let readErrno = errno
            if readErrno == EINTR { continue }
            if readErrno == EAGAIN || readErrno == EWOULDBLOCK {
                guard ContinuousClock.now < deadline else {
                    throw RadioTransportError.openFailed(
                        reason: "Bluetooth helper did not become ready within 22s"
                    )
                }
                try await Task.sleep(for: .milliseconds(5))
                continue
            }
            throw RadioTransportError.openFailed(
                reason: String(cString: strerror(readErrno))
            )
        }
        guard ready == bluetoothHelperReadyMagic else {
            throw RadioTransportError.openFailed(
                reason: "Bluetooth helper emitted an invalid READY frame"
            )
        }
    }

    private func terminateHelper(
        generation: UInt64,
        graceful: Bool,
        markPoisoned: Bool
    ) async {
        guard var current = helper, current.generation == generation else {
            if markPoisoned, generation == helperGeneration {
                poisoned = true
            }
            return
        }
        helper = nil
        current.reader?.stop()
        current.reader = nil
        if current.outputFD >= 0 {
            Darwin.close(current.outputFD)
            current.outputFD = -1
        }
        if current.inputFD >= 0 {
            Darwin.close(current.inputFD)
            current.inputFD = -1
        }
        let pid = current.pid
        let livenessFD = current.livenessFD
        let holdsSlot = current.holdsSlot
        current.livenessFD = -1
        if markPoisoned { poisoned = true }

        _ = await Task.detached(priority: .utility) {
            lodestar_bt_helper_terminate(
                pid,
                livenessFD,
                graceful ? 1 : 0,
                holdsSlot ? 1 : 0
            )
        }.value
    }

    private func takePendingBytes(maximum: Int) -> [UInt8] {
        let count = min(maximum, pendingBytes.count)
        let result = Array(pendingBytes.prefix(count))
        pendingBytes.removeFirst(count)
        return result
    }

    private func wakeNextReaderIfPossible() {
        guard reservedReadID == nil, !pendingBytes.isEmpty else { return }
        while !waitingReads.isEmpty {
            let waiter = waitingReads.removeFirst()
            if waiter.gate.signal() {
                reservedReadID = waiter.id
                waiter.continuation.resume(returning: true)
                return
            }
            waiter.continuation.resume(returning: false)
        }
    }

    private func cancelRead(id: UInt64) {
        if reservedReadID == id {
            reservedReadID = nil
            wakeNextReaderIfPossible()
            return
        }
        guard let index = waitingReads.firstIndex(where: { $0.id == id }) else {
            return
        }
        let waiter = waitingReads.remove(at: index)
        _ = waiter.gate.cancel()
        waiter.continuation.resume(returning: false)
    }

    private func finishReadsForClose() {
        reservedReadID = nil
        for waiter in waitingReads {
            _ = waiter.gate.cancel()
            waiter.continuation.resume(returning: false)
        }
        waitingReads.removeAll()
    }
    #endif

    private func updateState(_ newState: RadioTransportState) {
        _state = newState
        stateContinuation.yield(newState)
    }
}

private extension RadioTransportError {
    var displayMessage: String {
        switch self {
        case .notAvailableOnPlatform(let reason): return reason
        case .notConnected: return "Not connected"
        case .openFailed(let reason): return reason
        case .writeFailed(let reason): return reason
        case .readFailed(let reason): return reason
        case .deviceNotFound(let address): return "Device not found: \(address)"
        }
    }
}

#if os(macOS)

@_silgen_name("lodestar_bt_helper_spawn")
private func lodestar_bt_helper_spawn(
    _ executable: UnsafePointer<CChar>,
    _ device: UnsafePointer<CChar>,
    _ mode: Int32,
    _ pid: UnsafeMutablePointer<Int32>,
    _ input: UnsafeMutablePointer<Int32>,
    _ output: UnsafeMutablePointer<Int32>,
    _ liveness: UnsafeMutablePointer<Int32>,
    _ holdsSlot: UnsafeMutablePointer<Int32>
) -> Int32

@_silgen_name("lodestar_bt_helper_terminate")
private func lodestar_bt_helper_terminate(
    _ pid: Int32,
    _ liveness: Int32,
    _ graceful: Int32,
    _ holdsSlot: Int32
) -> Int32

@_silgen_name("lodestar_bt_helper_environment_protocol_probe")
private func lodestar_bt_helper_environment_protocol_probe() -> Int32

private let bluetoothHelperReadyMagic = Array("THD75BT-READY-v1".utf8)
private let bluetoothMaxPairedDevices = 64
private let bluetoothMaxPairedDisplayNameBytes = 1_024

private enum BluetoothHelperMode: Int32 {
    case radio = 0
    case pairedDevices = 1
    case echoTest = 2
    case hangTest = 3
}

private struct BluetoothHelperProcess {
    var pid: Int32
    var inputFD: Int32
    var outputFD: Int32
    var livenessFD: Int32
    var holdsSlot: Bool
    var generation: UInt64 = 0
    var reader: BluetoothHelperPipeReader?

    static func spawn(
        device: String,
        mode: BluetoothHelperMode
    ) throws -> BluetoothHelperProcess {
        guard let executable = Bundle.main.executableURL?.path else {
            throw RadioTransportError.openFailed(
                reason: "Cannot locate the signed Lodestar executable"
            )
        }
        var pid: Int32 = -1
        var input: Int32 = -1
        var output: Int32 = -1
        var liveness: Int32 = -1
        var holdsSlot: Int32 = 0
        let result = executable.withCString { executableCString in
            device.withCString { deviceCString in
                lodestar_bt_helper_spawn(
                    executableCString,
                    deviceCString,
                    mode.rawValue,
                    &pid,
                    &input,
                    &output,
                    &liveness,
                    &holdsSlot
                )
            }
        }
        guard result == 0 else {
            let spawnErrno = errno
            let reason = spawnErrno == EBUSY
                ? "Another Bluetooth helper is still alive"
                : String(cString: strerror(spawnErrno))
            throw RadioTransportError.openFailed(reason: reason)
        }
        return BluetoothHelperProcess(
            pid: pid,
            inputFD: input,
            outputFD: output,
            livenessFD: liveness,
            holdsSlot: holdsSlot != 0
        )
    }

    static func pairedDevices() -> [BluetoothDevice] {
        readPairedDevices() ?? []
    }

    private static func readPairedDevices() -> [BluetoothDevice]? {
        guard var process = try? spawn(device: "-", mode: .pairedDevices) else {
            return nil
        }
        var bytes: [UInt8] = []
        let deadline = ContinuousClock.now.advanced(by: .seconds(2))
        var complete = false

        while ContinuousClock.now < deadline && !complete {
            var buffer = [UInt8](repeating: 0, count: 4096)
            let count = buffer.withUnsafeMutableBytes { rawBuffer -> Int in
                guard let baseAddress = rawBuffer.baseAddress else { return 0 }
                return Darwin.read(
                    process.outputFD, baseAddress, rawBuffer.count
                )
            }
            if count > 0 {
                bytes.append(contentsOf: buffer.prefix(count))
                complete = pairedPayload(bytes) != nil
                continue
            }
            if count == 0 { break }
            let readErrno = errno
            if readErrno == EINTR { continue }
            if readErrno == EAGAIN || readErrno == EWOULDBLOCK {
                usleep(1_000)
                continue
            }
            break
        }

        Darwin.close(process.inputFD)
        Darwin.close(process.outputFD)
        process.inputFD = -1
        process.outputFD = -1
        _ = lodestar_bt_helper_terminate(
            process.pid,
            process.livenessFD,
            complete ? 1 : 0,
            process.holdsSlot ? 1 : 0
        )
        process.livenessFD = -1
        return pairedPayload(bytes)
    }

    #if DEBUG
    static func parsePairedDevicePayloadForTesting(
        _ payload: [UInt8]
    ) -> [BluetoothDevice]? {
        pairedPayload(payload)
    }

    static func pairedDevicesForTesting() -> [BluetoothDevice]? {
        readPairedDevices()
    }

    static func echoProbe(_ payload: [UInt8]) throws -> [UInt8] {
        guard payload.count <= 512 else {
            throw RadioTransportError.writeFailed(
                reason: "Echo test payload exceeds one atomic pipe frame"
            )
        }
        var process = try spawn(device: "-", mode: .echoTest)
        defer { terminateSynchronously(&process, graceful: false) }
        try readReadySynchronously(from: process.outputFD)
        try writeSynchronously(payload, to: process.inputFD)
        return try readSynchronously(
            count: payload.count,
            from: process.outputFD,
            timeout: .seconds(1)
        )
    }

    static func reapProbe() throws {
        var wedged = try spawn(device: "-", mode: .hangTest)
        do {
            var forbidden = try spawn(device: "-", mode: .hangTest)
            terminateSynchronously(&forbidden, graceful: false)
            terminateSynchronously(&wedged, graceful: false)
            throw RadioTransportError.openFailed(
                reason: "A concurrent helper unexpectedly acquired the slot"
            )
        } catch let error as RadioTransportError {
            guard case .openFailed(let reason) = error,
                  reason == "Another Bluetooth helper is still alive" else {
                terminateSynchronously(&wedged, graceful: false)
                throw error
            }
        }
        terminateSynchronously(&wedged, graceful: false)

        let deadline = ContinuousClock.now.advanced(by: .seconds(1))
        while true {
            do {
                var replacement = try spawn(device: "-", mode: .hangTest)
                terminateSynchronously(&replacement, graceful: false)
                return
            } catch let error as RadioTransportError {
                guard case .openFailed(let reason) = error,
                      reason == "Another Bluetooth helper is still alive",
                      ContinuousClock.now < deadline else {
                    throw error
                }
                usleep(1_000)
            }
        }
    }

    private static func terminateSynchronously(
        _ process: inout BluetoothHelperProcess,
        graceful: Bool
    ) {
        if process.inputFD >= 0 {
            Darwin.close(process.inputFD)
            process.inputFD = -1
        }
        if process.outputFD >= 0 {
            Darwin.close(process.outputFD)
            process.outputFD = -1
        }
        _ = lodestar_bt_helper_terminate(
            process.pid,
            process.livenessFD,
            graceful ? 1 : 0,
            process.holdsSlot ? 1 : 0
        )
        process.livenessFD = -1
    }

    private static func readReadySynchronously(from descriptor: Int32) throws {
        let actual = try readSynchronously(
            count: bluetoothHelperReadyMagic.count,
            from: descriptor,
            timeout: .seconds(1)
        )
        guard actual == bluetoothHelperReadyMagic else {
            throw RadioTransportError.openFailed(
                reason: "Test helper emitted an invalid READY frame"
            )
        }
    }

    private static func writeSynchronously(
        _ bytes: [UInt8],
        to descriptor: Int32
    ) throws {
        let deadline = ContinuousClock.now.advanced(by: .seconds(1))
        while true {
            let count = bytes.withUnsafeBytes { rawBuffer -> Int in
                guard let baseAddress = rawBuffer.baseAddress else { return 0 }
                return Darwin.write(descriptor, baseAddress, rawBuffer.count)
            }
            if count == bytes.count { return }
            if count >= 0 {
                throw RadioTransportError.writeFailed(
                    reason: "Test helper pipe accepted a partial frame"
                )
            }
            let writeErrno = errno
            if writeErrno == EINTR { continue }
            if (writeErrno == EAGAIN || writeErrno == EWOULDBLOCK),
               ContinuousClock.now < deadline {
                usleep(1_000)
                continue
            }
            throw RadioTransportError.writeFailed(
                reason: String(cString: strerror(writeErrno))
            )
        }
    }

    private static func readSynchronously(
        count: Int,
        from descriptor: Int32,
        timeout: Duration
    ) throws -> [UInt8] {
        var result: [UInt8] = []
        result.reserveCapacity(count)
        let deadline = ContinuousClock.now.advanced(by: timeout)
        while result.count < count {
            var buffer = [UInt8](
                repeating: 0,
                count: count - result.count
            )
            let readCount = buffer.withUnsafeMutableBytes { rawBuffer -> Int in
                guard let baseAddress = rawBuffer.baseAddress else { return 0 }
                return Darwin.read(descriptor, baseAddress, rawBuffer.count)
            }
            if readCount > 0 {
                result.append(contentsOf: buffer.prefix(readCount))
                continue
            }
            if readCount == 0 {
                throw RadioTransportError.readFailed(
                    reason: "Test helper exited before its complete frame"
                )
            }
            let readErrno = errno
            if readErrno == EINTR { continue }
            if (readErrno == EAGAIN || readErrno == EWOULDBLOCK),
               ContinuousClock.now < deadline {
                usleep(1_000)
                continue
            }
            throw RadioTransportError.readFailed(
                reason: readErrno == EAGAIN || readErrno == EWOULDBLOCK
                    ? "Test helper read timed out"
                    : String(cString: strerror(readErrno))
            )
        }
        return result
    }
    #endif

    private static func pairedPayload(_ bytes: [UInt8]) -> [BluetoothDevice]? {
        guard bytes.count >= bluetoothHelperReadyMagic.count,
              Array(bytes.prefix(bluetoothHelperReadyMagic.count))
                == bluetoothHelperReadyMagic else {
            return nil
        }
        var cursor = bluetoothHelperReadyMagic.count
        var devices: [BluetoothDevice] = []
        var addresses: Set<String> = []
        while true {
            guard bytes.count - cursor >= 4 else { return nil }
            let addressLength = Int(bytes[cursor]) << 8
                | Int(bytes[cursor + 1])
            let nameLength = Int(bytes[cursor + 2]) << 8
                | Int(bytes[cursor + 3])
            cursor += 4
            if addressLength == 0 && nameLength == 0 {
                guard cursor == bytes.count else { return nil }
                return devices
            }
            guard addressLength == 17,
                  nameLength > 0,
                  nameLength <= bluetoothMaxPairedDisplayNameBytes,
                  devices.count < bluetoothMaxPairedDevices,
                  bytes.count - cursor >= addressLength + nameLength else {
                return nil
            }
            let addressBytes = bytes[cursor..<(cursor + addressLength)]
            cursor += addressLength
            let nameBytes = bytes[cursor..<(cursor + nameLength)]
            cursor += nameLength
            guard let address = String(bytes: addressBytes, encoding: .utf8),
                  isExactBluetoothAddress(address),
                  addresses.insert(address.lowercased()).inserted,
                  let name = String(bytes: nameBytes, encoding: .utf8) else {
                return nil
            }
            devices.append(BluetoothDevice(
                id: address,
                name: name.isEmpty ? address : name,
                address: address
            ))
        }
    }

    fileprivate static func isExactBluetoothAddress(_ address: String) -> Bool {
        let bytes = Array(address.utf8)
        guard bytes.count == 17 else { return false }
        var separator: UInt8?
        for (index, byte) in bytes.enumerated() {
            if [2, 5, 8, 11, 14].contains(index) {
                guard byte == 0x2D || byte == 0x3A else { return false }
                if let separator {
                    guard byte == separator else { return false }
                } else {
                    separator = byte
                }
            } else {
                let isDigit = (0x30...0x39).contains(byte)
                let isUpperHex = (0x41...0x46).contains(byte)
                let isLowerHex = (0x61...0x66).contains(byte)
                guard isDigit || isUpperHex || isLowerHex else { return false }
            }
        }
        return true
    }
}

private struct WaitingRead {
    let id: UInt64
    let maximum: Int
    let gate: ReadCancellationGate
    let continuation: CheckedContinuation<Bool, Never>
}

private final class ReadCancellationGate: @unchecked Sendable {
    private enum State { case waiting, cancelled, signaled }
    private let lock = NSLock()
    private var state: State = .waiting

    var isCancelled: Bool {
        lock.withLock { state == .cancelled }
    }

    @discardableResult
    func cancel() -> Bool {
        lock.withLock {
            guard state == .waiting else { return false }
            state = .cancelled
            return true
        }
    }

    func signal() -> Bool {
        lock.withLock {
            guard state == .waiting else { return false }
            state = .signaled
            return true
        }
    }
}

private final class BluetoothHelperPipeReader: @unchecked Sendable {
    private let descriptor: Int32
    private let generation: UInt64
    private weak var owner: IOBluetoothTransport?
    private let source: DispatchSourceRead
    private let lock = NSLock()
    private var stopped = false

    init(
        descriptor: Int32,
        generation: UInt64,
        owner: IOBluetoothTransport
    ) {
        self.descriptor = descriptor
        self.generation = generation
        self.owner = owner
        self.source = DispatchSource.makeReadSource(
            fileDescriptor: descriptor,
            queue: DispatchQueue(label: "org.swiftraccoon.lodestar.bt-helper-read")
        )
        source.setEventHandler { [weak self] in self?.drain() }
        source.setCancelHandler { Darwin.close(descriptor) }
        source.activate()
    }

    func stop() {
        let shouldCancel = lock.withLock {
            guard !stopped else { return false }
            stopped = true
            return true
        }
        if shouldCancel { source.cancel() }
    }

    private func drain() {
        var received: [UInt8] = []
        var reachedEOF = false
        while true {
            var buffer = [UInt8](repeating: 0, count: 4096)
            let count = buffer.withUnsafeMutableBytes { rawBuffer -> Int in
                guard let baseAddress = rawBuffer.baseAddress else { return 0 }
                return Darwin.read(descriptor, baseAddress, rawBuffer.count)
            }
            if count > 0 {
                received.append(contentsOf: buffer.prefix(count))
                continue
            }
            if count == 0 {
                reachedEOF = true
                break
            }
            let readErrno = errno
            if readErrno == EINTR { continue }
            if readErrno == EAGAIN || readErrno == EWOULDBLOCK { break }
            reachedEOF = true
            break
        }

        if !received.isEmpty || reachedEOF {
            let eventGeneration = generation
            let eventBytes = received
            let eventReachedEOF = reachedEOF
            Task { @Sendable [weak owner] in
                guard let owner else { return }
                await owner.receiveHelperEvent(
                    generation: eventGeneration,
                    bytes: eventBytes,
                    reachedEOF: eventReachedEOF
                )
            }
        }
        if reachedEOF { stop() }
    }

    deinit { stop() }
}

private extension Array where Element == UInt8 {
    func chunked(maximumCount: Int) -> [[UInt8]] {
        precondition(maximumCount > 0)
        var result: [[UInt8]] = []
        result.reserveCapacity((count + maximumCount - 1) / maximumCount)
        var offset = 0
        while offset < count {
            let end = Swift.min(offset + maximumCount, count)
            result.append(Array(self[offset..<end]))
            offset = end
        }
        return result
    }
}

#endif
