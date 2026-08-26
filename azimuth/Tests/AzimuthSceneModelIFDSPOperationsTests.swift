// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import XCTest
@testable import Azimuth

@MainActor
final class AzimuthSceneModelIFDSPOperationsTests: XCTestCase {
    func testNoUSBInputProofAfterPreparationRestoresWithoutStartingAudio() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events, transport: "Bluetooth")
        radio.currentIFDSPUSBInputProof = nil
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)

        await model.startIFDSP()

        XCTAssertEqual(events.values, ["radio.prepare", "radio.restore"])
        XCTAssertTrue(
            model.operationError?.contains(
                "could not prove that the USB-C CAT and audio interfaces belong"
            ) == true
        )
        XCTAssertNil(stream.lastInputProof)
        XCTAssertEqual(model.ifDSPModeState, .inactive)
    }

    func testBluetoothUSBMMDVMBlockOffersMenu650InspectionBeforeMutationOrAudio() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events, transport: "Bluetooth")
        radio.automaticIFDSPDVGatewayRecoveryAvailable = true
        radio.currentIFDSPUSBInputProof = nil
        let mode = IFDSPTestModeController(events: events)
        mode.prepareError = RadioControllerError.ifDspDVGatewayRecoveryRequired
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)

        await model.startIFDSP()

        XCTAssertEqual(events.values, ["radio.prepare"])
        XCTAssertEqual(
            model.ifDSPDVGatewayRecoveryAlert,
            .offer(automaticRecoveryAvailable: true)
        )
        XCTAssertNil(model.operationError)
        XCTAssertEqual(radio.disableDVGatewayCallCount, 0)
        XCTAssertNil(stream.lastInputProof)
        XCTAssertTrue(
            model.ifDSPDVGatewayRecoveryAlert?.message.contains(
                "IF-DSP needs USB-C CAT and audio control"
            ) == true
        )
        XCTAssertTrue(
            model.ifDSPDVGatewayRecoveryAlert?.message.contains(
                "set it to Off only if needed"
            ) == true
        )
        XCTAssertFalse(
            model.ifDSPDVGatewayRecoveryAlert?.message.contains(
                "Menu 650 is set"
            ) == true
        )
        XCTAssertTrue(
            model.ifDSPDVGatewayRecoveryAlert?.message.contains(
                "resets the radio’s CAT and USB interfaces even when no setting write is needed"
            ) == true
        )
    }

    func testProvedUSBAudioPreflightRunsBeforeRadioPreparation() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events, transport: "USB-C")
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)

        await model.startIFDSP()

        XCTAssertEqual(
            events.values,
            ["audio.preflight", "radio.prepare", "audio.start"]
        )
        XCTAssertEqual(stream.lastInputProof, .testProof)
        XCTAssertTrue(model.ifDSPState.isStreaming)
        XCTAssertNil(model.ifDSPDVGatewayRecoveryAlert)
        guard case .connected(_, let transport) = model.radioState.connection else {
            return XCTFail("IF-DSP should remain connected through proved USB-C CAT")
        }
        XCTAssertEqual(transport, "USB-C")
    }

    func testApprovedDVGatewayRecoveryReconnectsBeforeResumingIFDSPOnce() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events, transport: "Bluetooth")
        radio.automaticIFDSPDVGatewayRecoveryAvailable = true
        radio.currentIFDSPUSBInputProof = nil
        radio.inputProofAfterDVGatewayRecovery = .testProof
        let mode = IFDSPTestModeController(events: events)
        mode.prepareError = RadioControllerError.ifDspDVGatewayRecoveryRequired
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        await model.startIFDSP()
        mode.prepareError = nil
        events.values.removeAll()

        await model.inspectDVGatewayAndStartIFDSP()

        XCTAssertEqual(
            events.values,
            [
                "radio.disable-dv-gateway",
                "audio.preflight",
                "radio.prepare",
                "audio.start",
            ]
        )
        XCTAssertEqual(radio.disableDVGatewayCallCount, 1)
        XCTAssertNil(model.ifDSPDVGatewayRecoveryAlert)
        XCTAssertTrue(model.ifDSPState.isStreaming)
        XCTAssertEqual(stream.lastInputProof, .testProof)
        guard case .connected(_, let transport) = model.radioState.connection else {
            return XCTFail("Approved recovery should leave CAT on the proved USB-C endpoint")
        }
        XCTAssertEqual(transport, "USB-C")
    }

    func testDecliningDVGatewayRecoveryLeavesRadioAndAudioUntouched() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events, transport: "Bluetooth")
        radio.automaticIFDSPDVGatewayRecoveryAvailable = true
        radio.currentIFDSPUSBInputProof = nil
        let mode = IFDSPTestModeController(events: events)
        mode.prepareError = RadioControllerError.ifDspDVGatewayRecoveryRequired
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        await model.startIFDSP()
        events.values.removeAll()

        model.dismissIFDSPDVGatewayRecoveryAlert()

        XCTAssertEqual(events.values, [])
        XCTAssertEqual(radio.disableDVGatewayCallCount, 0)
        XCTAssertEqual(model.ifDSPState, .idle)
    }

    func testRejectedDVGatewayRecoveryDoesNotOfferTheSameActionAgain() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events, transport: "Bluetooth")
        radio.automaticIFDSPDVGatewayRecoveryAvailable = true
        radio.currentIFDSPUSBInputProof = nil
        radio.disableDVGatewayError = RadioControllerError.capabilityUnavailable(
            "The attached USB-C endpoint could not be proved. No radio setting was changed."
        )
        let mode = IFDSPTestModeController(events: events)
        mode.prepareError = RadioControllerError.ifDspDVGatewayRecoveryRequired
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        await model.startIFDSP()
        events.values.removeAll()

        await model.inspectDVGatewayAndStartIFDSP()

        XCTAssertEqual(events.values, ["radio.disable-dv-gateway"])
        XCTAssertEqual(radio.disableDVGatewayCallCount, 1)
        XCTAssertEqual(
            model.ifDSPDVGatewayRecoveryAlert,
            .failed(
                message: "The attached USB-C endpoint could not be proved. No radio setting was changed."
            )
        )
        XCTAssertFalse(
            model.ifDSPDVGatewayRecoveryAlert?.automaticRecoveryAvailable == true
        )
        XCTAssertEqual(
            model.ifDSPDVGatewayRecoveryAlert?.dismissalButtonTitle,
            "Dismiss"
        )
    }

    func testCancelledDVGatewayInspectionDoesNotRelaunchOffer() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events, transport: "Bluetooth")
        radio.automaticIFDSPDVGatewayRecoveryAvailable = true
        radio.currentIFDSPUSBInputProof = nil
        radio.waitForDVGatewayRecoveryCancellation = true
        let mode = IFDSPTestModeController(events: events)
        mode.prepareError = RadioControllerError.ifDspDVGatewayRecoveryRequired
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        await model.startIFDSP()

        let recovery = Task { await model.inspectDVGatewayAndStartIFDSP() }
        await assertEventually {
            radio.disableDVGatewayCallCount == 1
        }
        recovery.cancel()
        await recovery.value

        XCTAssertNil(model.ifDSPDVGatewayRecoveryAlert)
        XCTAssertEqual(radio.disableDVGatewayCallCount, 1)
        XCTAssertNil(stream.lastInputProof)

        await model.startIFDSP()

        XCTAssertEqual(
            model.ifDSPDVGatewayRecoveryAlert,
            .offer(automaticRecoveryAvailable: true)
        )
        XCTAssertEqual(radio.disableDVGatewayCallCount, 1)
    }

    func testPostAttemptFailureRequiresAFreshIFDSPPreflightBeforeAnotherApproval() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events, transport: "Bluetooth")
        radio.automaticIFDSPDVGatewayRecoveryAvailable = true
        radio.currentIFDSPUSBInputProof = nil
        radio.disableDVGatewayError = RadioControllerError.operationFailed(
            "The radio restarted, but its Bluetooth CAT connection did not return."
        )
        let mode = IFDSPTestModeController(events: events)
        mode.prepareError = RadioControllerError.ifDspDVGatewayRecoveryRequired
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        await model.startIFDSP()

        await model.inspectDVGatewayAndStartIFDSP()

        XCTAssertEqual(
            model.ifDSPDVGatewayRecoveryAlert,
            .failed(
                message: "The radio restarted, but its Bluetooth CAT connection did not return."
            )
        )
        XCTAssertFalse(
            model.ifDSPDVGatewayRecoveryAlert?.automaticRecoveryAvailable == true
        )

        model.dismissIFDSPDVGatewayRecoveryAlert()
        await model.startIFDSP()

        XCTAssertEqual(
            model.ifDSPDVGatewayRecoveryAlert,
            .offer(automaticRecoveryAvailable: true)
        )
        XCTAssertEqual(radio.disableDVGatewayCallCount, 1)
    }

    func testStartPreflightsPhysicalAudioBeforePreparingRadio() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)

        await model.startIFDSP()

        XCTAssertEqual(
            events.values,
            ["audio.preflight", "radio.prepare", "audio.start"]
        )
        XCTAssertEqual(stream.lastInputProof, .testProof)
        XCTAssertEqual(model.ifDSPModeState, .active(.testStatus))
        XCTAssertTrue(model.ifDSPState.isStreaming)
        XCTAssertNil(model.operationError)
    }

    func testMissingUSBAudioDoesNotPrepareOrRestoreTheRadio() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(
            events: events,
            startResult: .testStreaming,
            preflightFailure: .waitingForUSBAudio(
                availableInputs: ["iPad Microphone"]
            )
        )
        let model = makeModel(radio: radio, mode: mode, stream: stream)

        await model.startIFDSP()

        XCTAssertEqual(
            events.values,
            ["audio.preflight", "audio.stop"]
        )
        XCTAssertEqual(model.ifDSPModeState, .inactive)
        XCTAssertEqual(model.ifDSPState, .idle)
        XCTAssertTrue(model.operationError?.contains("TH-D75 USB audio input was not available") == true)
    }

    func testCancellationAfterAudioPreflightCannotPrepareTheRadio() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        stream.waitForPreflightCancellation = true
        let model = makeModel(radio: radio, mode: mode, stream: stream)

        let start = Task { await model.startIFDSP() }
        await assertEventually {
            events.values.contains("audio.preflight")
        }
        start.cancel()
        await start.value

        XCTAssertEqual(events.values, ["audio.preflight", "audio.stop"])
        XCTAssertFalse(events.values.contains("radio.prepare"))
        XCTAssertEqual(model.ifDSPModeState, .inactive)
        XCTAssertNil(model.operationError)
    }

    func testCancellationAfterRadioPreparationRestoresBeforeAudioStart() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        mode.prepareAction = {
            withUnsafeCurrentTask { task in
                task?.cancel()
            }
        }
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)

        await model.startIFDSP()

        XCTAssertEqual(
            events.values,
            ["audio.preflight", "radio.prepare", "audio.stop", "radio.restore"]
        )
        XCTAssertFalse(events.values.contains("audio.start"))
        XCTAssertEqual(model.ifDSPModeState, .inactive)
    }

    func testCancellationAfterAudioStartStopsCaptureAndRestoresRadio() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        stream.cancelDuringStart = true
        let model = makeModel(radio: radio, mode: mode, stream: stream)

        await model.startIFDSP()

        XCTAssertEqual(
            events.values,
            [
                "audio.preflight",
                "radio.prepare",
                "audio.start",
                "audio.stop",
                "radio.restore",
            ]
        )
        XCTAssertEqual(model.ifDSPState, .idle)
        XCTAssertEqual(model.ifDSPModeState, .inactive)
    }

    func testUserStopEndsAudioBeforeRestoringEveryRadioField() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        await model.startIFDSP()
        events.values.removeAll()

        await model.stopIFDSP()

        XCTAssertEqual(events.values, ["audio.stop", "radio.restore"])
        XCTAssertEqual(model.ifDSPModeState, .inactive)
        XCTAssertEqual(model.ifDSPState, .idle)
    }

    func testUnexpectedAudioRouteLossAutomaticallyRestoresRadio() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        model.activate()
        await model.startIFDSP()
        events.values.removeAll()

        stream.publish(
            .paused(reason: "The TH-D75 USB audio input disconnected.", lastFrame: nil)
        )

        await assertEventually {
            model.ifDSPModeState == .inactive && model.ifDSPState == .idle
        }
        XCTAssertEqual(events.values, ["audio.stop", "radio.restore"])
        XCTAssertTrue(model.operationError?.contains("USB audio input disconnected") == true)
    }

    func testRetuneFailureStopsAudioAndRestoresSavedRadioState() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        await model.startIFDSP()
        mode.retuneError = .retuneRejected
        events.values.removeAll()

        await model.retuneIFDSP(to: 433_925_000)

        XCTAssertEqual(
            events.values,
            ["radio.retune.433925000", "audio.stop"]
        )
        XCTAssertEqual(model.ifDSPModeState, .inactive)
        XCTAssertEqual(model.ifDSPState, .idle)
        XCTAssertTrue(model.operationError?.contains("retune was rejected") == true)
    }

    func testRetuneFailureWithDegradedReservationRetriesRestoration() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        await model.startIFDSP()
        mode.retuneError = .retuneRejected
        mode.retuneRestorationPending = true
        events.values.removeAll()

        await model.retuneIFDSP(to: 433_925_000)

        XCTAssertEqual(
            events.values,
            ["radio.retune.433925000", "audio.stop", "radio.restore"]
        )
        XCTAssertEqual(model.ifDSPModeState, .inactive)
        XCTAssertEqual(model.ifDSPState, .idle)
    }

    func testRouteLossDuringSuccessfulRetuneStopsIFAndRestoresRadio() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        model.activate()
        await model.startIFDSP()
        mode.retuneDelayNanoseconds = 75_000_000
        events.values.removeAll()

        let retune = Task { await model.retuneIFDSP(to: 433_925_000) }
        await assertEventually {
            events.values.contains("radio.retune.433925000")
        }
        stream.publish(
            .paused(reason: "The TH-D75 USB audio input disconnected.", lastFrame: nil)
        )
        await retune.value

        XCTAssertEqual(
            events.values,
            ["radio.retune.433925000", "audio.stop", "radio.restore"]
        )
        XCTAssertEqual(model.ifDSPModeState, .inactive)
        XCTAssertEqual(model.ifDSPState, .idle)
        XCTAssertTrue(model.operationError?.contains("USB audio input disconnected") == true)
    }

    func testFailedRestorationStaysReservedUntilExplicitRetrySucceeds() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        await model.startIFDSP()
        mode.restoreError = .restoreRejected
        events.values.removeAll()

        await model.stopIFDSP()

        XCTAssertEqual(events.values, ["audio.stop", "radio.restore"])
        XCTAssertTrue(model.ifDSPModeState.reservesRadioState)
        XCTAssertTrue(model.operationError?.contains("not fully restored") == true)

        mode.restoreError = nil
        events.values.removeAll()
        await model.stopIFDSP()

        XCTAssertEqual(events.values, ["audio.stop", "radio.restore"])
        XCTAssertEqual(model.ifDSPModeState, .inactive)
    }

    func testPrepareFailureWithFailedRestorationReportsOneOutcomeAndKeepsRetryState() async throws {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        mode.prepareError = IFDSPTestError.prepareRejected
        mode.prepareRestorationPending = true
        mode.restoreError = .restoreRejected

        await model.startIFDSP()

        XCTAssertEqual(
            events.values,
            ["audio.preflight", "radio.prepare", "audio.stop", "radio.restore"]
        )
        XCTAssertEqual(model.ifDSPState, .idle)
        guard case .failed(_, let restorationPending) = model.ifDSPModeState else {
            return XCTFail("A failed restore must keep the IF-DSP radio state reserved")
        }
        XCTAssertTrue(restorationPending, "The workspace must continue to offer Retry Restore")
        XCTAssertTrue(model.ifDSPModeState.reservesRadioState)

        let error = try XCTUnwrap(model.operationError)
        XCTAssertEqual(
            error,
            "IF-DSP couldn’t start, and Azimuth still could not verify the saved radio state. "
                + "The saved radio state could not be restored. Return the radio to normal "
                + "dual-band VFO operation, then choose Retry Restore before starting IF-DSP again."
        )
        XCTAssertFalse(error.contains("The IF tap could not be prepared."))
        XCTAssertFalse(error.contains("Radio restoration also failed"))
        XCTAssertTrue(error.contains("Retry Restore"))
    }

    func testPrepareFailureWithSuccessfulRestorationReportsTheCompletedOutcome() async throws {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        mode.prepareError = IFDSPTestError.prepareRejected
        mode.prepareRestorationPending = true

        await model.startIFDSP()

        XCTAssertEqual(
            events.values,
            ["audio.preflight", "radio.prepare", "audio.stop", "radio.restore"]
        )
        XCTAssertEqual(model.ifDSPModeState, .inactive)
        XCTAssertFalse(model.ifDSPModeState.reservesRadioState)
        XCTAssertEqual(
            model.operationError,
            "IF-DSP couldn’t start, but Azimuth restored and verified the saved radio state. "
                + "Correct the radio mode or connection problem, then try again."
        )
        XCTAssertFalse(model.operationError?.contains("could not restore") == true)
        XCTAssertFalse(model.operationError?.contains("Retry Restore") == true)
    }

    func testPrepareFailureWithPostRestoreRefreshFailureDoesNotOfferRetryRestore() async throws {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        mode.prepareError = IFDSPTestError.prepareRejected
        mode.prepareRestorationPending = true
        mode.restoreError = .restoreRejected
        mode.restoreCompletesBeforeError = true

        await model.startIFDSP()

        XCTAssertEqual(
            events.values,
            ["audio.preflight", "radio.prepare", "audio.stop", "radio.restore"]
        )
        XCTAssertEqual(model.ifDSPModeState, .inactive)
        XCTAssertFalse(model.ifDSPModeState.reservesRadioState)
        let error = try XCTUnwrap(model.operationError)
        XCTAssertTrue(error.contains("restored and verified the saved radio state"))
        XCTAssertTrue(error.contains("settings/screen workspace could not be refreshed"))
        XCTAssertTrue(error.contains("Reconnect before trying again"))
        XCTAssertFalse(error.contains("still could not verify"))
        XCTAssertFalse(error.contains("Retry Restore"))
    }

    func testBackgroundRestoresBeforeDisconnectAndDoesNotRestartIFMode() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        model.activate()
        await model.startIFDSP()
        events.values.removeAll()

        await model.handleScenePhaseBackground().value

        XCTAssertEqual(
            events.values,
            ["audio.stop", "radio.restore", "connection.disconnect"]
        )
        XCTAssertEqual(model.ifDSPModeState, .inactive)

        await model.handleScenePhaseActive().value

        XCTAssertEqual(events.values.last, "connection.connect")
        XCTAssertEqual(events.values.filter { $0 == "radio.prepare" }.count, 0)
        XCTAssertTrue(model.radioState.connection.isConnected)
        XCTAssertEqual(model.ifDSPState, .idle)
    }

    func testBackgroundRestoreFailureDisconnectsButDoesNotReconnectOrHideWarning() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        model.activate()
        await model.startIFDSP()
        mode.restoreError = .restoreRejected
        events.values.removeAll()

        await model.handleScenePhaseBackground().value

        XCTAssertEqual(
            events.values,
            ["audio.stop", "radio.restore", "connection.disconnect"]
        )
        XCTAssertTrue(model.operationError?.contains("not fully restored") == true)

        await model.handleScenePhaseActive().value

        XCTAssertFalse(events.values.contains("connection.connect"))
        XCTAssertEqual(model.radioState.connection, .disconnected)
        XCTAssertTrue(model.operationError?.contains("not fully restored") == true)
    }

    func testExplicitDisconnectAbortsWhenRadioRestorationIsStillPending() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        await model.startIFDSP()
        mode.restoreError = .restoreRejected
        events.values.removeAll()

        await model.disconnectRadio()

        XCTAssertEqual(events.values, ["audio.stop", "radio.restore"])
        XCTAssertTrue(model.radioState.connection.isConnected)
        XCTAssertTrue(model.ifDSPModeState.reservesRadioState)
        XCTAssertTrue(model.operationError?.contains("not fully restored") == true)
    }

    func testRestoredRadioButWorkspaceRefreshFailureUsesAccurateMessage() async {
        let events = IFDSPTestEvents()
        let radio = IFDSPTestRadioController(events: events)
        let mode = IFDSPTestModeController(events: events)
        let stream = IFDSPTestStream(events: events, startResult: .testStreaming)
        let model = makeModel(radio: radio, mode: mode, stream: stream)
        await model.startIFDSP()
        mode.restoreError = .restoreRejected
        mode.restoreCompletesBeforeError = true
        events.values.removeAll()

        await model.stopIFDSP()

        XCTAssertEqual(events.values, ["audio.stop", "radio.restore"])
        XCTAssertEqual(model.ifDSPModeState, .inactive)
        XCTAssertTrue(model.operationError?.contains("radio state was restored") == true)
        XCTAssertTrue(model.operationError?.contains("workspace could not be refreshed") == true)
        XCTAssertFalse(model.operationError?.contains("not fully restored") == true)
    }

    private func makeModel(
        radio: IFDSPTestRadioController,
        mode: IFDSPTestModeController,
        stream: IFDSPTestStream
    ) -> AzimuthSceneModel {
        AzimuthSceneModel(
            radioController: radio,
            catalogProvider: PreviewRadioSettingCatalogProvider(),
            assistantPlanner: IFDSPTestPlanner(),
            ifDSPStream: stream,
            ifDSPModeController: mode,
            initialCatalog: .designPreview
        )
    }

    private func assertEventually(
        timeout: TimeInterval = 1,
        condition: @escaping @MainActor () -> Bool,
        file: StaticString = #filePath,
        line: UInt = #line
    ) async {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition(), Date() < deadline {
            try? await Task.sleep(nanoseconds: 5_000_000)
        }
        XCTAssertTrue(condition(), "Timed out waiting for IF-DSP cleanup", file: file, line: line)
    }
}

@MainActor
private final class IFDSPTestEvents {
    var values: [String] = []
}

@MainActor
private final class IFDSPTestModeController: IFDSPModeControlling {
    private(set) var ifDSPModeState: IFDSPRadioModeState = .inactive
    private let events: IFDSPTestEvents
    var prepareError: (any Error)?
    var prepareAction: (() -> Void)?
    var prepareRestorationPending = false
    var retuneError: IFDSPTestError?
    var retuneRestorationPending = false
    var retuneDelayNanoseconds: UInt64 = 0
    var restoreError: IFDSPTestError?
    var restoreCompletesBeforeError = false

    init(events: IFDSPTestEvents) {
        self.events = events
    }

    func prepareIFDSPMode() async throws -> IFDSPRadioModeStatus {
        prepareAction?()
        events.values.append("radio.prepare")
        if let prepareError {
            ifDSPModeState = .failed(
                message: prepareError.localizedDescription,
                restorationPending: prepareRestorationPending
            )
            throw prepareError
        }
        ifDSPModeState = .active(.testStatus)
        return .testStatus
    }

    func retuneIFDSP(to frequencyHz: UInt32) async throws -> IFDSPRadioModeStatus {
        events.values.append("radio.retune.\(frequencyHz)")
        if retuneDelayNanoseconds > 0 {
            try await Task.sleep(nanoseconds: retuneDelayNanoseconds)
        }
        if let retuneError {
            ifDSPModeState = retuneRestorationPending
                ? .failed(message: retuneError.localizedDescription, restorationPending: true)
                : .inactive
            throw retuneError
        }
        let status = IFDSPRadioModeStatus(
            bandBFrequencyHz: frequencyHz,
            ifCenterHz: 12_000
        )
        ifDSPModeState = .active(status)
        return status
    }

    func restoreIFDSPMode() async throws {
        events.values.append("radio.restore")
        if let restoreError {
            ifDSPModeState = restoreCompletesBeforeError
                ? .inactive
                : .failed(message: restoreError.localizedDescription, restorationPending: true)
            throw restoreError
        }
        ifDSPModeState = .inactive
    }
}

@MainActor
private final class IFDSPTestStream: IFDSPLiveStreaming {
    private(set) var currentState: IFDSPLiveStreamState = .idle
    private(set) var configuration: IFDSPConfiguration = .standard
    let monitoringState = IFDSPMonitoringState.unavailable(reason: "Test monitoring is off.")
    let updates: AsyncStream<IFDSPLiveStreamState>

    private let continuation: AsyncStream<IFDSPLiveStreamState>.Continuation
    private let events: IFDSPTestEvents
    private let startResult: IFDSPLiveStreamState
    private let preflightFailure: IFDSPLiveStreamState?
    private var preparedInput: IFDSPPreparedAudioInput?
    private(set) var lastInputProof: IFDSPUSBInputProof?
    var waitForPreflightCancellation = false
    var cancelDuringStart = false

    init(
        events: IFDSPTestEvents,
        startResult: IFDSPLiveStreamState,
        preflightFailure: IFDSPLiveStreamState? = nil
    ) {
        self.events = events
        self.startResult = startResult
        self.preflightFailure = preflightFailure
        let stream = AsyncStream.makeStream(
            of: IFDSPLiveStreamState.self,
            bufferingPolicy: .bufferingNewest(16)
        )
        updates = stream.stream
        continuation = stream.continuation
        continuation.yield(currentState)
    }

    func preflight(
        inputProof: IFDSPUSBInputProof
    ) async -> IFDSPPreparedAudioInput? {
        lastInputProof = inputProof
        events.values.append("audio.preflight")
        if waitForPreflightCancellation {
            do {
                try await Task.sleep(for: .seconds(60))
            } catch is CancellationError {
                return nil
            } catch {
                return nil
            }
        }
        if let preflightFailure {
            publish(preflightFailure)
            return nil
        }
        let preparedInput = IFDSPPreparedAudioInput()
        self.preparedInput = preparedInput
        publish(.starting(routeName: "TH-D75 USB Audio"))
        return preparedInput
    }

    func start(preparedInput: IFDSPPreparedAudioInput) async {
        guard self.preparedInput == preparedInput else {
            publish(
                .failed(
                    message: "The prepared test audio input expired.",
                    lastFrame: nil
                )
            )
            return
        }
        self.preparedInput = nil
        events.values.append("audio.start")
        publish(startResult)
        if cancelDuringStart {
            withUnsafeCurrentTask { task in
                task?.cancel()
            }
        }
    }

    func stop() {
        preparedInput = nil
        events.values.append("audio.stop")
        publish(.idle)
    }

    func setConfiguration(_ configuration: IFDSPConfiguration) async {
        self.configuration = configuration
    }

    func publish(_ state: IFDSPLiveStreamState) {
        currentState = state
        continuation.yield(state)
    }
}

@MainActor
private final class IFDSPTestRadioController: RadioControlling {
    private(set) var currentState: RadioWorkspaceState
    let updates: AsyncStream<RadioWorkspaceState>

    private let continuation: AsyncStream<RadioWorkspaceState>.Continuation
    private let events: IFDSPTestEvents
    var automaticIFDSPDVGatewayRecoveryAvailable = false
    var currentIFDSPUSBInputProof: IFDSPUSBInputProof? = .testProof
    var inputProofAfterDVGatewayRecovery: IFDSPUSBInputProof?
    var disableDVGatewayError: (any Error)?
    var waitForDVGatewayRecoveryCancellation = false
    private(set) var disableDVGatewayCallCount = 0

    init(events: IFDSPTestEvents, transport: String = "USB-C") {
        self.events = events
        currentState = Self.connectedState(transport: transport)
        let stream = AsyncStream.makeStream(
            of: RadioWorkspaceState.self,
            bufferingPolicy: .bufferingNewest(8)
        )
        updates = stream.stream
        continuation = stream.continuation
        continuation.yield(currentState)
    }

    func connect() async throws {
        events.values.append("connection.connect")
        currentState = Self.connectedState(transport: "USB-C")
        continuation.yield(currentState)
    }

    func disconnect() async {
        events.values.append("connection.disconnect")
        currentState = .disconnected
        continuation.yield(currentState)
    }

    func publishConnected(transport: String) {
        currentState = Self.connectedState(transport: transport)
        continuation.yield(currentState)
    }

    func disableDVGatewayAndReconnectForIFDSP() async throws {
        disableDVGatewayCallCount += 1
        events.values.append("radio.disable-dv-gateway")
        if waitForDVGatewayRecoveryCancellation {
            do {
                try await Task.sleep(for: .seconds(60))
            } catch {
                throw CancellationError()
            }
        }
        if let disableDVGatewayError { throw disableDVGatewayError }
        if let inputProofAfterDVGatewayRecovery {
            currentIFDSPUSBInputProof = inputProofAfterDVGatewayRecovery
            publishConnected(transport: "USB-C")
        }
    }

    func refreshScreen() async throws {}
    func refreshSettings() async throws {}
    func press(_ key: RadioFrontPanelKey) async throws {}

    func applySettings(
        _ changes: [ValidatedRadioSettingChange],
        progress: @escaping @MainActor @Sendable (RadioSettingApplyProgress) -> Void
    ) async throws -> RadioSettingApplyReport {
        RadioSettingApplyReport(results: [])
    }

    private static func connectedState(transport: String) -> RadioWorkspaceState {
        RadioWorkspaceState(
            connection: .connected(device: "TH-D75", transport: transport),
            capabilities: RadioCapabilities(
                screenStreaming: .available,
                frontPanelControl: .available,
                settingRead: .available,
                settingWrite: .available
            ),
            screenFrame: nil,
            telemetry: .unavailable,
            settingValues: [:],
            lastScreenError: nil
        )
    }
}

@MainActor
private final class IFDSPTestPlanner: AssistantPlanning {
    let availability = AssistantAvailability.unavailable(reason: "Not used by IF-DSP tests.")

    func propose(
        request: String,
        catalog: RadioSettingCatalog,
        currentValues: [String: ProposedSettingValue]
    ) async throws -> AssistantPlan {
        throw IFDSPTestError.unexpectedAssistant
    }
}

private enum IFDSPTestError: LocalizedError {
    case prepareRejected
    case retuneRejected
    case restoreRejected
    case unexpectedAssistant

    var errorDescription: String? {
        switch self {
        case .prepareRejected:
            "The IF tap could not be prepared."
        case .retuneRejected:
            "The retune was rejected."
        case .restoreRejected:
            "The saved radio state could not be restored."
        case .unexpectedAssistant:
            "The IF-DSP test invoked the Assistant unexpectedly."
        }
    }
}

private extension IFDSPRadioModeStatus {
    static let testStatus = IFDSPRadioModeStatus(
        bandBFrequencyHz: 145_500_000,
        ifCenterHz: 12_000
    )
}

private extension IFDSPUSBInputProof {
    static let testProof = try! IFDSPUSBInputProof(
        catSerialNumber: "C3C10368",
        macOSUSBDeviceRegistryEntryID: nil
    )
}

private extension IFDSPLiveStreamState {
    static let testStreaming = IFDSPLiveStreamState.streaming(
        route: IFDSPInputRoute(
            name: "TH-D75 USB Audio",
            kind: .usbAudio,
            sourceSampleRate: 48_000,
            sourceChannelCount: 1
        ),
        frame: nil
    )
}
