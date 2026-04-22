// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

/// Thin pass-through so `LodestarApp` can keep referring to `ContentView`
/// while the real layout lives in `LodestarShell`.
struct ContentView: View {
    var body: some View {
        LodestarShell()
    }
}

#Preview("macOS") {
    ContentView()
}
