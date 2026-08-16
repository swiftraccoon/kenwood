// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import OSLog

private let azimuthUSBTransportLog = Logger(
    subsystem: "org.swiftraccoon.azimuth",
    category: "usb-serial"
)

/// Build diagnostic messages as ordinary strings, then mark the complete
/// value public at the unified-log boundary. Besides keeping radio metadata
/// visible, this avoids `OSLogMessage` concatenation at multiline call sites.
private let azimuthVerboseUSBTracing =
    ProcessInfo.processInfo.environment["AZIMUTH_VERBOSE_USB_TRACE"] == "1"

private func azimuthUSBTrace(_ message: String) {
    guard azimuthVerboseUSBTracing else { return }
    azimuthUSBTransportLog.debug("\(message, privacy: .public)")
}

private func azimuthUSBNotice(_ message: String) {
    azimuthUSBTransportLog.notice("\(message, privacy: .public)")
}

private func azimuthUSBError(_ message: String) {
    azimuthUSBTransportLog.error("\(message, privacy: .public)")
}

/// Synchronous generation gate for callbacks that may arrive on an OS queue
/// after the actor has closed and reopened the underlying USB session.
private final class AzimuthUSBConnectionGeneration: @unchecked Sendable {
    private let lock = NSLock()
    private var value: UInt64 = 0

    func advance() -> UInt64 {
        lock.lock()
        defer { lock.unlock() }
        value &+= 1
        return value
    }

    func isCurrent(_ generation: UInt64) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return value == generation
    }
}

/// Direct TH-D75 transport shared by the iPad DriverKit and native macOS CDC
/// implementations. The link protocol deliberately hides their different OS
/// plumbing while retaining the same lossless doorbell/drain behavior.
public actor AzimuthUSBSerialTransport: AzimuthRadioTransport {
    public let device: AzimuthRadioDevice
    private let link: any AzimuthUSBSerialLink

    private var currentState: AzimuthRadioTransportState = .disconnected
    private let stateContinuation: AsyncStream<AzimuthRadioTransportState>.Continuation
    public nonisolated let stateStream: AsyncStream<AzimuthRadioTransportState>

    private var receiveBuffer: [UInt8] = []
    private var readers:
        [(id: UInt64, limit: Int, continuation: CheckedContinuation<[UInt8], Error>)] = []
    private var nextReaderID: UInt64 = 0
    private var linkDown = true
    private nonisolated let connectionGeneration = AzimuthUSBConnectionGeneration()
    /// The dext returns a rolling snapshot. Remember the last event already
    /// emitted so repeated lifecycle snapshots do not flood Xcode's console.
    private var lastLoggedDextEntry: AzimuthUSBDextLogEntry?

    /// 20 retries at 10 ms gives bounded, 200 ms backpressure tolerance.
    private static let backpressureRetries = 20
    private static let backpressureDelayNanoseconds: UInt64 = 10_000_000
    private static let maximumLoggedDextEntries = 16

    public init(
        device: AzimuthRadioDevice = .thD75USBC,
        link: any AzimuthUSBSerialLink
    ) {
        self.device = device
        self.link = link
        var continuation: AsyncStream<AzimuthRadioTransportState>.Continuation!
        stateStream = AsyncStream { continuation = $0 }
        stateContinuation = continuation
    }

    public var state: AzimuthRadioTransportState { currentState }

    public var hardwareSerialNumber: String? { link.hardwareSerialNumber }

    public func open() async throws {
        guard currentState != .connected else { return }
        let generation = connectionGeneration.advance()
        updateState(.connecting)

        var dataServicePresent = link.servicePresent()
        var controlServicePresent = link.commServicePresent()
        azimuthUSBNotice(
            "[Azimuth USB] open requested: path=\(link.connectionDescription) "
                + "dataService=\(dataServicePresent) "
                + "controlService=\(Self.presenceText(controlServicePresent))"
        )

        do {
            if (!dataServicePresent || controlServicePresent == false),
               link.serviceRegistrationWaitNanoseconds > 0 {
                let attempts = max(1, Int(link.serviceRegistrationWaitNanoseconds / 100_000_000))
                azimuthUSBNotice(
                    "[Azimuth USB] waiting for DriverKit data/control services: "
                        + "timeoutMs=\(link.serviceRegistrationWaitNanoseconds / 1_000_000)"
                )
                var completedChecks = 0
                for attempt in 0..<attempts {
                    dataServicePresent = link.servicePresent()
                    controlServicePresent = link.commServicePresent()
                    completedChecks = attempt + 1
                    if dataServicePresent && controlServicePresent != false { break }
                    try await Task.sleep(nanoseconds: 100_000_000)
                    try ensureOpenIsCurrent(generation)
                }
                try ensureOpenIsCurrent(generation)
                if !dataServicePresent { dataServicePresent = link.servicePresent() }
                if controlServicePresent == false {
                    controlServicePresent = link.commServicePresent()
                }
                azimuthUSBNotice(
                    "[Azimuth USB] DriverKit wait complete: "
                        + "checks=\(completedChecks) "
                        + "dataService=\(dataServicePresent) "
                        + "controlService=\(Self.presenceText(controlServicePresent))"
                )
            }
            try ensureOpenIsCurrent(generation)
        } catch is CancellationError {
            cancelOpeningConnectionIfCurrent(
                generation: generation,
                closeAttemptedLink: false
            )
            throw CancellationError()
        }
        guard dataServicePresent else {
            let reason = Self.missingServiceDescription
            emitDiagnosticSnapshot(context: "open failed before IOServiceOpen", includeDriverDetails: false)
            azimuthUSBError("[Azimuth USB] open failed: \(reason)")
            updateState(.failed(message: reason))
            throw AzimuthRadioTransportError.openFailed(reason: reason)
        }
        guard controlServicePresent != false else {
            let reason = Self.missingControlServiceDescription
            emitDiagnosticSnapshot(
                context: "open failed before IOServiceOpen",
                includeDriverDetails: false
            )
            azimuthUSBError("[Azimuth USB] open failed: \(reason)")
            updateState(.failed(message: reason))
            throw AzimuthRadioTransportError.openFailed(reason: reason)
        }

        var linkOpenWasAttempted = false
        do {
            try ensureOpenIsCurrent(generation)
            azimuthUSBNotice("[Azimuth USB] opening DriverKit user client")
            linkOpenWasAttempted = true
            try link.open()
            try ensureOpenIsCurrent(generation)
            azimuthUSBNotice("[Azimuth USB] DriverKit user client open succeeded")
            linkDown = false
            try armDoorbell(generation: generation)
            // The dext/tty may already contain bytes. This also closes the
            // arm-vs-arrival race when the link's immediate callback is async.
            try drainEverything(source: "open")
            try ensureOpenIsCurrent(generation)
            updateState(.connected)
            emitDiagnosticSnapshot(context: "open complete", includeDriverDetails: true)
        } catch is CancellationError {
            cancelOpeningConnectionIfCurrent(
                generation: generation,
                closeAttemptedLink: linkOpenWasAttempted
            )
            throw CancellationError()
        } catch {
            guard connectionGeneration.isCurrent(generation) else {
                throw CancellationError()
            }
            let reason = Self.describe(error)
            emitDiagnosticSnapshot(context: "open failed", includeDriverDetails: true)
            azimuthUSBError("[Azimuth USB] open failed: \(reason)")
            linkDown = true
            _ = connectionGeneration.advance()
            link.close()
            updateState(.failed(message: reason))
            throw AzimuthRadioTransportError.openFailed(reason: reason)
        }
    }

    public func close() async {
        guard currentState != .disconnected || !linkDown else { return }
        azimuthUSBNotice("[Azimuth USB] close requested: state=\(currentState)")
        if !linkDown {
            // Capture the dext while the user client is still valid. This is
            // especially important when the radio core rejects qualification
            // immediately after the USB transport itself opened successfully.
            emitDiagnosticSnapshot(context: "pre-close", includeDriverDetails: true)
        }
        linkDown = true
        _ = connectionGeneration.advance()
        link.close()
        receiveBuffer.removeAll(keepingCapacity: false)
        resumeAllReaders(with: [])
        updateState(.disconnected)
        azimuthUSBNotice("[Azimuth USB] close complete")
    }

    /// This remains nonisolated and synchronous for the generated core
    /// callback. Link implementations serialize the actual OS operation.
    public nonisolated func setBaudRate(baud: UInt32) throws {
        guard AzimuthUSBABIV2.supportedBaudRates.contains(baud) else {
            throw AzimuthUSBLinkError.unsupportedBaudRate(baud)
        }
        azimuthUSBNotice("[Azimuth USB] set baud requested: baud=\(baud)")
        do {
            try link.setBaudRate(baud: baud)
            azimuthUSBTrace("[Azimuth USB] set baud accepted: baud=\(baud)")
        } catch {
            azimuthUSBError(
                "[Azimuth USB] set baud failed: baud=\(baud) error=\(error)"
            )
            throw error
        }
    }

    public func write(_ bytes: [UInt8]) async throws {
        guard currentState == .connected, !linkDown else {
            throw AzimuthRadioTransportError.notConnected
        }
        guard !bytes.isEmpty else { return }

        // Selector v1 is explicitly bounded to 4096 bytes. Chunking here lets
        // callers submit larger automation payloads without widening the ABI.
        var offset = 0
        while offset < bytes.count {
            let end = min(offset + AzimuthUSBABIV1.maximumTransferBytes, bytes.count)
            try await writeChunk(Array(bytes[offset..<end]))
            offset = end
        }
    }

    public func read(maxBytes: Int) async throws -> [UInt8] {
        guard maxBytes > 0 else {
            throw AzimuthRadioTransportError.readFailed(reason: "maxBytes must be positive")
        }
        if !receiveBuffer.isEmpty {
            let bytes = takeBuffered(maxBytes: maxBytes)
            azimuthUSBTrace(
                "[Azimuth USB] read returned immediately: bytes=\(bytes.count) "
                    + "bufferedRemaining=\(receiveBuffer.count)"
            )
            return bytes
        }
        if linkDown {
            azimuthUSBNotice("[Azimuth USB] read returned EOF: link is down")
            return []
        }

        let id = nextReaderID
        nextReaderID &+= 1
        azimuthUSBTrace("[Azimuth USB] read parked: id=\(id) maxBytes=\(maxBytes)")
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                readers.append((id: id, limit: maxBytes, continuation: continuation))
            }
        } onCancel: {
            Task { await self.cancelReader(id: id) }
        }
    }

    public func diagnosticsReport() -> String {
        var lines = [
            "transport: state=\(currentState) path=\(link.connectionDescription) "
                + "buffered=\(receiveBuffer.count) parkedReads=\(readers.count)",
            "DriverKit services: data=\(link.servicePresent()) "
                + "control=\(Self.presenceText(link.commServicePresent()))"
        ]
        do {
            if let status = try link.status() {
                lines.append(status.text)
            } else {
                lines.append("dext status: not applicable")
            }
        } catch {
            lines.append("dext status failed: \(Self.describe(error))")
        }
        if let comm = link.commServicePresent() {
            lines.append("CDC control-interface driver registered: \(comm)")
        }
        do {
            let entries = try link.dextLog()
            if !entries.isEmpty {
                lines.append("dext log (\(entries.count) events):")
                lines.append(contentsOf: entries.map { "  " + $0.text })
            }
        } catch {
            lines.append("dext log failed: \(Self.describe(error))")
        }
        return lines.joined(separator: "\n")
    }

    private func writeChunk(_ bytes: [UInt8]) async throws {
        for attempt in 0...Self.backpressureRetries {
            azimuthUSBTrace(
                "[Azimuth USB] write chunk: bytes=\(bytes.count) "
                    + "attempt=\(attempt + 1)/\(Self.backpressureRetries + 1)"
            )
            do {
                try link.write(bytes)
                azimuthUSBTrace(
                    "[Azimuth USB] write accepted: bytes=\(bytes.count) attempt=\(attempt + 1)"
                )
                return
            } catch AzimuthUSBLinkError.backpressure {
                azimuthUSBNotice(
                    "[Azimuth USB] write backpressure: bytes=\(bytes.count) "
                        + "attempt=\(attempt + 1)"
                )
                guard attempt < Self.backpressureRetries else {
                    throw AzimuthRadioTransportError.writeFailed(
                        reason: "USB transmit queue stayed full for 200 ms"
                    )
                }
                try await Task.sleep(nanoseconds: Self.backpressureDelayNanoseconds)
            } catch {
                let reason = Self.describe(error)
                azimuthUSBError("[Azimuth USB] write failed: \(reason)")
                handleLinkDrop()
                throw AzimuthRadioTransportError.writeFailed(reason: reason)
            }
        }
    }

    // MARK: Doorbell + drain

    private func ensureOpenIsCurrent(_ generation: UInt64) throws {
        guard connectionGeneration.isCurrent(generation) else {
            throw CancellationError()
        }
        try Task.checkCancellation()
    }

    private func cancelOpeningConnectionIfCurrent(
        generation: UInt64,
        closeAttemptedLink: Bool
    ) {
        guard connectionGeneration.isCurrent(generation) else { return }
        _ = connectionGeneration.advance()
        linkDown = true
        if closeAttemptedLink { link.close() }
        receiveBuffer.removeAll(keepingCapacity: false)
        resumeAllReaders(with: [])
        updateState(.disconnected)
    }

    private func armDoorbell(generation: UInt64) throws {
        try link.armDoorbell { [weak self] hasData in
            guard let self, self.connectionGeneration.isCurrent(generation) else {
                return
            }
            Task {
                await self.doorbellFired(
                    hasData: hasData,
                    generation: generation
                )
            }
        }
        azimuthUSBTrace(
            "[Azimuth USB] one-shot doorbell armed: generation=\(generation)"
        )
    }

    private func doorbellFired(hasData: Bool, generation: UInt64) {
        guard connectionGeneration.isCurrent(generation), !linkDown else { return }
        azimuthUSBTrace(
            "[Azimuth USB] doorbell fired: hasData=\(hasData) generation=\(generation)"
        )
        guard hasData else {
            handleLinkDrop(reason: "doorbell reported link down")
            return
        }
        do {
            // Consume the edge that fired this one-shot before re-arming it.
            // The dext serializes read and arm: bytes arriving in the gap make
            // RegisterDoorbell fire immediately, while later bytes fire the
            // newly armed notification normally.
            try drainEverything(source: "doorbell")
            try armDoorbell(generation: generation)
        } catch {
            azimuthUSBError("[Azimuth USB] receive pump failed: \(Self.describe(error))")
            handleLinkDrop(reason: "receive pump failed: \(Self.describe(error))")
        }
    }

    private func drainEverything(source: String) throws {
        var chunks = 0
        var totalBytes = 0
        while true {
            let bytes = try link.drain(maxBytes: AzimuthUSBABIV1.maximumTransferBytes)
            if bytes.isEmpty { break }
            chunks += 1
            totalBytes += bytes.count
            receiveBuffer.append(contentsOf: bytes)
        }
        azimuthUSBTrace(
            "[Azimuth USB] drain complete: source=\(source) "
                + "chunks=\(chunks) bytes=\(totalBytes) buffered=\(receiveBuffer.count)"
        )
        deliverBufferedBytes()
    }

    private func deliverBufferedBytes() {
        while !receiveBuffer.isEmpty, !readers.isEmpty {
            let reader = readers.removeFirst()
            let bytes = takeBuffered(maxBytes: reader.limit)
            azimuthUSBTrace(
                "[Azimuth USB] parked read delivered: id=\(reader.id) bytes=\(bytes.count) "
                    + "bufferedRemaining=\(receiveBuffer.count)"
            )
            reader.continuation.resume(returning: bytes)
        }
    }

    private func takeBuffered(maxBytes: Int) -> [UInt8] {
        let count = min(maxBytes, receiveBuffer.count)
        let result = Array(receiveBuffer.prefix(count))
        receiveBuffer.removeFirst(count)
        return result
    }

    private func cancelReader(id: UInt64) {
        guard let index = readers.firstIndex(where: { $0.id == id }) else { return }
        azimuthUSBTrace("[Azimuth USB] parked read cancelled: id=\(id)")
        readers.remove(at: index).continuation.resume(returning: [])
    }

    private func handleLinkDrop(reason: String = "link dropped") {
        guard !linkDown else { return }
        azimuthUSBError("[Azimuth USB] link down: \(reason)")
        emitDiagnosticSnapshot(context: "link down", includeDriverDetails: true)
        linkDown = true
        _ = connectionGeneration.advance()
        link.close()
        resumeAllReaders(with: [])
        updateState(.disconnected)
    }

    /// Emits the diagnostics that previously lived only in
    /// `diagnosticsReport()` into unified logging, where an attached Xcode can
    /// display them during a real iPad connection attempt.
    private func emitDiagnosticSnapshot(context: String, includeDriverDetails: Bool) {
        let dataPresent = link.servicePresent()
        let controlPresent = link.commServicePresent()
        azimuthUSBNotice(
            "[Azimuth USB] \(context): path=\(link.connectionDescription) "
                + "dataService=\(dataPresent) "
                + "controlService=\(Self.presenceText(controlPresent))"
        )
        guard includeDriverDetails else { return }

        do {
            if let status = try link.status() {
                azimuthUSBNotice("[Azimuth DEXT] \(context): \(status.text)")
            } else {
                azimuthUSBTrace("[Azimuth DEXT] \(context): status not applicable")
            }
        } catch {
            azimuthUSBNotice(
                "[Azimuth DEXT] \(context): status unavailable: \(Self.describe(error))"
            )
        }

        do {
            let entries = try link.dextLog()
            let newEntries: ArraySlice<AzimuthUSBDextLogEntry>
            if let lastLoggedDextEntry,
               let lastIndex = entries.lastIndex(of: lastLoggedDextEntry) {
                newEntries = entries[entries.index(after: lastIndex)...]
            } else {
                newEntries = entries[...]
            }
            if let newest = entries.last { lastLoggedDextEntry = newest }

            if entries.isEmpty {
                azimuthUSBTrace("[Azimuth DEXT] \(context): diagnostic ring empty")
            } else if newEntries.isEmpty {
                azimuthUSBTrace(
                    "[Azimuth DEXT] \(context): no new ring events "
                        + "(latestSequence=\(entries.last?.sequence ?? 0))"
                )
            } else {
                let entriesToLog = newEntries.suffix(Self.maximumLoggedDextEntries)
                let omittedCount = newEntries.count - entriesToLog.count
                azimuthUSBNotice(
                    omittedCount == 0
                        ? "[Azimuth DEXT] \(context): emitting \(entriesToLog.count) new ring event(s)"
                        : "[Azimuth DEXT] \(context): emitting last \(entriesToLog.count) "
                            + "of \(newEntries.count) unseen ring event(s)"
                )
                for entry in entriesToLog {
                    azimuthUSBNotice("[Azimuth DEXT] \(entry.text)")
                }
            }
        } catch {
            azimuthUSBNotice(
                "[Azimuth DEXT] \(context): ring unavailable: \(Self.describe(error))"
            )
        }
    }

    private func resumeAllReaders(with bytes: [UInt8]) {
        readers.forEach { $0.continuation.resume(returning: bytes) }
        readers.removeAll()
    }

    private func updateState(_ state: AzimuthRadioTransportState) {
        currentState = state
        stateContinuation.yield(state)
    }

    private static func describe(_ error: Error) -> String {
        switch error {
        case AzimuthUSBLinkError.unsupportedEnvironment(let reason):
            return reason
        case AzimuthUSBLinkError.serviceNotFound:
            return "TH-D75 USB service not found"
        case AzimuthUSBLinkError.ambiguousDevices(let paths):
            let names = paths.map { ($0 as NSString).lastPathComponent }
                .joined(separator: ", ")
            return "Multiple TH-D75 USB serial devices are attached (\(names)). "
                + "Choose an exact device path or disconnect all but one radio."
        case AzimuthUSBLinkError.openedDeviceIdentityUnstable(let path):
            return "The USB identity for \((path as NSString).lastPathComponent) changed while "
                + "Azimuth opened it. No radio operation was attempted; reconnect USB-C and retry."
        case AzimuthUSBLinkError.openFailed(let code):
            return "USB open failed: \(azimuthKernReturnString(code))"
        case AzimuthUSBLinkError.notOpen:
            return "USB link is not open"
        case AzimuthUSBLinkError.invalidTransferLength(let length):
            return "Invalid USB transfer length: \(length)"
        case AzimuthUSBLinkError.unsupportedBaudRate(let baud):
            return "Unsupported USB serial baud rate: \(baud)"
        case AzimuthUSBLinkError.backpressure:
            return "USB transmit queue is full"
        case AzimuthUSBLinkError.callFailed(let code):
            return "Driver call failed: \(azimuthKernReturnString(code))"
        case AzimuthUSBLinkError.systemCall(let operation, let code):
            return "\(operation) failed (errno \(code))"
        default:
            return String(describing: error)
        }
    }

    private static func presenceText(_ presence: Bool?) -> String {
        presence.map(String.init) ?? "not-applicable"
    }

    private static var missingServiceDescription: String {
        #if targetEnvironment(simulator)
        return "USBDriverKit does not run in Simulator. Run Azimuth on a physical M-series iPad."
        #elseif os(iOS)
        return "Azimuth cannot currently see a TH-D75 USB connection. Power on the radio, set Menu 980 to COM + AF/IF Output, and connect it with a data-capable USB-C cable. If those are already correct, open Azimuth Settings to verify that the TH-D75 Driver is enabled and no competing TH-D75 driver is active."
        #else
        return "No TH-D75 USB serial device was found. Set USB Function to COM + AF/IF Output, power the radio on, and connect it with a data-capable USB cable."
        #endif
    }

    private static var missingControlServiceDescription: String {
        "Azimuth found the TH-D75 data interface, but its CDC control interface did not become ready. Unplug and reconnect USB-C, then try again."
    }
}
