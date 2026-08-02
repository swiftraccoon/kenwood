// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

/// Verified radio-side facts for a prepared USB IF session.
struct IFDSPRadioModeStatus: Equatable, Sendable {
    let bandBFrequencyHz: UInt32
    let ifCenterHz: UInt32
}

/// Honest lifecycle of the radio configuration which supplies IF audio.
enum IFDSPRadioModeState: Equatable, Sendable {
    case inactive
    case preparing
    case active(IFDSPRadioModeStatus)
    case tuning(previous: IFDSPRadioModeStatus, requestedFrequencyHz: UInt32)
    case restoring(IFDSPRadioModeStatus?)
    /// `restorationPending` keeps conflicting controls disabled when the core
    /// could not verify every saved field.
    case failed(message: String, restorationPending: Bool)

    var activeStatus: IFDSPRadioModeStatus? {
        switch self {
        case .active(let status), .tuning(let status, _): return status
        case .restoring(let status): return status
        case .inactive, .preparing, .failed: return nil
        }
    }

    var reservesRadioState: Bool {
        switch self {
        case .active, .tuning, .restoring: return true
        case .failed(_, let restorationPending): return restorationPending
        case .inactive, .preparing: return false
        }
    }
}

/// Radio-side lifecycle boundary used before and after physical USB capture.
///
/// The caller must sequence `prepareIFDSPMode()` before starting the audio
/// stream and stop that stream before `restoreIFDSPMode()`.
@MainActor
protocol IFDSPModeControlling: AnyObject {
    var ifDSPModeState: IFDSPRadioModeState { get }

    @discardableResult
    func prepareIFDSPMode() async throws -> IFDSPRadioModeStatus

    @discardableResult
    func retuneIFDSP(to frequencyHz: UInt32) async throws -> IFDSPRadioModeStatus

    func restoreIFDSPMode() async throws
}

/// Preview/test default which never implies that a radio was configured.
@MainActor
final class UnavailableIFDSPModeController: IFDSPModeControlling {
    let ifDSPModeState = IFDSPRadioModeState.inactive

    @discardableResult
    func prepareIFDSPMode() async throws -> IFDSPRadioModeStatus {
        throw RadioControllerError.adapterUnavailable
    }

    @discardableResult
    func retuneIFDSP(to frequencyHz: UInt32) async throws -> IFDSPRadioModeStatus {
        throw RadioControllerError.adapterUnavailable
    }

    func restoreIFDSPMode() async throws {
        throw RadioControllerError.adapterUnavailable
    }
}
