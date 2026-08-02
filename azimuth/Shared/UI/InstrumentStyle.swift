// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import SwiftUI

enum AzimuthPalette {
    static let signal = Color(red: 0.20, green: 0.92, blue: 0.72)
    static let bearing = Color(red: 0.28, green: 0.68, blue: 1.00)
    static let caution = Color(red: 1.00, green: 0.67, blue: 0.20)
    static let instrumentBlack = Color(red: 0.035, green: 0.045, blue: 0.055)
    static let screenGreen = Color(red: 0.56, green: 0.96, blue: 0.67)
}

enum AzimuthLayout {
    static let workspaceWidth: CGFloat = 1180
    static let browseWidth: CGFloat = 1000
    static let standardWidth: CGFloat = 900
    static let readingWidth: CGFloat = 820
    static let pageGutter: CGFloat = 20
    static let pageSpacing: CGFloat = 16
    static let cardSpacing: CGFloat = 12
    static let panelRadius: CGFloat = 16
}

struct AzimuthPageBackground: View {
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        ZStack {
            (colorScheme == .dark
                ? Color(red: 0.035, green: 0.045, blue: 0.06)
                : Color(red: 0.94, green: 0.955, blue: 0.97))

            RadialGradient(
                colors: [AzimuthPalette.bearing.opacity(colorScheme == .dark ? 0.16 : 0.10), .clear],
                center: .topLeading,
                startRadius: 20,
                endRadius: 560
            )
            RadialGradient(
                colors: [AzimuthPalette.signal.opacity(colorScheme == .dark ? 0.09 : 0.07), .clear],
                center: .bottomTrailing,
                startRadius: 30,
                endRadius: 520
            )
        }
        .ignoresSafeArea()
    }
}

struct InstrumentPanel<Content: View>: View {
    private let content: Content
    private let padding: CGFloat

    init(padding: CGFloat = 18, @ViewBuilder content: () -> Content) {
        self.padding = padding
        self.content = content()
    }

    var body: some View {
        content
            .frame(maxWidth: .infinity, alignment: .topLeading)
            .padding(padding)
            .background(
                .regularMaterial,
                in: RoundedRectangle(cornerRadius: AzimuthLayout.panelRadius, style: .continuous)
            )
            .overlay {
                RoundedRectangle(cornerRadius: AzimuthLayout.panelRadius, style: .continuous)
                    .strokeBorder(.primary.opacity(0.08), lineWidth: 1)
            }
            .shadow(color: .black.opacity(0.07), radius: 10, y: 4)
    }
}

struct AzimuthEyebrow: View {
    let text: String

    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text.uppercased())
            .font(.caption2.weight(.bold).monospaced())
            .tracking(1.7)
            .foregroundStyle(AzimuthPalette.bearing)
    }
}

struct AzimuthStatusPill: View {
    let title: String
    let symbol: String
    let color: Color

    var body: some View {
        Label(title, systemImage: symbol)
            .font(.caption.weight(.semibold))
            .foregroundStyle(color)
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .background(color.opacity(0.12), in: Capsule())
            .overlay { Capsule().strokeBorder(color.opacity(0.22)) }
    }
}

struct AzimuthMetric: View {
    let label: String
    let value: String
    var tint: Color = .primary

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(label.uppercased())
                .font(.caption2.weight(.bold).monospaced())
                .tracking(0.8)
                .foregroundStyle(.secondary)
            Text(value)
                .font(.body.weight(.semibold).monospaced())
                .foregroundStyle(tint)
                .lineLimit(1)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

struct AzimuthWordmark: View {
    var compact = false

    var body: some View {
        HStack(spacing: compact ? 8 : 11) {
            ZStack {
                Circle()
                    .stroke(AzimuthPalette.bearing.opacity(0.45), lineWidth: 1)
                Circle()
                    .trim(from: 0.08, to: 0.72)
                    .stroke(
                        AngularGradient(
                            colors: [AzimuthPalette.signal, AzimuthPalette.bearing],
                            center: .center
                        ),
                        style: StrokeStyle(lineWidth: 2.5, lineCap: .round)
                    )
                    .rotationEffect(.degrees(-38))
                Path { path in
                    path.move(to: CGPoint(x: 15, y: 19))
                    path.addLine(to: CGPoint(x: 22, y: 10))
                }
                .stroke(AzimuthPalette.signal, style: StrokeStyle(lineWidth: 2, lineCap: .round))
            }
            .frame(width: compact ? 26 : 34, height: compact ? 26 : 34)

            VStack(alignment: .leading, spacing: 0) {
                Text("AZIMUTH")
                    .font((compact ? Font.subheadline : .headline).weight(.black))
                    .tracking(compact ? 1.4 : 2.1)
                if !compact {
                    Text("TH-D75 CONTROL INSTRUMENT")
                        .font(.system(size: 8, weight: .bold, design: .monospaced))
                        .tracking(0.9)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Azimuth, TH-D75 control instrument")
    }
}

extension View {
    func azimuthPage() -> some View {
        background { AzimuthPageBackground() }
    }

    func azimuthContentColumn(maxWidth: CGFloat) -> some View {
        frame(maxWidth: maxWidth, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .center)
            .padding(.horizontal, AzimuthLayout.pageGutter)
            .padding(.vertical, AzimuthLayout.pageSpacing)
    }
}
