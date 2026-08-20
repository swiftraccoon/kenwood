// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import OSLog

private let log = Logger(subsystem: "org.swiftraccoon.lodestar", category: "usb-serial")

/// iPad direct-radio transport over USB-C CDC, mediated by the
/// `LodestarDriver` DriverKit extension.
///
/// ## Architecture
///
/// ```
/// LodestarIPad.app ──IOServiceOpen/IOConnectCall*──▶ LodestarDriver.dext ──bulk IN/OUT──▶ TH-D75
///   (this actor over a USBSerialLink)                  (IOService over USBDriverKit)        interface 1 (CDC Data)
/// ```
///
/// The dext matches the radio's CDC **Data** interface (bInterfaceNumber
/// 1, class 0x0A, where the bulk endpoints live; the class 02/02
/// control interface has only an interrupt endpoint). Wire bytes above
/// this transport (CAT, MMDVM frames, MCP) are identical to the macOS
/// Bluetooth path; `MmdvmReader`, `RadioModeProber`, `McpSession`, and
/// `RelayCoordinator` reuse unchanged.
///
/// ## Doorbell + drain protocol
///
/// One async user-client completion stays armed as a one-shot doorbell.
/// The dext fires it on its RX buffer's empty→non-empty edge (or
/// immediately if armed while non-empty). On each ring this actor
/// re-arms FIRST, then drains `link.drain` until empty; the re-arm-first
/// order plus the immediate-fire-when-non-empty rule closes every race.
///
/// ## Platform notes
///
/// On iPadOS the dext ships inside the app bundle, is auto-discovered at
/// install, and must be enabled by the user in **Settings → General →
/// Drivers**. It launches on demand when the radio is plugged in.
/// DriverKit runs on M-series iPads only; on other devices
/// `availableDevices()` stays empty and the UI remains reflector-only.
public actor USBSerialTransport: RadioTransport {
    public let device: BluetoothDevice
    public nonisolated let pcOutputInterface: PcOutputInterface = .usb
    private let link: any USBSerialLink
    private var _state: RadioTransportState = .disconnected
    private let stateContinuation: AsyncStream<RadioTransportState>.Continuation
    public nonisolated let stateStream: AsyncStream<RadioTransportState>

    private var buffer: [UInt8] = []
    /// Parked readers, FIFO. Each keeps its own `maxBytes` (the
    /// delivery cap) and an ID so cancellation can target exactly one
    /// continuation: resuming *all* parked reads on one reader's
    /// cancellation would send the `[]` link-closed sentinel to
    /// unrelated readers.
    private var readContinuations:
        [(id: UInt64, maxBytes: Int, continuation: CheckedContinuation<[UInt8], Error>)] = []
    private var nextReadID: UInt64 = 0
    /// Set when the link reported teardown; reads return `[]` from then on.
    private var linkDown = false

    /// Write-backpressure retry budget: 20 × 10 ms = 200 ms worst case.
    private static let backpressureRetries = 20
    private static let backpressureDelayNs: UInt64 = 10_000_000

    public init(device: BluetoothDevice = .usbSynthetic, link: any USBSerialLink) {
        self.device = device
        self.link = link
        var cont: AsyncStream<RadioTransportState>.Continuation!
        self.stateStream = AsyncStream { c in cont = c }
        self.stateContinuation = cont
    }

    public var state: RadioTransportState { _state }

    public func open() async throws {
        updateState(.connecting)
        guard link.servicePresent() else {
            let reason = "No TH-D75 found over USB. Plug the radio into the "
                + "USB-C port, and check the driver is enabled in "
                + "Settings → General → Drivers."
            updateState(.failed(message: reason))
            throw RadioTransportError.openFailed(reason: reason)
        }
        do {
            try link.open()
            linkDown = false
            try armDoorbell()
            // Data may have been buffered before we armed: drain once now.
            try drainAll()
            updateState(.connected)
        } catch {
            // link.open() may have succeeded before a later step threw,
            // so release the user-client connection instead of leaking it.
            link.close()
            let reason = Self.describe(error)
            updateState(.failed(message: reason))
            throw RadioTransportError.openFailed(reason: reason)
        }
    }

    public func close() async {
        linkDown = true
        link.close()
        resumeParkedReads(with: [])
        updateState(.disconnected)
        stateContinuation.finish()
    }

    public func write(_ bytes: [UInt8]) async throws {
        guard case .connected = _state else {
            throw RadioTransportError.notConnected
        }
        for attempt in 0...Self.backpressureRetries {
            do {
                try link.write(bytes)
                return
            } catch USBLinkError.backpressure {
                if attempt == Self.backpressureRetries {
                    throw RadioTransportError.writeFailed(
                        reason: "dext write queue full (persistent backpressure)")
                }
                try await Task.sleep(nanoseconds: Self.backpressureDelayNs)
            } catch {
                let reason = Self.describe(error)
                log.error("write failed: \(reason)")
                // A hard user-client failure means the link is dead
                // (dext crashed, radio unplugged, connection severed),
                // so reflect that instead of lingering in a zombie
                // "connected" state with parked reads that never wake.
                handleLinkDrop()
                throw RadioTransportError.writeFailed(reason: reason)
            }
        }
        // Unreachable: the loop always returns or throws; this satisfies
        // the compiler's definite-return analysis.
        throw RadioTransportError.writeFailed(reason: "dext write queue full (persistent backpressure)")
    }

    public func read(maxBytes: Int) async throws -> [UInt8] {
        if !buffer.isEmpty {
            let n = min(maxBytes, buffer.count)
            let chunk = Array(buffer.prefix(n))
            buffer.removeFirst(n)
            return chunk
        }
        if linkDown { return [] }
        let id = nextReadID
        nextReadID += 1
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { c in
                readContinuations.append((id: id, maxBytes: maxBytes, continuation: c))
            }
        } onCancel: {
            Task { await self.cancelParkedRead(id: id) }
        }
    }

    // MARK: - Doorbell + drain

    private func armDoorbell() throws {
        try link.armDoorbell { [weak self] dataAvailable in
            Task { await self?.doorbellFired(dataAvailable: dataAvailable) }
        }
    }

    private func doorbellFired(dataAvailable: Bool) {
        guard dataAvailable else {
            handleLinkDrop()
            return
        }
        do {
            // Re-arm FIRST so no empty→non-empty edge is ever missed,
            // then drain everything currently buffered.
            try armDoorbell()
            try drainAll()
        } catch {
            handleLinkDrop()
        }
    }

    private func drainAll() throws {
        while true {
            let chunk = try link.drain(maxBytes: 4096)
            if chunk.isEmpty { break }
            buffer.append(contentsOf: chunk)
        }
        deliverToParkedReads()
    }

    private func deliverToParkedReads() {
        while !buffer.isEmpty, !readContinuations.isEmpty {
            let reader = readContinuations.removeFirst()
            let n = min(reader.maxBytes, buffer.count)
            let chunk = Array(buffer.prefix(n))
            buffer.removeFirst(n)
            reader.continuation.resume(returning: chunk)
        }
    }

    /// Cancellation resumes exactly the cancelled reader, never its
    /// siblings (the `[]` sentinel would read as "link closed" to them).
    private func cancelParkedRead(id: UInt64) {
        guard let idx = readContinuations.firstIndex(where: { $0.id == id }) else { return }
        let reader = readContinuations.remove(at: idx)
        reader.continuation.resume(returning: [])
    }

    private func handleLinkDrop() {
        guard !linkDown else { return }
        linkDown = true
        link.close()
        log.warning("USB link dropped (unplug or dext teardown)")
        resumeParkedReads(with: [])
        updateState(.disconnected)
    }

    private func resumeParkedReads(with chunk: [UInt8]) {
        for reader in readContinuations { reader.continuation.resume(returning: chunk) }
        readContinuations.removeAll()
    }

    private func updateState(_ new: RadioTransportState) {
        _state = new
        stateContinuation.yield(new)
    }

    private static func describe(_ error: Error) -> String {
        switch error {
        case USBLinkError.serviceNotFound:
            return "TH-D75 USB service not found (radio unplugged or driver disabled)."
        case USBLinkError.openFailed(let kern):
            return "Driver connection failed: \(kernReturnString(kern)). "
                + "Check the Lodestar driver is enabled in the Settings app."
        case USBLinkError.backpressure:
            return "Dext write queue full."
        case USBLinkError.notOpen:
            return "USB link is not open."
        case USBLinkError.callFailed(let kern):
            return "Driver call failed: \(kernReturnString(kern))."
        default:
            return String(describing: error)
        }
    }

    /// One-paste debugging: transport internals + dext counters + the
    /// dext's own event ring. Best-effort: every line that can't be
    /// fetched says why instead of vanishing.
    public func diagnosticsReport() -> String {
        var lines: [String] = []
        lines.append("transport: state=\(_state) buffered=\(buffer.count) "
            + "linkDown=\(linkDown) parkedReads=\(readContinuations.count)")
        do {
            if let s = try link.status() {
                lines.append(s.text)
            } else {
                lines.append("dext status: unsupported by this link")
            }
        } catch {
            lines.append("dext status: FAILED \(Self.describe(error))")
        }
        if let comm = link.commServicePresent() {
            lines.append("comm-interface driver registered: \(comm)")
        }
        do {
            let entries = try link.dextLog()
            lines.append("dext log (\(entries.count) events):")
            lines.append(contentsOf: entries.map { "  " + $0.text })
        } catch {
            lines.append("dext log: FAILED \(Self.describe(error))")
        }
        return lines.joined(separator: "\n")
    }
}

public extension BluetoothDevice {
    /// Synthetic descriptor for the USB path: one cable, one radio, no
    /// picker needed. IDs mirror the TH-D75 USB VID/PID.
    static let usbSynthetic = BluetoothDevice(
        id: "usb:2166:9023",
        name: "TH-D75 (USB-C)",
        address: "USB-CDC"
    )
}
