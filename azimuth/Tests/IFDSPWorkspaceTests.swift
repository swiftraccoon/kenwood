// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Azimuth

final class IFDSPWorkspaceTests: XCTestCase {
    func testPCMBlockBatcherPreservesEverySampleInFixedWindows() {
        var batcher = IFDSPPCMBlockBatcher(targetSampleCount: 4)

        batcher.append([1, 2, 3])
        XCTAssertNil(batcher.nextBatch())
        batcher.append([4, 5, 6, 7, 8, 9])

        XCTAssertEqual(batcher.nextBatch(), [1, 2, 3, 4])
        XCTAssertEqual(batcher.nextBatch(), [5, 6, 7, 8])
        XCTAssertNil(batcher.nextBatch())
        XCTAssertEqual(batcher.bufferedSampleCount, 1)
    }

    func testCaptureStatisticsDistinguishSourceBlocksFromDroppedSamples() {
        var statistics = IFDSPCaptureStatistics()
        statistics.recordSourceBlock(sampleCount: 960)
        statistics.recordSourceBlock(sampleCount: 2_048)
        statistics.recordDroppedBlock(sampleCount: 960)

        XCTAssertEqual(statistics.sourceBlockCount, 2)
        XCTAssertEqual(statistics.sourceSampleCount, 3_008)
        XCTAssertEqual(statistics.droppedBlockCount, 1)
        XCTAssertEqual(statistics.droppedSampleCount, 960)
    }

    func testCaptureLossUsesSamplesRatherThanUnrelatedDSPWindowCount() {
        let frame = IFDSPLiveFrame(
            sequence: 2,
            inputSampleCount: 9_600,
            inputLevelDBFS: -24,
            outputLevelDBFS: -18,
            spectrum: nil,
            clippedSampleCount: 0,
            sourceBlockCount: 12,
            sourceSampleCount: 12_000,
            droppedBlockCount: 2,
            droppedSampleCount: 2_400,
            capturedAt: Date()
        )

        XCTAssertEqual(frame.captureLossFraction, 0.2, accuracy: 0.000_001)
    }

    func testWaterfallRowsBecomeOneBoundedRaster() throws {
        let image = try XCTUnwrap(
            IFDSPWaterfallRaster.makeImage(
                rowsDBFS: [
                    [-120, -90, -60, -30],
                    [-110, -80, -50, -20],
                    [-100, -70, -40, -10],
                ],
                levelBounds: -120...0
            )
        )

        XCTAssertEqual(image.width, 4)
        XCTAssertEqual(image.height, 3)
        XCTAssertEqual(image.bitsPerPixel, 32)
    }

    func testOperationalFrequencyEntryRequiresQualifiedBandBRangesAndFiveKilohertzSteps() {
        XCTAssertEqual(IFDSPFrequencyEntry.frequencyHz(fromMHz: "145.500"), 145_500_000)
        XCTAssertEqual(IFDSPFrequencyEntry.frequencyHz(fromMHz: " 433.925 "), 433_925_000)
        XCTAssertEqual(IFDSPFrequencyEntry.frequencyHz(fromMHz: "0.100"), 100_000)
        XCTAssertEqual(IFDSPFrequencyEntry.frequencyHz(fromMHz: "75.995"), 75_995_000)
        XCTAssertEqual(IFDSPFrequencyEntry.frequencyHz(fromMHz: "108.000"), 108_000_000)
        XCTAssertEqual(IFDSPFrequencyEntry.frequencyHz(fromMHz: "523.995"), 523_995_000)
        XCTAssertNil(IFDSPFrequencyEntry.frequencyHz(fromMHz: "76.000"))
        XCTAssertNil(IFDSPFrequencyEntry.frequencyHz(fromMHz: "100.000"))
        XCTAssertNil(IFDSPFrequencyEntry.frequencyHz(fromMHz: "107.995"))
        XCTAssertNil(IFDSPFrequencyEntry.frequencyHz(fromMHz: "145.502"))
        XCTAssertNil(IFDSPFrequencyEntry.frequencyHz(fromMHz: "524.000"))
        XCTAssertNil(IFDSPFrequencyEntry.frequencyHz(fromMHz: "not a frequency"))
    }

    func testEveryIFDSPControlResolvesToAnAuthoritativeCatalogSetting() async throws {
        let catalog = try await AzimuthCoreCatalogProvider().catalog()

        XCTAssertEqual(Set(IFDSPSettingMap.allSettingIDs).count, IFDSPSettingMap.allSettingIDs.count)
        for settingID in IFDSPSettingMap.allSettingIDs {
            XCTAssertNotNil(catalog.definition(id: settingID), settingID)
        }
    }

    func testSetupCardsUseTheFourDocumentedRadioMenus() async throws {
        let catalog = try await AzimuthCoreCatalogProvider().catalog()
        let expectedMenus = [
            "radio.UsbFunction": ["980"],
            "radio.DetectOutput": ["102"],
            "radio.SingleBandDisplay": ["904"],
            "radio.UsbAudioOutLevel": ["91A"],
        ]

        XCTAssertEqual(Set(IFDSPSettingMap.setupSettingIDs), Set(expectedMenus.keys))
        for (settingID, menuNumbers) in expectedMenus {
            XCTAssertEqual(catalog.definition(id: settingID)?.menuNumbers, menuNumbers)
        }
    }

    func testEqualizerRawLevelsDecodeToDocumentedDecibelDomains() {
        XCTAssertEqual(
            IFDSPValueFormatter.equalizerDecibels(.integer(0), kind: .receive),
            -9
        )
        XCTAssertEqual(
            IFDSPValueFormatter.equalizerDecibels(.integer(9), kind: .receive),
            0
        )
        XCTAssertEqual(
            IFDSPValueFormatter.equalizerDecibels(.integer(18), kind: .receive),
            9
        )
        XCTAssertEqual(
            IFDSPValueFormatter.equalizerDecibels(.integer(12), kind: .transmit),
            3
        )
        XCTAssertNil(IFDSPValueFormatter.equalizerDecibels(.integer(13), kind: .transmit))
        XCTAssertNil(IFDSPValueFormatter.equalizerDecibels(.boolean(true), kind: .receive))

        XCTAssertEqual(IFDSPValueFormatter.decibelLabel(-9), "-9 dB")
        XCTAssertEqual(IFDSPValueFormatter.decibelLabel(0), "0 dB")
        XCTAssertEqual(IFDSPValueFormatter.decibelLabel(3), "+3 dB")
    }
}
