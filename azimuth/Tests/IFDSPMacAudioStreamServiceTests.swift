// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

#if os(macOS)

import CoreAudio
import XCTest
@testable import Azimuth

@MainActor
final class IFDSPMacAudioStreamServiceTests: XCTestCase {
    func testTahoeUIDLocationResolvesObservedADCStreamToExactUSBRegistryEntry() {
        let expected = makeUSBIdentity(registryEntryID: 1_042)
        let descriptor = IFDSPMacUSBDeviceDescriptor(
            identity: expected,
            serialNumber: nil,
            locationID: 0x0010_0000
        )

        let resolved = IFDSPAppleUSBAudioUIDResolver.identity(
            for: "AppleUSBAudioEngine:JVCKENWOOD:TH_D75:100000:3",
            among: [descriptor]
        )

        XCTAssertEqual(resolved, expected)
    }

    func testTahoeUIDResolverDoesNotDependOnNormalizedProductText() {
        let expected = makeUSBIdentity(registryEntryID: 1_042)
        let descriptor = IFDSPMacUSBDeviceDescriptor(
            identity: expected,
            serialNumber: nil,
            locationID: 0x0010_0000
        )

        XCTAssertEqual(
            IFDSPAppleUSBAudioUIDResolver.identity(
                for: "AppleUSBAudioEngine:JVCKENWOOD:TH_D75:100000:3",
                among: [descriptor]
            ),
            expected
        )
        XCTAssertEqual(
            IFDSPAppleUSBAudioUIDResolver.identity(
                for: "AppleUSBAudioEngine:JVCKENWOOD:any-name:100000:3",
                among: [descriptor]
            ),
            expected
        )
        XCTAssertNil(
            IFDSPAppleUSBAudioUIDResolver.identity(
                for: "AppleUSBAudioEngine:JVCKENWOOD:any-name:200000:3",
                among: [descriptor]
            )
        )
    }

    func testTahoeUIDResolverFailsClosedForAmbiguousLocationOrMalformedUID() {
        let first = IFDSPMacUSBDeviceDescriptor(
            identity: makeUSBIdentity(registryEntryID: 1_041),
            serialNumber: nil,
            locationID: 0x0010_0000
        )
        let second = IFDSPMacUSBDeviceDescriptor(
            identity: makeUSBIdentity(registryEntryID: 1_042),
            serialNumber: nil,
            locationID: 0x0010_0000
        )

        XCTAssertNil(
            IFDSPAppleUSBAudioUIDResolver.identity(
                for: "AppleUSBAudioEngine:JVCKENWOOD:TH_D75:100000:3",
                among: [first, second]
            )
        )
        XCTAssertNil(
            IFDSPAppleUSBAudioUIDResolver.identity(
                for: "AppleUSBAudioEngine:JVCKENWOOD:TH_D75:100000:not-an-interface",
                among: [first]
            )
        )
    }

    func testTahoeUIDResolverAcceptsEitherDocumentedSerialOrLocationIdentity() {
        let expected = makeUSBIdentity(registryEntryID: 1_042)
        let descriptor = IFDSPMacUSBDeviceDescriptor(
            identity: expected,
            serialNumber: "RADIO-AUDIO-1",
            locationID: 0x0010_0000
        )

        XCTAssertEqual(
            IFDSPAppleUSBAudioUIDResolver.identity(
                for: "AppleUSBAudioEngine:JVCKENWOOD:TH_D75:RADIO-AUDIO-1:3",
                among: [descriptor]
            ),
            expected
        )
        XCTAssertEqual(
            IFDSPAppleUSBAudioUIDResolver.identity(
                for: "AppleUSBAudioEngine:JVCKENWOOD:TH_D75:100000:3",
                among: [descriptor]
            ),
            expected
        )
    }

    func testSelectorNeverFallsBackToBuiltInOrGenericUSBInput() {
        let devices = [
            makeDevice(
                id: 1,
                name: "TH-D75",
                transportType: kAudioDeviceTransportTypeBuiltIn,
                usbIdentity: makeUSBIdentity()
            ),
            makeDevice(
                id: 2,
                name: "TH-D75 USB Audio",
                transportType: kAudioDeviceTransportTypeUSB,
                usbIdentity: makeUSBIdentity(
                    vendorID: 0x1234,
                    productID: 0x5678
                )
            ),
        ]

        XCTAssertThrowsError(
            try IFDSPMacAudioDeviceSelector.selectTHD75Input(
                from: devices,
                expectedUSBDeviceRegistryEntryID: 1_042,
                expectedCATSerialNumber: "C3C10368"
            )
        ) { error in
            XCTAssertEqual(
                error as? IFDSPMacAudioError,
                .expectedRadioAudioNotReady(
                    name: "TH-D75",
                    reasons: ["CoreAudio did not report USB transport"]
                )
            )
        }
    }

    func testSelectorRequiresExactSharedUSBDeviceAncestor() throws {
        let first = makeDevice(
            id: 41,
            usbIdentity: makeUSBIdentity(registryEntryID: 1_041)
        )
        let second = makeDevice(
            id: 42,
            usbIdentity: makeUSBIdentity(registryEntryID: 1_042)
        )

        let selected = try IFDSPMacAudioDeviceSelector.selectTHD75Input(
            from: [first, second],
            expectedUSBDeviceRegistryEntryID: 1_042,
            expectedCATSerialNumber: "C3C10368"
        )

        XCTAssertEqual(selected.audioDeviceID, 42)
        XCTAssertEqual(selected.usbIdentity?.registryEntryID, 1_042)

        XCTAssertThrowsError(
            try IFDSPMacAudioDeviceSelector.selectTHD75Input(
                from: [first, second],
                expectedUSBDeviceRegistryEntryID: 9_999,
                expectedCATSerialNumber: "C3C10368"
            )
        ) { error in
            XCTAssertEqual(
                error as? IFDSPMacAudioError,
                .expectedRadioAudioUnavailable(
                    expectedCATSerialNumber: "C3C10368",
                    expectedUSBDeviceRegistryEntryID: 9_999,
                    verifiedDeviceCount: 2
                )
            )
        }
        XCTAssertThrowsError(
            try IFDSPMacAudioDeviceSelector.selectTHD75Input(
                from: [first, second],
                expectedUSBDeviceRegistryEntryID: 0,
                expectedCATSerialNumber: "C3C10368"
            )
        ) { error in
            XCTAssertEqual(
                error as? IFDSPMacAudioError,
                .invalidExpectedUSBDeviceRegistryEntryID
            )
        }
    }

    func testSelectorHasNoUSBDescriptorSerialDependency() throws {
        let device = makeDevice(
            id: 42,
            usbIdentity: makeUSBIdentity()
        )

        let selected = try IFDSPMacAudioDeviceSelector.selectTHD75Input(
            from: [device],
            expectedUSBDeviceRegistryEntryID: 1_042,
            expectedCATSerialNumber: "C5310165"
        )

        XCTAssertEqual(selected.audioDeviceID, 42)
    }

    func testSelectorRejectsDuplicateSharedUSBAncestorMatches() {
        let first = makeDevice(
            id: 41,
            usbIdentity: makeUSBIdentity(registryEntryID: 1_041)
        )
        let second = makeDevice(
            id: 42,
            usbIdentity: makeUSBIdentity(registryEntryID: 1_041)
        )

        XCTAssertThrowsError(
            try IFDSPMacAudioDeviceSelector.selectTHD75Input(
                from: [first, second],
                expectedUSBDeviceRegistryEntryID: 1_041,
                expectedCATSerialNumber: "C3C10368"
            )
        ) { error in
            XCTAssertEqual(
                error as? IFDSPMacAudioError,
                .ambiguousExpectedRadioAudio(
                    expectedCATSerialNumber: "C3C10368",
                    matchCount: 2
                )
            )
        }
    }

    func testInputProofValidatesCATSerialAndRegistryIdentity() throws {
        let valid = try IFDSPUSBInputProof(
            catSerialNumber: "C3C10368",
            macOSUSBDeviceRegistryEntryID: 1_042
        )
        XCTAssertEqual(valid.catSerialNumber, "C3C10368")
        XCTAssertEqual(valid.macOSUSBDeviceRegistryEntryID, 1_042)

        XCTAssertThrowsError(try IFDSPUSBInputProof(
            catSerialNumber: "",
            macOSUSBDeviceRegistryEntryID: 1_042
        )) { error in
            XCTAssertEqual(
                error as? IFDSPUSBInputProof.ValidationError,
                .invalidCATSerial
            )
        }
        XCTAssertThrowsError(try IFDSPUSBInputProof(
            catSerialNumber: "C3C10368",
            macOSUSBDeviceRegistryEntryID: 0
        )) { error in
            XCTAssertEqual(
                error as? IFDSPUSBInputProof.ValidationError,
                .invalidMacOSUSBDeviceRegistryEntryID
            )
        }
    }

    func testMacServiceRejectsProofWithoutUSBAncestorBeforePermission() async throws {
        let backend = FakeIFDSPMacAudioBackend(devices: [makeDevice(id: 42)])
        let service = IFDSPAudioStreamService(
            audioBackend: backend,
            readinessPolicy: .immediate,
            requestAudioPermission: { XCTFail("permission must not be requested"); return true }
        )

        let token = await service.preflight(
            inputProof: try makeInputProof(registryEntryID: nil)
        )

        XCTAssertNil(token)
        guard case .failed(let message, _) = service.currentState else {
            return XCTFail("expected missing identity failure, got \(service.currentState)")
        }
        XCTAssertTrue(message.contains("did not retain its current macOS USB device identity"))
        XCTAssertEqual(backend.availableDevicesCallCount, 0)
    }

    func testPermissionDenialStopsBeforeAudioEnumeration() async throws {
        let backend = FakeIFDSPMacAudioBackend(devices: [makeDevice(id: 42)])
        let service = IFDSPAudioStreamService(
            audioBackend: backend,
            readinessPolicy: .immediate,
            requestAudioPermission: { false }
        )

        let token = await service.preflight(inputProof: try makeInputProof())

        XCTAssertNil(token)
        guard case .failed(let message, _) = service.currentState else {
            return XCTFail("expected permission failure, got \(service.currentState)")
        }
        XCTAssertTrue(message.contains("Audio-input permission"))
        XCTAssertEqual(backend.availableDevicesCallCount, 0)
        XCTAssertEqual(backend.revalidatedDeviceIDs, [])
        XCTAssertEqual(backend.startedDeviceIDs, [])
    }

    func testCancellationWhilePermissionIsSuspendedCannotIssueTokenOrEnumerate()
        async throws {
        let backend = FakeIFDSPMacAudioBackend(devices: [makeDevice(id: 42)])
        let permission = SuspendedIFDSPAudioPermission()
        let service = IFDSPAudioStreamService(
            audioBackend: backend,
            readinessPolicy: .immediate,
            requestAudioPermission: { await permission.request() }
        )
        let proof = try makeInputProof()
        let preflight = Task {
            await service.preflight(inputProof: proof)
        }
        let requestSuspended = await eventually { permission.isWaiting }
        XCTAssertTrue(requestSuspended)

        preflight.cancel()
        permission.resume(granted: true)
        let token = await preflight.value

        XCTAssertNil(token)
        XCTAssertEqual(service.currentState, .idle)
        XCTAssertEqual(backend.availableDevicesCallCount, 0)
        XCTAssertEqual(backend.revalidatedDeviceIDs, [])
        XCTAssertEqual(backend.startedDeviceIDs, [])
    }

    func testSelectorReportsExactZeroChannelDeviceAsNotReadyInsteadOfAbsent() {
        let device = makeDevice(id: 42, inputChannelCount: 0)

        XCTAssertThrowsError(
            try IFDSPMacAudioDeviceSelector.selectTHD75Input(
                from: [device],
                expectedUSBDeviceRegistryEntryID: 1_042,
                expectedCATSerialNumber: "C3C10368"
            )
        ) { error in
            XCTAssertEqual(
                error as? IFDSPMacAudioError,
                .expectedRadioAudioNotReady(
                    name: "TH-D75 USB Audio",
                    reasons: ["the device has no input channels"]
                )
            )
        }
    }

    func testServiceRevalidatesThenStartsTheExactAudioDeviceID() async throws {
        let enumerated = makeDevice(id: 42, name: "Enumerated TH-D75")
        let revalidated = makeDevice(id: 42, name: "Revalidated TH-D75")
        let backend = FakeIFDSPMacAudioBackend(devices: [enumerated])
        backend.revalidatedDevice = revalidated
        let service = IFDSPAudioStreamService(
            audioBackend: backend,
            readinessPolicy: .immediate,
            requestAudioPermission: { true }
        )

        guard let token = await service.preflight(
            inputProof: try makeInputProof()
        ) else {
            return XCTFail("expected audio preflight token")
        }

        XCTAssertEqual(backend.availableDevicesCallCount, 1)
        XCTAssertEqual(backend.revalidatedDeviceIDs, [42])
        XCTAssertEqual(backend.startedDeviceIDs, [])
        guard case .starting(let preparedRoute) = service.currentState else {
            return XCTFail("expected prepared route, got \(service.currentState)")
        }
        XCTAssertEqual(preparedRoute, "Revalidated TH-D75")

        await service.start(preparedInput: token)

        XCTAssertEqual(backend.revalidatedDeviceIDs, [42, 42])
        XCTAssertEqual(backend.startedDeviceIDs, [42])
        XCTAssertEqual(backend.startedDeviceNames, ["Revalidated TH-D75"])
        guard case .streaming(let route, nil) = service.currentState else {
            service.stop()
            return XCTFail("expected streaming state, got \(service.currentState)")
        }
        XCTAssertEqual(route.name, "Revalidated TH-D75")
        XCTAssertEqual(route.kind, .usbAudio)

        await service.start(preparedInput: token)
        XCTAssertEqual(backend.startedDeviceIDs, [42])
        XCTAssertEqual(backend.session.stopCallCount, 0)

        service.stop()
        XCTAssertEqual(backend.session.stopCallCount, 1)
    }

    func testIdentityChangeDuringRevalidationPreventsStart() async throws {
        let enumerated = makeDevice(id: 42)
        let changed = makeDevice(
            id: 99,
            usbIdentity: makeUSBIdentity(registryEntryID: 9_999)
        )
        let backend = FakeIFDSPMacAudioBackend(devices: [enumerated])
        backend.revalidatedDevice = changed
        let service = IFDSPAudioStreamService(
            audioBackend: backend,
            readinessPolicy: .immediate,
            requestAudioPermission: { true }
        )

        let token = await service.preflight(inputProof: try makeInputProof())

        XCTAssertNil(token)
        XCTAssertEqual(backend.revalidatedDeviceIDs, [42])
        XCTAssertEqual(backend.startedDeviceIDs, [])
        guard case .failed(let message, _) = service.currentState else {
            return XCTFail("expected identity failure, got \(service.currentState)")
        }
        XCTAssertTrue(message.contains("changed before capture"))
    }

    func testIdentityChangeAfterPreflightPreventsCaptureStart() async throws {
        let backend = FakeIFDSPMacAudioBackend(devices: [makeDevice(id: 42)])
        let service = IFDSPAudioStreamService(
            audioBackend: backend,
            readinessPolicy: .immediate,
            requestAudioPermission: { true }
        )
        guard let token = await service.preflight(
            inputProof: try makeInputProof()
        ) else {
            return XCTFail("expected audio preflight token")
        }
        backend.revalidatedDevice = makeDevice(
            id: 99,
            usbIdentity: makeUSBIdentity(registryEntryID: 9_999)
        )

        await service.start(preparedInput: token)

        XCTAssertEqual(backend.revalidatedDeviceIDs, [42, 42])
        XCTAssertEqual(backend.startedDeviceIDs, [])
        guard case .failed(let message, _) = service.currentState else {
            return XCTFail("expected identity failure, got \(service.currentState)")
        }
        XCTAssertTrue(message.contains("changed before capture"))
    }

    func testSafeBufferSizeChangeAfterPreflightUsesRevalidatedCaptureContract()
        async throws {
        let backend = FakeIFDSPMacAudioBackend(
            devices: [makeDevice(id: 42, bufferFrameSize: 4_800)]
        )
        let service = IFDSPAudioStreamService(
            audioBackend: backend,
            readinessPolicy: .immediate,
            requestAudioPermission: { true }
        )
        guard let token = await service.preflight(
            inputProof: try makeInputProof()
        ) else {
            return XCTFail("expected audio preflight token")
        }
        backend.revalidatedDevice = makeDevice(
            id: 42,
            bufferFrameSize: 2_400
        )

        await service.start(preparedInput: token)

        XCTAssertEqual(backend.startedDeviceIDs, [42])
        XCTAssertEqual(backend.startedBufferFrameSizes, [2_400])
        guard case .streaming = service.currentState else {
            return XCTFail("expected capture after safe buffer-size change")
        }
        service.stop()
    }

    func testPreflightReenumeratesBrieflyWithoutStartingCapture() async throws {
        let backend = FakeIFDSPMacAudioBackend(devices: [makeDevice(id: 42)])
        backend.availableDeviceSnapshots = [[], [makeDevice(id: 42)]]
        let service = IFDSPAudioStreamService(
            audioBackend: backend,
            readinessPolicy: IFDSPMacAudioReadinessPolicy(
                maximumAttempts: 2,
                retryDelayNanoseconds: 0
            ),
            requestAudioPermission: { true }
        )

        let token = await service.preflight(inputProof: try makeInputProof())

        XCTAssertNotNil(token)
        XCTAssertEqual(backend.availableDevicesCallCount, 2)
        XCTAssertEqual(backend.startedDeviceIDs, [])
    }

    func testReadinessWaitIsTaskCancellable() async throws {
        let backend = FakeIFDSPMacAudioBackend(devices: [])
        let service = IFDSPAudioStreamService(
            audioBackend: backend,
            readinessPolicy: IFDSPMacAudioReadinessPolicy(
                maximumAttempts: 10,
                retryDelayNanoseconds: 5_000_000_000
            ),
            requestAudioPermission: { true }
        )
        let proof = try makeInputProof()
        let preflight = Task {
            await service.preflight(inputProof: proof)
        }
        let enumerationStarted = await eventually {
            backend.availableDevicesCallCount == 1
        }
        XCTAssertTrue(enumerationStarted)

        preflight.cancel()
        let token = await preflight.value

        XCTAssertNil(token)
        XCTAssertEqual(backend.availableDevicesCallCount, 1)
        XCTAssertEqual(service.currentState, .idle)
        XCTAssertEqual(backend.startedDeviceIDs, [])
    }

    func testAudioInventoryQueryFailureIsNotReportedAsMissingRadio() async throws {
        let backend = FakeIFDSPMacAudioBackend(devices: [])
        backend.queryFailures = [
            "CoreAudio device ID 3141: failed reading the device UID"
        ]
        let service = IFDSPAudioStreamService(
            audioBackend: backend,
            readinessPolicy: .immediate,
            requestAudioPermission: { true }
        )

        let token = await service.preflight(inputProof: try makeInputProof())

        XCTAssertNil(token)
        guard case .failed(let message, _) = service.currentState else {
            return XCTFail("expected incomplete inventory failure")
        }
        XCTAssertTrue(message.contains("could not inspect every current audio device"))
        XCTAssertTrue(message.contains("CoreAudio device ID 3141"))
        XCTAssertEqual(backend.startedDeviceIDs, [])
    }

    func testExactMatchCannotBypassAnUninspectableCurrentAudioDevice()
        async throws {
        let backend = FakeIFDSPMacAudioBackend(devices: [makeDevice(id: 42)])
        backend.queryFailures = [
            "CoreAudio device ID 3142: failed reading its input configuration"
        ]
        let service = IFDSPAudioStreamService(
            audioBackend: backend,
            readinessPolicy: .immediate,
            requestAudioPermission: { true }
        )

        let token = await service.preflight(inputProof: try makeInputProof())

        XCTAssertNil(token)
        guard case .failed(let message, _) = service.currentState else {
            return XCTFail("expected incomplete inventory failure")
        }
        XCTAssertTrue(message.contains("could not inspect every current audio device"))
        XCTAssertEqual(backend.revalidatedDeviceIDs, [])
        XCTAssertEqual(backend.startedDeviceIDs, [])
    }

    func testObservedADCStreamReportsExactIdentityRejectionInsteadOfMissingName()
        async throws {
        let adc = makeDevice(
            id: 3_141,
            uid: "AppleUSBAudioEngine:JVCKENWOOD:TH_D75:100000:3",
            name: "ADC stream IN",
            usbIdentity: nil
        )
        let backend = FakeIFDSPMacAudioBackend(devices: [adc])
        let service = IFDSPAudioStreamService(
            audioBackend: backend,
            readinessPolicy: .immediate,
            requestAudioPermission: { true }
        )

        let token = await service.preflight(inputProof: try makeInputProof())

        XCTAssertNil(token)
        guard case .failed(let message, _) = service.currentState else {
            return XCTFail("expected exact identity diagnostic, got \(service.currentState)")
        }
        XCTAssertTrue(message.contains("ADC stream IN"))
        XCTAssertTrue(message.contains("UID did not resolve"))
        XCTAssertEqual(backend.startedDeviceIDs, [])
    }

    func testStoppedPreflightTokenCannotStartOrStopANewerSession() async throws {
        let backend = FakeIFDSPMacAudioBackend(devices: [makeDevice(id: 42)])
        let service = IFDSPAudioStreamService(
            audioBackend: backend,
            readinessPolicy: .immediate,
            requestAudioPermission: { true }
        )
        guard let staleToken = await service.preflight(
            inputProof: try makeInputProof()
        ) else {
            return XCTFail("expected first preflight token")
        }
        service.stop()
        guard let currentToken = await service.preflight(
            inputProof: try makeInputProof()
        ) else {
            return XCTFail("expected second preflight token")
        }

        await service.start(preparedInput: staleToken)

        XCTAssertEqual(backend.startedDeviceIDs, [])
        guard case .starting = service.currentState else {
            return XCTFail("stale token changed the current preflight")
        }

        await service.start(preparedInput: currentToken)
        XCTAssertEqual(backend.startedDeviceIDs, [42])
        service.stop()
    }

    func testDeviceLossStopsWithoutEnumeratingOrStartingFallback() async throws {
        let backend = FakeIFDSPMacAudioBackend(devices: [makeDevice(id: 42)])
        let service = IFDSPAudioStreamService(
            audioBackend: backend,
            readinessPolicy: .immediate,
            requestAudioPermission: { true }
        )
        guard let token = await service.preflight(
            inputProof: try makeInputProof()
        ) else {
            return XCTFail("expected audio preflight token")
        }
        await service.start(preparedInput: token)

        backend.reportDeviceLoss("selected TH-D75 disappeared; no fallback")
        let paused = await eventually {
            if case .paused = service.currentState { return true }
            return false
        }

        XCTAssertTrue(paused)
        XCTAssertEqual(backend.availableDevicesCallCount, 1)
        XCTAssertEqual(backend.startedDeviceIDs, [42])
        XCTAssertEqual(backend.session.stopCallCount, 1)
        guard case .paused(let reason, _) = service.currentState else { return }
        XCTAssertTrue(reason.contains("no fallback"))
    }

    func testWorkerSidePCMAndOverrunAccountingReachSharedDSPPipeline() async throws {
        let backend = FakeIFDSPMacAudioBackend(devices: [makeDevice(id: 42)])
        let service = IFDSPAudioStreamService(
            audioBackend: backend,
            readinessPolicy: .immediate,
            requestAudioPermission: { true }
        )
        guard let token = await service.preflight(
            inputProof: try makeInputProof()
        ) else {
            return XCTFail("expected audio preflight token")
        }
        await service.start(preparedInput: token)

        backend.reportOverrun(blockCount: 2, sampleCount: 9_600)
        backend.emit(samples: [Float](repeating: 0.125, count: 4_800))
        let processed = await eventually {
            service.currentState.latestFrame != nil
        }

        XCTAssertTrue(processed)
        guard let frame = service.currentState.latestFrame else {
            service.stop()
            return XCTFail("expected a processed IF-DSP frame")
        }
        XCTAssertEqual(frame.sourceBlockCount, 1)
        XCTAssertEqual(frame.sourceSampleCount, 4_800)
        XCTAssertEqual(frame.droppedBlockCount, 2)
        XCTAssertEqual(frame.droppedSampleCount, 9_600)
        service.stop()
    }

    func testFloatStereoDecoderDownmixesOnWorkerSide() {
        let samples: [Float] = [1, -1, 0.25, 0.75]
        var descriptor = IFDSPMacRawBufferDescriptor(
            byteOffset: 0,
            byteCount: samples.count * MemoryLayout<Float>.stride,
            channelCount: 2
        )
        let decoded: [Float]? = samples.withUnsafeBytes { bytes -> [Float]? in
            guard let baseAddress = bytes.baseAddress else { return nil }
            return withUnsafePointer(to: &descriptor) { descriptorPointer in
                IFDSPMacPCMDecoder.decode(
                    IFDSPMacRawAudioBlockView(
                        data: baseAddress,
                        buffers: descriptorPointer,
                        bufferCount: 1,
                        frameCount: 2
                    ),
                    format: makePCMFormat(channelCount: 2)
                )
            }
        }

        XCTAssertEqual(decoded?.count, 2)
        XCTAssertEqual(decoded?[0] ?? .nan, 0, accuracy: 0.000_001)
        XCTAssertEqual(decoded?[1] ?? .nan, 0.5, accuracy: 0.000_001)
    }

    private func eventually(
        attempts: Int = 200,
        condition: @MainActor () -> Bool
    ) async -> Bool {
        for _ in 0..<attempts {
            if condition() { return true }
            try? await Task.sleep(nanoseconds: 5_000_000)
        }
        return condition()
    }

    private func makeDevice(
        id: AudioDeviceID,
        uid: String? = nil,
        name: String = "TH-D75 USB Audio",
        transportType: UInt32 = kAudioDeviceTransportTypeUSB,
        inputChannelCount: Int = 1,
        bufferFrameSize: UInt32 = 4_800,
        usbIdentity: IFDSPMacUSBIdentity? = IFDSPMacUSBIdentity(
            vendorID: 0x2166,
            productID: 0x9023,
            registryEntryID: 1_042
        )
    ) -> IFDSPMacAudioDevice {
        IFDSPMacAudioDevice(
            audioDeviceID: id,
            uid: uid ?? "coreaudio-\(id)",
            name: name,
            transportType: transportType,
            inputChannelCount: inputChannelCount,
            bufferFrameSize: bufferFrameSize,
            sampleRate: 48_000,
            streamFormat: makePCMFormat(channelCount: 1),
            usbIdentity: usbIdentity,
            isAlive: true
        )
    }

    private func makeInputProof(
        catSerialNumber: String = "C3C10368",
        registryEntryID: UInt64? = 1_042
    ) throws -> IFDSPUSBInputProof {
        try IFDSPUSBInputProof(
            catSerialNumber: catSerialNumber,
            macOSUSBDeviceRegistryEntryID: registryEntryID
        )
    }

    private func makeUSBIdentity(
        vendorID: UInt16 = 0x2166,
        productID: UInt16 = 0x9023,
        registryEntryID: UInt64 = 1_042
    ) -> IFDSPMacUSBIdentity {
        IFDSPMacUSBIdentity(
            vendorID: vendorID,
            productID: productID,
            registryEntryID: registryEntryID
        )
    }

    private func makePCMFormat(channelCount: UInt32) -> IFDSPMacPCMFormat {
        IFDSPMacPCMFormat(
            AudioStreamBasicDescription(
                mSampleRate: 48_000,
                mFormatID: kAudioFormatLinearPCM,
                mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
                mBytesPerPacket: UInt32(MemoryLayout<Float>.size) * channelCount,
                mFramesPerPacket: 1,
                mBytesPerFrame: UInt32(MemoryLayout<Float>.size) * channelCount,
                mChannelsPerFrame: channelCount,
                mBitsPerChannel: 32,
                mReserved: 0
            )
        )
    }
}

@MainActor
private final class FakeIFDSPMacAudioBackend:
    IFDSPMacAudioBackend,
    @unchecked Sendable
{
    let session = FakeIFDSPMacAudioCaptureSession()
    var devices: [IFDSPMacAudioDevice]
    var availableDeviceSnapshots: [[IFDSPMacAudioDevice]] = []
    var queryFailures: [String] = []
    var revalidatedDevice: IFDSPMacAudioDevice?
    private(set) var availableDevicesCallCount = 0
    private(set) var revalidatedDeviceIDs: [AudioDeviceID] = []
    private(set) var startedDeviceIDs: [AudioDeviceID] = []
    private(set) var startedDeviceNames: [String] = []
    private(set) var startedBufferFrameSizes: [UInt32] = []
    private var receive: (@Sendable (IFDSPSourcePCMBlock) -> Void)?
    private var overrun: (@Sendable (Int, Int) -> Void)?
    private var deviceLost: (@Sendable (String) -> Void)?
    private var captureFailed: (@Sendable (String) -> Void)?

    init(devices: [IFDSPMacAudioDevice]) {
        self.devices = devices
    }

    func availableDeviceInventory() throws -> IFDSPMacAudioDeviceInventory {
        availableDevicesCallCount += 1
        if !availableDeviceSnapshots.isEmpty {
            return IFDSPMacAudioDeviceInventory(
                devices: availableDeviceSnapshots.removeFirst(),
                queryFailures: queryFailures
            )
        }
        return IFDSPMacAudioDeviceInventory(
            devices: devices,
            queryFailures: queryFailures
        )
    }

    func revalidate(_ device: IFDSPMacAudioDevice) throws -> IFDSPMacAudioDevice {
        revalidatedDeviceIDs.append(device.audioDeviceID)
        return revalidatedDevice ?? device
    }

    func startCapture(
        device: IFDSPMacAudioDevice,
        receive: @escaping @Sendable (IFDSPSourcePCMBlock) -> Void,
        overrun: @escaping @Sendable (Int, Int) -> Void,
        deviceLost: @escaping @Sendable (String) -> Void,
        captureFailed: @escaping @Sendable (String) -> Void
    ) throws -> any IFDSPMacAudioCaptureSession {
        startedDeviceIDs.append(device.audioDeviceID)
        startedDeviceNames.append(device.name)
        startedBufferFrameSizes.append(device.bufferFrameSize)
        self.receive = receive
        self.overrun = overrun
        self.deviceLost = deviceLost
        self.captureFailed = captureFailed
        return session
    }

    func emit(samples: [Float], sampleRate: Double = 48_000) {
        receive?(IFDSPSourcePCMBlock(samples: samples, sampleRate: sampleRate))
    }

    func reportOverrun(blockCount: Int, sampleCount: Int) {
        overrun?(blockCount, sampleCount)
    }

    func reportDeviceLoss(_ reason: String) {
        deviceLost?(reason)
    }
}

@MainActor
private final class SuspendedIFDSPAudioPermission {
    private var continuation: CheckedContinuation<Bool, Never>?

    var isWaiting: Bool { continuation != nil }

    func request() async -> Bool {
        await withCheckedContinuation { continuation in
            precondition(self.continuation == nil)
            self.continuation = continuation
        }
    }

    func resume(granted: Bool) {
        let continuation = continuation
        self.continuation = nil
        continuation?.resume(returning: granted)
    }
}

private final class FakeIFDSPMacAudioCaptureSession:
    IFDSPMacAudioCaptureSession,
    @unchecked Sendable
{
    private let lock = NSLock()
    private var stops = 0

    var stopCallCount: Int { lock.withLock { stops } }

    func stop() {
        lock.withLock { stops += 1 }
    }
}

#endif
