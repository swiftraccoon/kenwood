// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import CoreGraphics
import Foundation
import SwiftUI
#if os(iOS)
import UIKit
#endif

enum IPadUSBSetupGuidance: Equatable {
    case hidden
    case simulator
    case connectionTroubleshooting

    static func resolve(
        connection: RadioConnectionState,
        dataServicePresent: Bool,
        controlServicePresent: Bool,
        isSimulator: Bool
    ) -> Self {
        if isSimulator { return .simulator }
        if dataServicePresent && controlServicePresent { return .hidden }
        if case .failed = connection { return .connectionTroubleshooting }
        return .hidden
    }
}

struct RadioWorkspace: View {
    @Environment(AzimuthSceneModel.self) private var model

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: AzimuthLayout.pageSpacing) {
                #if os(iOS)
                if shouldShowIPadUSBSetup {
                    ipadUSBSetup
                }
                #endif

                radioConsole
            }
            .azimuthContentColumn(maxWidth: AzimuthLayout.workspaceWidth)
        }
        .azimuthPage()
        .accessibilityIdentifier("azimuth.page.radio")
        .toolbar { connectionToolbar }
        .alert(
            "Radio operation",
            isPresented: Binding(
                get: { model.operationError != nil },
                set: { if !$0 { model.dismissOperationError() } }
            )
        ) {
            Button("OK") { model.dismissOperationError() }
        } message: {
            Text(model.operationError ?? "Unknown radio error")
        }
    }

    @ToolbarContentBuilder
    private var connectionToolbar: some ToolbarContent {
        #if os(iOS)
        ToolbarItemGroup(placement: .topBarTrailing) {
            connectionToolbarStatus
            connectionToolbarAction
        }
        #else
        ToolbarItemGroup(placement: .primaryAction) {
            connectionToolbarStatus
            connectionToolbarAction
        }
        #endif
    }

    @ViewBuilder
    private var connectionToolbarStatus: some View {
        switch model.radioState.connection {
        case .disconnected:
            EmptyView()
        case .connecting:
            ProgressView()
                .controlSize(.small)
                .accessibilityLabel("Connecting to TH-D75")
        case .connected(let device, let transport):
            Label("Live", systemImage: "bolt.horizontal.circle.fill")
                .font(.caption.weight(.semibold))
                .foregroundStyle(AzimuthPalette.signal)
                .accessibilityLabel("\(device) connected over \(transport)")
        case .failed(let message):
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.red)
                .accessibilityLabel("Connection failed")
                .accessibilityHint(message)
        }
    }

    @ViewBuilder
    private var connectionToolbarAction: some View {
        #if targetEnvironment(simulator)
        Label("Physical iPad required", systemImage: "ipad")
            .font(.caption.weight(.semibold))
            .foregroundStyle(.secondary)
        #else
        switch model.radioState.connection {
        case .disconnected:
            Button {
                Task { await model.connectRadio() }
            } label: {
                Label("Connect", systemImage: "cable.connector")
            }
            .disabled(model.isRadioOperationInFlight)
            .accessibilityIdentifier("azimuth.radio.connect")
        case .connecting:
            EmptyView()
        case .connected:
            Button(role: .destructive) {
                Task { await model.disconnectRadio() }
            } label: {
                Label("Disconnect", systemImage: "cable.connector.slash")
            }
            .disabled(model.isRadioOperationInFlight)
            .accessibilityIdentifier("azimuth.radio.disconnect")
        case .failed:
            Button {
                Task { await model.connectRadio() }
            } label: {
                Label("Retry", systemImage: "arrow.clockwise")
            }
            .disabled(model.isRadioOperationInFlight)
            .accessibilityIdentifier("azimuth.radio.retry")
        }
        #endif
    }

    #if os(iOS)
    private var shouldShowIPadUSBSetup: Bool {
        ipadUSBSetupGuidance != .hidden
    }

    private var ipadUSBSetupGuidance: IPadUSBSetupGuidance {
        #if targetEnvironment(simulator)
        return .resolve(
            connection: model.radioState.connection,
            dataServicePresent: false,
            controlServicePresent: false,
            isSimulator: true
        )
        #else
        let link = IOKitAzimuthUSBSerialLink()
        return .resolve(
            connection: model.radioState.connection,
            dataServicePresent: link.servicePresent(),
            controlServicePresent: link.commServicePresent() == true,
            isSimulator: false
        )
        #endif
    }

    private var ipadUSBSetup: some View {
        InstrumentPanel {
            VStack(alignment: .leading, spacing: 12) {
                AzimuthEyebrow("iPad USB-C connection")
                #if targetEnvironment(simulator)
                Label(
                    "USBDriverKit cannot run in Simulator. Select a physical M-series iPad as the run destination.",
                    systemImage: "ipad.badge.exclamationmark"
                )
                .foregroundStyle(.orange)
                #else
                Label("Azimuth could not detect a complete TH-D75 USB connection.", systemImage: "cable.connector.slash")
                    .foregroundStyle(.orange)
                Label("Power on the radio, set Menu 980 to COM + AF/IF Output, and reconnect a data-capable USB-C cable.", systemImage: "1.circle.fill")
                Label("If those are already correct, open Settings to verify that the Azimuth driver is enabled and no competing TH-D75 driver is active.", systemImage: "2.circle.fill")

                Button {
                    guard let url = URL(string: UIApplication.openSettingsURLString) else { return }
                    UIApplication.shared.open(url)
                } label: {
                    Label("Check Driver in Settings", systemImage: "gear")
                }
                .buttonStyle(.bordered)
                #endif
            }
            .font(.callout)
        }
    }
    #endif

    private var radioConsole: some View {
        InstrumentPanel(padding: 16) {
            ViewThatFits(in: .horizontal) {
                HStack(alignment: .top, spacing: AzimuthLayout.pageSpacing) {
                    remoteDisplay
                        .frame(minWidth: 480, maxWidth: .infinity, alignment: .topLeading)

                    Divider()

                    VStack(alignment: .leading, spacing: AzimuthLayout.pageSpacing) {
                        remoteControls
                        Divider()
                        capabilityStrip
                    }
                    .frame(width: 360, alignment: .topLeading)
                }

                VStack(alignment: .leading, spacing: AzimuthLayout.pageSpacing) {
                    remoteDisplay
                    Divider()
                    remoteControls
                        .frame(maxWidth: 480, alignment: .topLeading)
                        .frame(maxWidth: .infinity, alignment: .center)
                    Divider()
                    capabilityStrip
                }
            }
        }
    }

    private var remoteDisplay: some View {
        VStack(alignment: .leading, spacing: AzimuthLayout.cardSpacing) {
            HStack {
                Label("Live color display", systemImage: "rectangle.on.rectangle")
                    .font(.headline)
                Spacer()
                frameStatus
                Button {
                    Task { await model.refreshRadioScreen() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .buttonStyle(.borderless)
                .disabled(
                    !model.radioState.capabilities.screenStreaming.isAvailable
                        || model.isRadioOperationInFlight
                )
                .accessibilityLabel("Refresh radio screen")
            }

            RadioScreenSurface(
                frame: model.radioState.screenFrame,
                error: model.radioState.lastScreenError
            )

            LazyVGrid(
                columns: [GridItem(.adaptive(minimum: 120), spacing: AzimuthLayout.cardSpacing)],
                alignment: .leading,
                spacing: AzimuthLayout.cardSpacing
            ) {
                AzimuthMetric(
                    label: "Primary",
                    value: model.radioState.telemetry.primaryFrequency ?? "–"
                )
                AzimuthMetric(
                    label: "Mode",
                    value: model.radioState.telemetry.operatingMode ?? "–"
                )
                AzimuthMetric(
                    label: "Band",
                    value: model.radioState.telemetry.activeBand ?? "–"
                )
                AzimuthMetric(
                    label: "Firmware",
                    value: model.radioState.telemetry.firmware ?? "–"
                )
            }
        }
    }

    @ViewBuilder
    private var frameStatus: some View {
        if let frame = model.radioState.screenFrame, frame.isValid {
            HStack(spacing: 5) {
                Circle().fill(AzimuthPalette.signal).frame(width: 6, height: 6)
                Text(frame.capturedAt, style: .relative)
            }
            .font(.caption.monospacedDigit())
            .foregroundStyle(.secondary)
        } else {
            Text("NO LIVE FRAME")
                .font(.caption2.bold().monospaced())
                .foregroundStyle(.secondary)
        }
    }

    private var remoteControls: some View {
        VStack(alignment: .leading, spacing: AzimuthLayout.cardSpacing) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("REMOTE PANEL")
                        .font(.caption.bold().monospaced())
                        .tracking(1.2)
                    Text("All 25 automated keys")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Circle()
                    .fill(controlsEnabled ? AzimuthPalette.signal : .secondary.opacity(0.4))
                    .frame(width: 8, height: 8)
            }

            HStack(spacing: 6) {
                panelKey("MODE", .mode)
                panelKey("MENU", .menu)
                panelKey("A/B", .ab)
                panelKey("F", .function)
                panelKey("MONI", .monitor)
            }

            HStack(alignment: .center, spacing: AzimuthLayout.cardSpacing) {
                directionalPad
                VStack(spacing: 6) {
                    keypadRow([("MARK\n0", .mark0), ("VFO\n1", .vfo1), ("MR\n2", .mr2)])
                    keypadRow([("CALL\n3", .call3), ("MSG\n4", .msg4), ("LIST\n5", .list5)])
                    keypadRow([("BCN\n6", .beacon6), ("REV\n7", .reverse7), ("TONE\n8", .tone8)])
                    keypadRow([("PF1\n9", .pf1_9), ("MHz\n✱", .mhzStar), ("PF2\n#", .pf2Hash)])
                }
            }

            Divider()
            HStack(spacing: 6) {
                panelKey("MIC PF1", .micPf1)
                panelKey("MIC PF2", .micPf2)
                panelKey("MIC PF3", .micPf3)
            }

            if !controlsEnabled {
                Label(controlsUnavailableReason, systemImage: "lock.fill")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private var directionalPad: some View {
        Grid(horizontalSpacing: 6, verticalSpacing: 6) {
            GridRow {
                Color.clear.frame(width: 44, height: 44)
                iconKey("chevron.up", .up)
                Color.clear.frame(width: 44, height: 44)
            }
            GridRow {
                iconKey("chevron.left", .left)
                panelKey("OK", .enter, prominent: true)
                iconKey("chevron.right", .right)
            }
            GridRow {
                Color.clear.frame(width: 44, height: 44)
                iconKey("chevron.down", .down)
                Color.clear.frame(width: 44, height: 44)
            }
        }
    }

    private func keypadRow(_ keys: [(String, RadioFrontPanelKey)]) -> some View {
        HStack(spacing: 6) {
            ForEach(keys, id: \.1) { title, key in
                panelKey(title, key)
            }
        }
    }

    private func panelKey(
        _ title: String,
        _ key: RadioFrontPanelKey,
        prominent: Bool = false
    ) -> some View {
        Button {
            Task { await model.press(key) }
        } label: {
            Text(title)
                .font(.system(size: title.contains("\n") ? 8 : 9, weight: .bold, design: .monospaced))
                .multilineTextAlignment(.center)
                .lineLimit(2)
                .minimumScaleFactor(0.7)
                .frame(maxWidth: .infinity, minHeight: 44)
                .foregroundStyle(prominent ? AzimuthPalette.instrumentBlack : .primary)
                .background(
                    prominent ? AzimuthPalette.signal : Color.primary.opacity(0.07),
                    in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                )
                .overlay {
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .strokeBorder(.primary.opacity(0.10))
                }
        }
        .buttonStyle(.plain)
        .disabled(!controlsEnabled || model.isRadioOperationInFlight)
        .accessibilityLabel(title.replacingOccurrences(of: "\n", with: " "))
    }

    private func iconKey(_ symbol: String, _ key: RadioFrontPanelKey) -> some View {
        Button {
            Task { await model.press(key) }
        } label: {
            Image(systemName: symbol)
                .font(.caption.bold())
                .frame(width: 44, height: 44)
                .background(Color.primary.opacity(0.07), in: RoundedRectangle(cornerRadius: 8))
        }
        .buttonStyle(.plain)
        .disabled(!controlsEnabled || model.isRadioOperationInFlight)
    }

    private var controlsEnabled: Bool {
        model.radioState.capabilities.frontPanelControl.isAvailable
    }

    private var controlsUnavailableReason: String {
        switch model.radioState.capabilities.frontPanelControl {
        case .available: return "Controls ready"
        case .preparing: return "Front-panel control is preparing."
        case .unavailable(let reason): return reason
        }
    }

    private var capabilityStrip: some View {
        VStack(alignment: .leading, spacing: AzimuthLayout.cardSpacing) {
            AzimuthEyebrow("Negotiated capabilities")
            LazyVGrid(
                columns: [GridItem(.adaptive(minimum: 155), spacing: AzimuthLayout.cardSpacing)],
                alignment: .leading,
                spacing: AzimuthLayout.cardSpacing
            ) {
                capability("Screen stream", model.radioState.capabilities.screenStreaming)
                capability("25-key control", model.radioState.capabilities.frontPanelControl)
                capability("Settings read", model.radioState.capabilities.settingRead)
                capability("Settings write", model.radioState.capabilities.settingWrite)
            }
        }
    }

    private func capability(_ title: String, _ state: RadioCapabilityState) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Image(systemName: capabilitySymbol(state))
                    .foregroundStyle(capabilityColor(state))
                Spacer()
                Text(capabilityLabel(state))
                    .font(.caption2.bold().monospaced())
                    .foregroundStyle(.secondary)
            }
            Text(title)
                .font(.subheadline.weight(.semibold))
        }
        .frame(maxWidth: .infinity, minHeight: 42, alignment: .topLeading)
        .padding(10)
        .background(.primary.opacity(0.04), in: RoundedRectangle(cornerRadius: 10))
    }

    private func capabilitySymbol(_ state: RadioCapabilityState) -> String {
        switch state {
        case .available: return "checkmark.circle.fill"
        case .preparing: return "clock.arrow.circlepath"
        case .unavailable: return "minus.circle"
        }
    }

    private func capabilityColor(_ state: RadioCapabilityState) -> Color {
        switch state {
        case .available: return AzimuthPalette.signal
        case .preparing: return AzimuthPalette.caution
        case .unavailable: return .secondary
        }
    }

    private func capabilityLabel(_ state: RadioCapabilityState) -> String {
        switch state {
        case .available: return "READY"
        case .preparing: return "PREPARING"
        case .unavailable: return "OFFLINE"
        }
    }
}

private struct RadioScreenSurface: View {
    let frame: RadioScreenFrame?
    let error: String?

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 13, style: .continuous)
                .fill(AzimuthPalette.instrumentBlack)
                .shadow(color: .black.opacity(0.45), radius: 8, y: 3)

            if let frame, frame.isValid, let image = frame.cgImage {
                Image(decorative: image, scale: 1)
                    .resizable()
                    .interpolation(.none)
                    .aspectRatio(contentMode: .fit)
                    .padding(16)
                    .accessibilityLabel("Live TH-D75 color display")
            } else {
                VStack(spacing: 12) {
                    Image(systemName: error == nil ? "rectangle.slash" : "exclamationmark.triangle")
                        .font(.system(size: 34, weight: .light))
                    Text(error == nil ? "NO LIVE RADIO FRAME" : "FRAME UNAVAILABLE")
                        .font(.caption.bold().monospaced())
                        .tracking(1.2)
                    Text(error ?? "Azimuth will show the radio only after a validated 240 × 180 color frame arrives.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 360)
                }
                .foregroundStyle(.white)
                .padding()
            }
        }
        .aspectRatio(4.0 / 3.0, contentMode: .fit)
        .overlay {
            RoundedRectangle(cornerRadius: 13, style: .continuous)
                .strokeBorder(.white.opacity(0.14), lineWidth: 1)
        }
    }
}

extension RadioScreenFrame {
    var cgImage: CGImage? {
        guard isValid,
              let provider = CGDataProvider(data: rgba8888 as CFData) else { return nil }
        return CGImage(
            width: width,
            height: height,
            bitsPerComponent: 8,
            bitsPerPixel: 32,
            bytesPerRow: width * 4,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: [
                CGBitmapInfo(rawValue: CGImageAlphaInfo.last.rawValue),
                .byteOrder32Big,
            ],
            provider: provider,
            decode: nil,
            shouldInterpolate: false,
            intent: .defaultIntent
        )
    }
}
