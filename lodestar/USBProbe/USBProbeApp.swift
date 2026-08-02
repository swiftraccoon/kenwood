import SwiftUI

// @MainActor so the @State default-value expression may call ProbeModel's
// MainActor-isolated init under strict concurrency.
@main
@MainActor
struct USBProbeApp: App {
    @State private var model = ProbeModel()

    var body: some Scene {
        WindowGroup {
            ProbeView(model: model)
        }
    }
}
