// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import XCTest
@testable import Lodestar

final class RailStateTests: XCTestCase {
    /// Baseline healthy inputs: reflector linked, radio relaying,
    /// nobody transmitting. Individual tests perturb one dimension.
    private func healthy() -> RailState.Inputs {
        RailState.Inputs(
            transport: .connected,
            radioMode: .mmdvm,
            mcpStatus: .idle,
            hasProbeError: false,
            reflector: .connected,
            relay: .running,
            streamActive: false,
            hasHeardHistory: true,
            manualChainExpanded: false
        )
    }

    // Spec §4 row: healthy + quiet → strip + heard list fills rail.
    func testHealthyQuietShowsStripAndHeardList() {
        let s = RailState.derive(healthy())
        XCTAssertEqual(s.chain, .strip)
        XCTAssertFalse(s.showsMcpCard)
        XCTAssertFalse(s.showsNowTransmitting)
        XCTAssertTrue(s.showsHeardList)
        XCTAssertFalse(s.mapDimmed)
    }

    // Spec §4 row: station transmitting → strip + NOW TX + heard list.
    func testStreamActiveShowsNowTransmitting() {
        var i = healthy()
        i.streamActive = true
        let s = RailState.derive(i)
        XCTAssertEqual(s.chain, .strip)
        XCTAssertTrue(s.showsNowTransmitting)
        XCTAssertTrue(s.showsHeardList)
    }

    // Spec §4 row: first run, nothing connected → expanded chain only.
    func testNothingConnectedExpandsChain() {
        let i = RailState.Inputs(
            transport: .disconnected,
            radioMode: .unknown,
            mcpStatus: .idle,
            hasProbeError: false,
            reflector: .disconnected,
            relay: .stopped,
            streamActive: false,
            hasHeardHistory: false,
            manualChainExpanded: false
        )
        let s = RailState.derive(i)
        XCTAssertEqual(s.chain, .expanded)
        XCTAssertFalse(s.showsMcpCard)
        XCTAssertFalse(s.showsNowTransmitting)
        XCTAssertFalse(s.showsHeardList)
        XCTAssertTrue(s.mapDimmed)
    }

    // Persisted heard history stays visible even before linking
    // (matches the dashboard's `connected || !recentlyHeard.isEmpty`).
    func testHeardHistoryVisibleWhenPersistedButDisconnected() {
        var i = healthy()
        i.reflector = .disconnected
        i.relay = .stopped
        let s = RailState.derive(i)
        XCTAssertTrue(s.showsHeardList)
        XCTAssertTrue(s.mapDimmed)
    }

    // Spec §4 row: CAT mode → expanded chain + MCP setup card.
    func testCatModeExpandsChainAndShowsMcpCard() {
        var i = healthy()
        i.radioMode = .cat
        i.relay = .stopped
        let s = RailState.derive(i)
        XCTAssertEqual(s.chain, .expanded)
        XCTAssertTrue(s.showsMcpCard)
    }

    // Unrecognized probe byte behaves like CAT for setup purposes.
    func testUnrecognizedModeShowsMcpCard() {
        var i = healthy()
        i.radioMode = .unrecognized(firstByte: 0x42)
        i.relay = .stopped
        let s = RailState.derive(i)
        XCTAssertEqual(s.chain, .expanded)
        XCTAssertTrue(s.showsMcpCard)
    }

    // In-flight MCP flow keeps the card visible regardless of mode.
    func testMcpRunningShowsMcpCard() {
        var i = healthy()
        i.mcpStatus = .running("writing menu 650")
        let s = RailState.derive(i)
        XCTAssertTrue(s.showsMcpCard)
        XCTAssertEqual(s.chain, .expanded)
    }

    // Spec §4 precedence: any failure expands the chain…
    func testTransportFailureExpandsChain() {
        var i = healthy()
        i.transport = .failed(message: "USB unplugged")
        let s = RailState.derive(i)
        XCTAssertEqual(s.chain, .expanded)
    }

    func testReflectorFailureExpandsChain() {
        var i = healthy()
        i.reflector = .failed("auth rejected")
        i.relay = .stopped
        let s = RailState.derive(i)
        XCTAssertEqual(s.chain, .expanded)
        XCTAssertTrue(s.mapDimmed)
    }

    func testRelayFailureExpandsChain() {
        var i = healthy()
        i.relay = .failed("write timeout")
        let s = RailState.derive(i)
        XCTAssertEqual(s.chain, .expanded)
    }

    func testProbeErrorExpandsChain() {
        var i = healthy()
        i.hasProbeError = true
        let s = RailState.derive(i)
        XCTAssertEqual(s.chain, .expanded)
    }

    // …but an error does NOT hide a live stream: the operator still
    // sees who is on the air while the chain shows the failure.
    func testErrorDuringStreamKeepsNowTransmittingVisible() {
        var i = healthy()
        i.streamActive = true
        i.relay = .failed("write timeout")
        let s = RailState.derive(i)
        XCTAssertEqual(s.chain, .expanded)
        XCTAssertTrue(s.showsNowTransmitting)
    }

    // Radio connecting shows progress in the expanded chain.
    func testTransportConnectingExpandsChain() {
        var i = healthy()
        i.transport = .connecting
        let s = RailState.derive(i)
        XCTAssertEqual(s.chain, .expanded)
    }

    // Strip tap: manual override expands…
    func testManualExpandOverridesStrip() {
        var i = healthy()
        i.manualChainExpanded = true
        let s = RailState.derive(i)
        XCTAssertEqual(s.chain, .expanded)
    }

    // …and clearing the manual flag cannot collapse a forced expansion.
    func testManualCollapseCannotOverrideError() {
        var i = healthy()
        i.transport = .failed(message: "gone")
        i.manualChainExpanded = false
        let s = RailState.derive(i)
        XCTAssertEqual(s.chain, .expanded)
    }
}
