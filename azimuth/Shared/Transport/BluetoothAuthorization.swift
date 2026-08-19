// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

/// Authorization failures detected in the foreground Azimuth process before
/// a sandboxed helper is allowed to touch macOS Bluetooth services.
enum AzimuthBluetoothAuthorizationError: LocalizedError, Sendable, Equatable {
    case denied
    case restricted
    case bluetoothUnavailable
    case bluetoothPoweredOff
    case foregroundActivationRequired
    case authorizationTimedOut

    var errorDescription: String? {
        switch self {
        case .denied:
            "Bluetooth access is off for Azimuth. Open System Settings > Privacy & Security > Bluetooth, allow Azimuth, then refresh radio connections."
        case .restricted:
            "Bluetooth access is restricted for Azimuth by system policy. Ask the device administrator to allow Bluetooth, then refresh radio connections."
        case .bluetoothUnavailable:
            "Bluetooth is unavailable on this Mac. USB-C radio connections remain available."
        case .bluetoothPoweredOff:
            "Bluetooth is turned off. Turn it on in Control Center or System Settings, then refresh radio connections."
        case .foregroundActivationRequired:
            "Azimuth could not become the active app to request Bluetooth access. Bring Azimuth to the foreground, then refresh radio connections."
        case .authorizationTimedOut:
            "Azimuth did not receive a Bluetooth permission decision. Bring Azimuth to the foreground, respond to the macOS prompt, then refresh radio connections."
        }
    }
}

/// Foreground app-process gate for Bluetooth privacy authorization.
protocol AzimuthBluetoothAuthorizationProviding: Sendable {
    func ensureBluetoothAuthorization() async throws
}

/// Closure adapter used by focused tests without consulting system privacy
/// state or launching any native Bluetooth process.
struct AzimuthBluetoothAuthorizationBridge:
    AzimuthBluetoothAuthorizationProviding,
    Sendable
{
    typealias Authorize = @Sendable () async throws -> Void

    private let authorize: Authorize

    init(_ authorize: @escaping Authorize) {
        self.authorize = authorize
    }

    func ensureBluetoothAuthorization() async throws {
        try Task.checkCancellation()
        try await authorize()
        try Task.checkCancellation()
    }
}

#if os(macOS)

import AppKit
@preconcurrency import CoreBluetooth

/// Waits until Azimuth is actually active before any API which can trigger a
/// macOS privacy prompt is constructed. `NSApplication.activate()` is only a
/// request, so observing `isActive` is the boundary, not the method return.
struct AzimuthMacBluetoothForegroundActivation: Sendable {
    typealias IsActive = @MainActor @Sendable () -> Bool
    typealias RequestActivation = @MainActor @Sendable () -> Void
    typealias Pause = @Sendable () async throws -> Void

    private let isActive: IsActive
    private let requestActivation: RequestActivation
    private let pause: Pause
    private let maximumChecks: Int

    init(
        maximumChecks: Int = 100,
        isActive: @escaping IsActive = { NSApplication.shared.isActive },
        requestActivation: @escaping RequestActivation = {
            NSApplication.shared.activate()
        },
        pause: @escaping Pause = {
            try await Task.sleep(for: .milliseconds(50))
        }
    ) {
        self.maximumChecks = maximumChecks
        self.isActive = isActive
        self.requestActivation = requestActivation
        self.pause = pause
    }

    func ensureForeground() async throws {
        try Task.checkCancellation()
        if await isActive() { return }
        await requestActivation()
        for _ in 0..<maximumChecks {
            try Task.checkCancellation()
            if await isActive() { return }
            try await pause()
        }
        guard await isActive() else {
            throw AzimuthBluetoothAuthorizationError.foregroundActivationRequired
        }
    }
}

/// Requests Bluetooth privacy consent from the foreground Azimuth app.
///
/// The signed RFCOMM helper inherits the app sandbox, but it cannot present a
/// reliable first-use consent prompt from its short-lived child process. This
/// provider owns that prompt in the responsible GUI process and completes all
/// concurrent callers only after macOS reports a terminal authorization.
final class AzimuthMacBluetoothAuthorizationProvider:
    NSObject,
    AzimuthBluetoothAuthorizationProviding,
    CBCentralManagerDelegate,
    @unchecked Sendable
{
    static let shared = AzimuthMacBluetoothAuthorizationProvider()

    typealias ForegroundAuthorization = @Sendable () async throws -> Void
    typealias CurrentAuthorization = @Sendable () -> Result<Void, Error>?
    typealias IsForegroundActive = @MainActor @Sendable () -> Bool
    typealias ManagerFactory = @MainActor @Sendable (
        any CBCentralManagerDelegate
    ) -> CBCentralManager

    private final class Request: @unchecked Sendable {
        private let lock = NSLock()
        private var continuation: CheckedContinuation<Void, Error>?
        private var result: Result<Void, Error>?

        var isResolved: Bool {
            lock.withLock { result != nil }
        }

        func value() async throws {
            try await withCheckedThrowingContinuation { continuation in
                let completed = lock.withLock { () -> Result<Void, Error>? in
                    if let result { return result }
                    self.continuation = continuation
                    return nil
                }
                if let completed {
                    continuation.resume(with: completed)
                }
            }
        }

        func resolve(_ next: Result<Void, Error>) {
            let pendingContinuation = lock.withLock {
                guard result == nil else {
                    return nil as CheckedContinuation<Void, Error>?
                }
                result = next
                let pendingContinuation = self.continuation
                self.continuation = nil
                return pendingContinuation
            }
            pendingContinuation?.resume(with: next)
        }
    }

    private let lock = NSLock()
    private let authorizeFromForeground: ForegroundAuthorization
    private let currentAuthorization: CurrentAuthorization
    private let isForegroundActive: IsForegroundActive
    private let makeManager: ManagerFactory
    private var requests: [UUID: Request] = [:]
    private var manager: CBCentralManager?

    init(
        authorizeFromForeground: @escaping ForegroundAuthorization = {
            try await AzimuthMacBluetoothForegroundActivation().ensureForeground()
        },
        currentAuthorization: @escaping CurrentAuthorization = {
            AzimuthMacBluetoothAuthorizationProvider.authorizationResult(
                authorization: CBManager.authorization
            )
        },
        isForegroundActive: @escaping IsForegroundActive = {
            NSApplication.shared.isActive
        },
        makeManager: @escaping ManagerFactory = { delegate in
            CBCentralManager(
                delegate: delegate,
                queue: .main,
                options: [CBCentralManagerOptionShowPowerAlertKey: false]
            )
        }
    ) {
        self.authorizeFromForeground = authorizeFromForeground
        self.currentAuthorization = currentAuthorization
        self.isForegroundActive = isForegroundActive
        self.makeManager = makeManager
    }

    func ensureBluetoothAuthorization() async throws {
        try Task.checkCancellation()
        if let result = currentAuthorization() {
            return try result.get()
        }

        try await authorizeFromForeground()
        try Task.checkCancellation()
        if let result = currentAuthorization() {
            return try result.get()
        }

        let identifier = UUID()
        let request = Request()
        let timeout = Task { [weak self, request] in
            do {
                try await Task.sleep(for: .seconds(30))
            } catch {
                return
            }
            request.resolve(
                .failure(AzimuthBluetoothAuthorizationError.authorizationTimedOut)
            )
            self?.removeRequest(identifier: identifier)
        }
        defer { timeout.cancel() }
        try await withTaskCancellationHandler {
            try Task.checkCancellation()
            await register(request, identifier: identifier)
            try await request.value()
            try Task.checkCancellation()
        } onCancel: {
            request.resolve(.failure(CancellationError()))
            self.removeRequest(identifier: identifier)
        }
    }

    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        if let result = Self.authorizationResult(
            authorization: CBManager.authorization,
            centralState: central.state
        ) {
            finishAll(with: result)
        }
    }

    private func register(_ request: Request, identifier: UUID) async {
        guard !request.isResolved else { return }
        if let result = currentAuthorization() {
            request.resolve(result)
            return
        }
        let shouldStart = lock.withLock { () -> Bool in
            guard !request.isResolved else { return false }
            requests[identifier] = request
            return manager == nil
        }
        guard shouldStart else { return }
        await startManagerIfNeeded()
    }

    @MainActor
    private func startManagerIfNeeded() {
        if let result = currentAuthorization() {
            finishAll(with: result)
            return
        }
        let shouldCreate = lock.withLock {
            manager == nil && !requests.isEmpty
        }
        guard shouldCreate else { return }

        // Foreground state can change after the earlier asynchronous wait.
        // Recheck on the main actor immediately before constructing the object
        // that can trigger TCC; no main-actor work can interleave between this
        // check and the synchronous construction below.
        guard isForegroundActive() else {
            finishAll(
                with: .failure(
                    AzimuthBluetoothAuthorizationError.foregroundActivationRequired
                )
            )
            return
        }
        let created = makeManager(self)
        let retained = lock.withLock { () -> Bool in
            guard manager == nil, !requests.isEmpty else { return false }
            manager = created
            return true
        }
        if !retained {
            created.delegate = nil
        }
    }

    private func removeRequest(identifier: UUID) {
        let released = lock.withLock { () -> CBCentralManager? in
            requests.removeValue(forKey: identifier)
            guard requests.isEmpty else { return nil }
            let released = manager
            manager = nil
            return released
        }
        guard let released else { return }
        DispatchQueue.main.async {
            released.delegate = nil
        }
    }

    private func finishAll(with result: Result<Void, Error>) {
        let completed = lock.withLock { () -> ([Request], CBCentralManager?) in
            let completed = Array(requests.values)
            requests.removeAll()
            let released = manager
            manager = nil
            return (completed, released)
        }
        completed.1?.delegate = nil
        for request in completed.0 {
            request.resolve(result)
        }
    }

    static func authorizationResult(
        authorization: CBManagerAuthorization,
        centralState: CBManagerState? = nil
    ) -> Result<Void, Error>? {
        switch authorization {
        case .allowedAlways:
            return .success(())
        case .denied:
            return .failure(AzimuthBluetoothAuthorizationError.denied)
        case .restricted:
            return .failure(AzimuthBluetoothAuthorizationError.restricted)
        case .notDetermined:
            break
        @unknown default:
            return .failure(AzimuthBluetoothAuthorizationError.restricted)
        }
        guard let centralState else { return nil }
        switch centralState {
        case .unsupported:
            return .failure(AzimuthBluetoothAuthorizationError.bluetoothUnavailable)
        case .poweredOff:
            return .failure(AzimuthBluetoothAuthorizationError.bluetoothPoweredOff)
        case .unauthorized:
            return .failure(AzimuthBluetoothAuthorizationError.denied)
        case .unknown, .resetting, .poweredOn:
            return nil
        @unknown default:
            return .failure(AzimuthBluetoothAuthorizationError.bluetoothUnavailable)
        }
    }
}

#endif
