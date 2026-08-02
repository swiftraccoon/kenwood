// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI
import OSLog

private let azimuthAppLog = Logger(
    subsystem: "org.swiftraccoon.azimuth",
    category: "app"
)

@main
@MainActor
struct AzimuthApp: App {
    @State private var model: AzimuthSceneModel

    init() {
        let version = Bundle.main.object(
            forInfoDictionaryKey: "CFBundleShortVersionString"
        ) as? String ?? "unknown"
        let build = Bundle.main.object(
            forInfoDictionaryKey: "CFBundleVersion"
        ) as? String ?? "unknown"
        #if targetEnvironment(simulator)
        let runtime = "iPad Simulator"
        #elseif os(iOS)
        let runtime = "physical iPad"
        #else
        let runtime = "macOS"
        #endif
        let operatingSystem = ProcessInfo.processInfo.operatingSystemVersionString
        azimuthAppLog.notice("[Azimuth App] launch version=\(version, privacy: .public) build=\(build, privacy: .public) runtime=\(runtime, privacy: .public) os=\(operatingSystem, privacy: .public)")

        let records = settingCatalog()
        do {
            let radioController = try AzimuthLiveRadioController(
                transport: AzimuthUSBSerialTransport.platformDefault(),
                records: records
            )
            let catalogProvider = try AzimuthCoreCatalogProvider(records: records)
            _model = State(
                initialValue: AzimuthSceneModel(
                    radioController: radioController,
                    catalogProvider: catalogProvider,
                    assistantPlanner: OnDeviceAssistantPlanner(),
                    aprsController: radioController,
                    ifDSPStream: IFDSPAudioStreamService(),
                    ifDSPModeController: radioController,
                    initialCatalog: catalogProvider.initialCatalog
                )
            )
            azimuthAppLog.notice(
                "[Azimuth App] composition ready settings=\(records.count, privacy: .public)"
            )
        } catch {
            // A generated-core/catalog mismatch is a build integrity failure,
            // not a condition where the shipping app should show preview data.
            azimuthAppLog.fault("[Azimuth App] composition failed: \(error.localizedDescription, privacy: .private)")
            fatalError("AzimuthCore composition failed: \(error)")
        }
    }

    var body: some Scene {
        WindowGroup {
            AzimuthShell()
                .environment(model)
        }
        #if os(macOS)
        .defaultSize(width: 1180, height: 820)
        .commands {
            CommandMenu("Navigate") {
                Button("Radio") { model.route = .radio }
                    .keyboardShortcut("1", modifiers: .command)
                Button("APRS") { model.route = .aprs }
                    .keyboardShortcut("2", modifiers: .command)
                Button("IF-DSP") { model.route = .ifDSP }
                    .keyboardShortcut("3", modifiers: .command)
                Button("Settings") { model.route = .settings }
                    .keyboardShortcut("4", modifiers: .command)
                Button("Assistant") { model.route = .assistant }
                    .keyboardShortcut("5", modifiers: .command)
                Button("Learn") { model.route = .learn }
                    .keyboardShortcut("6", modifiers: .command)
            }
        }
        #endif
    }
}
