// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation
import SwiftUI

/// A passband expressed as offsets from the TH-D75's 12 kHz USB IF center.
///
/// Positive offsets are above the IF center (USB), negative offsets are below
/// it (LSB), and a two-sided range can represent AM or a CW window.
struct IFDSPPassband: Equatable, Sendable {
    let lowerOffsetHz: Float
    let upperOffsetHz: Float

    init(lowerOffsetHz: Float, upperOffsetHz: Float) {
        self.lowerOffsetHz = min(lowerOffsetHz, upperOffsetHz)
        self.upperOffsetHz = max(lowerOffsetHz, upperOffsetHz)
    }
}

/// An honest plot of one physical IF spectrum.
///
/// `levelsDBFS` contains bins in ascending frequency order. The frequency of
/// bin zero is `12_000 + firstBinOffsetHz`; adjacent bins are `binWidthHz`
/// apart. With an empty array the view draws only its calibrated axes and an
/// explicit no-samples state, never a generated trace.
struct IFDSPSpectrumPlot: View {
    let levelsDBFS: [Float]
    let firstBinOffsetHz: Float
    let binWidthHz: Float
    var passband: IFDSPPassband?
    var floorDBFS: Float = -120
    var ceilingDBFS: Float = 0
    var height: CGFloat = 230

    private let ifCenterHz: Float = 12_000

    var body: some View {
        Canvas(opaque: false, colorMode: .nonLinear, rendersAsynchronously: true) { context, size in
            let plot = IFDSPChartGeometry.plotRect(in: size)
            drawBackground(context: &context, plot: plot)
            drawPassband(context: &context, plot: plot)
            drawGrid(context: &context, plot: plot)

            if !hasRenderableSpectrum {
                drawEmptyState(context: &context, plot: plot)
            } else {
                drawSpectrum(context: &context, plot: plot)
            }
        }
        .frame(height: height)
        .background(
            AzimuthPalette.instrumentBlack,
            in: RoundedRectangle(cornerRadius: 12, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(.white.opacity(0.12))
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("IF spectrum")
        .accessibilityValue(accessibilityValue)
    }

    private var frequencyBounds: ClosedRange<Float> {
        guard levelsDBFS.count > 1, binWidthHz > 0, binWidthHz.isFinite else {
            return 0...24_000
        }
        let first = ifCenterHz + firstBinOffsetHz
        let last = first + Float(levelsDBFS.count - 1) * binWidthHz
        guard first.isFinite, last.isFinite, last > first else { return 0...24_000 }
        return first...last
    }

    private var levelBounds: ClosedRange<Float> {
        IFDSPChartGeometry.validLevelBounds(floor: floorDBFS, ceiling: ceilingDBFS)
    }

    private var hasRenderableSpectrum: Bool {
        levelsDBFS.lazy.filter(\.isFinite).prefix(2).count == 2
    }

    private var accessibilityValue: String {
        guard let peak = measuredPeak else { return "No IF samples" }
        return String(
            format: "Peak %.1f kilohertz at %.1f decibels full scale",
            peak.frequencyHz / 1_000,
            peak.levelDBFS
        )
    }

    private var measuredPeak: (frequencyHz: Float, levelDBFS: Float)? {
        guard hasRenderableSpectrum else { return nil }
        var bestIndex: Int?
        var bestLevel = -Float.infinity
        for (index, level) in levelsDBFS.enumerated() where level.isFinite {
            if level > bestLevel {
                bestIndex = index
                bestLevel = level
            }
        }
        guard let bestIndex else { return nil }
        return (
            ifCenterHz + firstBinOffsetHz + Float(bestIndex) * binWidthHz,
            bestLevel
        )
    }

    private func drawBackground(context: inout GraphicsContext, plot: CGRect) {
        context.fill(
            Path(roundedRect: plot, cornerRadius: 5),
            with: .color(.black.opacity(0.30))
        )
    }

    private func drawPassband(context: inout GraphicsContext, plot: CGRect) {
        guard let passband else { return }
        let lower = ifCenterHz + passband.lowerOffsetHz
        let upper = ifCenterHz + passband.upperOffsetHz
        let lowerX = IFDSPChartGeometry.x(
            frequencyHz: lower,
            bounds: frequencyBounds,
            plot: plot
        )
        let upperX = IFDSPChartGeometry.x(
            frequencyHz: upper,
            bounds: frequencyBounds,
            plot: plot
        )
        let clippedLower = max(plot.minX, min(lowerX, upperX))
        let clippedUpper = min(plot.maxX, max(lowerX, upperX))
        guard clippedUpper > clippedLower else { return }

        let rect = CGRect(
            x: clippedLower,
            y: plot.minY,
            width: clippedUpper - clippedLower,
            height: plot.height
        )
        context.fill(Path(rect), with: .color(AzimuthPalette.signal.opacity(0.10)))
        context.stroke(
            Path(rect),
            with: .color(AzimuthPalette.signal.opacity(0.58)),
            style: StrokeStyle(lineWidth: 1, dash: [4, 3])
        )
    }

    private func drawGrid(context: inout GraphicsContext, plot: CGRect) {
        IFDSPChartDrawing.drawFrequencyGrid(
            context: &context,
            plot: plot,
            bounds: frequencyBounds,
            centerHz: ifCenterHz
        )
        IFDSPChartDrawing.drawLevelGrid(
            context: &context,
            plot: plot,
            bounds: levelBounds
        )
    }

    private func drawEmptyState(context: inout GraphicsContext, plot: CGRect) {
        context.draw(
            Text("NO IF SAMPLES")
                .font(.caption.bold().monospaced())
                .foregroundStyle(.white.opacity(0.55)),
            at: CGPoint(x: plot.midX, y: plot.midY),
            anchor: .center
        )
    }

    private func drawSpectrum(context: inout GraphicsContext, plot: CGRect) {
        guard hasRenderableSpectrum else {
            drawEmptyState(context: &context, plot: plot)
            return
        }
        var line = Path()
        var fill = Path()
        var hasPoint = false

        for (index, level) in levelsDBFS.enumerated() where level.isFinite {
            let frequency = ifCenterHz + firstBinOffsetHz + Float(index) * binWidthHz
            let point = CGPoint(
                x: IFDSPChartGeometry.x(
                    frequencyHz: frequency,
                    bounds: frequencyBounds,
                    plot: plot
                ),
                y: IFDSPChartGeometry.y(levelDBFS: level, bounds: levelBounds, plot: plot)
            )
            if hasPoint {
                line.addLine(to: point)
                fill.addLine(to: point)
            } else {
                line.move(to: point)
                fill.move(to: CGPoint(x: point.x, y: plot.maxY))
                fill.addLine(to: point)
                hasPoint = true
            }
        }

        guard hasPoint else {
            drawEmptyState(context: &context, plot: plot)
            return
        }
        fill.addLine(to: CGPoint(x: line.currentPoint?.x ?? plot.maxX, y: plot.maxY))
        fill.closeSubpath()

        context.fill(
            fill,
            with: .linearGradient(
                Gradient(colors: [
                    AzimuthPalette.signal.opacity(0.30),
                    AzimuthPalette.signal.opacity(0.01),
                ]),
                startPoint: CGPoint(x: plot.midX, y: plot.minY),
                endPoint: CGPoint(x: plot.midX, y: plot.maxY)
            )
        )
        context.stroke(
            line,
            with: .color(AzimuthPalette.signal),
            style: StrokeStyle(lineWidth: 1.5, lineJoin: .round)
        )
    }
}

/// A bounded waterfall made exclusively from supplied physical spectrum rows.
///
/// Rows are expected oldest-to-newest. The newest retained row appears at the
/// bottom, adjacent to the frequency axis. Rows beyond `maximumRows` are
/// discarded from the old end for rendering; no data is synthesized to fill
/// missing rows or bins.
struct IFDSPWaterfallPlot: View {
    let rowsDBFS: [[Float]]
    let firstBinOffsetHz: Float
    let binWidthHz: Float
    var passband: IFDSPPassband?
    var maximumRows: Int = 120
    var floorDBFS: Float = -120
    var ceilingDBFS: Float = 0
    var height: CGFloat = 280

    private let ifCenterHz: Float = 12_000

    var body: some View {
        Canvas(opaque: false, colorMode: .nonLinear, rendersAsynchronously: true) { context, size in
            let plot = IFDSPChartGeometry.plotRect(in: size)
            context.fill(
                Path(roundedRect: plot, cornerRadius: 5),
                with: .color(.black.opacity(0.64))
            )

            if retainedRows.isEmpty {
                drawEmptyState(context: &context, plot: plot)
            } else {
                drawRows(context: &context, plot: plot)
            }

            IFDSPChartDrawing.drawFrequencyGrid(
                context: &context,
                plot: plot,
                bounds: frequencyBounds,
                centerHz: ifCenterHz,
                horizontalOpacity: 0
            )
            drawPassbandOutline(context: &context, plot: plot)
        }
        .frame(height: height)
        .background(
            AzimuthPalette.instrumentBlack,
            in: RoundedRectangle(cornerRadius: 12, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(.white.opacity(0.12))
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("IF waterfall")
        .accessibilityValue(
            retainedRows.isEmpty
                ? "No IF samples"
                : "\(retainedRows.count) measured spectrum rows, newest at bottom"
        )
    }

    private var retainedRows: [[Float]] {
        guard maximumRows > 0 else { return [] }
        return Array(rowsDBFS.suffix(maximumRows))
    }

    private var largestBinCount: Int {
        retainedRows.lazy.map(\.count).max() ?? 0
    }

    private var frequencyBounds: ClosedRange<Float> {
        guard largestBinCount > 1, binWidthHz > 0, binWidthHz.isFinite else {
            return 0...24_000
        }
        let first = ifCenterHz + firstBinOffsetHz
        let last = first + Float(largestBinCount - 1) * binWidthHz
        guard first.isFinite, last.isFinite, last > first else { return 0...24_000 }
        return first...last
    }

    private var levelBounds: ClosedRange<Float> {
        IFDSPChartGeometry.validLevelBounds(floor: floorDBFS, ceiling: ceilingDBFS)
    }

    private func drawRows(context: inout GraphicsContext, plot: CGRect) {
        let retained = retainedRows
        guard let raster = IFDSPWaterfallRaster.makeImage(
            rowsDBFS: retained,
            levelBounds: levelBounds
        ) else { return }
        context.draw(
            Image(decorative: raster, scale: 1, orientation: .up),
            in: plot
        )
    }

    private func drawPassbandOutline(context: inout GraphicsContext, plot: CGRect) {
        guard let passband else { return }
        let lowerX = IFDSPChartGeometry.x(
            frequencyHz: ifCenterHz + passband.lowerOffsetHz,
            bounds: frequencyBounds,
            plot: plot
        )
        let upperX = IFDSPChartGeometry.x(
            frequencyHz: ifCenterHz + passband.upperOffsetHz,
            bounds: frequencyBounds,
            plot: plot
        )
        for x in [lowerX, upperX] where plot.minX...plot.maxX ~= x {
            var edge = Path()
            edge.move(to: CGPoint(x: x, y: plot.minY))
            edge.addLine(to: CGPoint(x: x, y: plot.maxY))
            context.stroke(
                edge,
                with: .color(AzimuthPalette.signal.opacity(0.72)),
                style: StrokeStyle(lineWidth: 1, dash: [3, 3])
            )
        }
    }

    private func drawEmptyState(context: inout GraphicsContext, plot: CGRect) {
        context.draw(
            Text("WATERFALL WAITING FOR IF SAMPLES")
                .font(.caption.bold().monospaced())
                .foregroundStyle(.white.opacity(0.55)),
            at: CGPoint(x: plot.midX, y: plot.midY),
            anchor: .center
        )
    }
}

/// A calibrated horizontal level meter. A nil value is rendered as unavailable
/// rather than as silence; callers should pass a measured floor (for example
/// `-120`) when the input is known to contain physical zero-valued samples.
struct IFDSPLevelMeter: View {
    let label: String
    let valueDBFS: Float?
    var peakDBFS: Float?
    var floorDBFS: Float = -60
    var ceilingDBFS: Float = 0

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline) {
                Text(label.uppercased())
                    .font(.caption2.bold().monospaced())
                    .tracking(0.8)
                    .foregroundStyle(.secondary)
                Spacer(minLength: 10)
                Text(valueLabel)
                    .font(.caption.bold().monospaced())
                    .foregroundStyle(valueColor)
            }

            GeometryReader { proxy in
                let width = proxy.size.width
                let fillWidth = width * CGFloat(levelFraction(valueDBFS))
                ZStack(alignment: .leading) {
                    Capsule()
                        .fill(.primary.opacity(0.08))
                    Capsule()
                        .fill(
                            LinearGradient(
                                colors: [
                                    AzimuthPalette.bearing,
                                    AzimuthPalette.signal,
                                    AzimuthPalette.caution,
                                    .red,
                                ],
                                startPoint: .leading,
                                endPoint: .trailing
                            )
                        )
                        .frame(width: fillWidth)

                    ForEach([-48, -36, -24, -12, -6, 0], id: \.self) { tick in
                        if Float(tick) >= levelBounds.lowerBound,
                           Float(tick) <= levelBounds.upperBound {
                            Rectangle()
                                .fill(.white.opacity(tick == 0 ? 0.65 : 0.24))
                                .frame(width: 1)
                                .offset(x: width * CGFloat(levelFraction(Float(tick))))
                        }
                    }

                    if let peakDBFS, peakDBFS.isFinite {
                        Rectangle()
                            .fill(.white.opacity(0.92))
                            .frame(width: 2)
                            .offset(x: width * CGFloat(levelFraction(peakDBFS)) - 1)
                    }
                }
            }
            .frame(height: 12)

            HStack(spacing: 0) {
                ForEach(0...4, id: \.self) { index in
                    Text(axisLabel(at: index, count: 4))
                    if index < 4 { Spacer(minLength: 0) }
                }
            }
            .font(.system(size: 8, weight: .semibold, design: .monospaced))
            .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(label) level")
        .accessibilityValue(valueDBFS.map(Self.dbLabel) ?? "Unavailable")
    }

    private var levelBounds: ClosedRange<Float> {
        IFDSPChartGeometry.validLevelBounds(floor: floorDBFS, ceiling: ceilingDBFS)
    }

    private var valueLabel: String {
        valueDBFS.map(Self.dbLabel) ?? "UNAVAILABLE"
    }

    private var valueColor: Color {
        guard let valueDBFS else { return .secondary }
        if valueDBFS >= -1 { return .red }
        if valueDBFS >= -12 { return AzimuthPalette.caution }
        return AzimuthPalette.signal
    }

    private func levelFraction(_ level: Float?) -> Float {
        guard let level, level.isFinite else { return 0 }
        let bounds = levelBounds
        let clamped = min(max(level, bounds.lowerBound), bounds.upperBound)
        return (clamped - bounds.lowerBound) / (bounds.upperBound - bounds.lowerBound)
    }

    private func axisLabel(at index: Int, count: Int) -> String {
        let bounds = levelBounds
        let fraction = Float(index) / Float(count)
        let value = bounds.lowerBound
            + fraction * (bounds.upperBound - bounds.lowerBound)
        return index == count
            ? String(format: "%.0f dBFS", value)
            : String(format: "%.0f", value)
    }

    private static func dbLabel(_ value: Float) -> String {
        String(format: "%.1f dBFS", value)
    }
}

private enum IFDSPChartGeometry {
    static func plotRect(in size: CGSize) -> CGRect {
        let left: CGFloat = 45
        let right: CGFloat = 12
        let top: CGFloat = 12
        let bottom: CGFloat = 29
        return CGRect(
            x: left,
            y: top,
            width: max(size.width - left - right, 1),
            height: max(size.height - top - bottom, 1)
        )
    }

    static func validLevelBounds(floor: Float, ceiling: Float) -> ClosedRange<Float> {
        guard floor.isFinite, ceiling.isFinite, ceiling > floor else { return -120...0 }
        return floor...ceiling
    }

    static func x(
        frequencyHz: Float,
        bounds: ClosedRange<Float>,
        plot: CGRect
    ) -> CGFloat {
        let span = max(bounds.upperBound - bounds.lowerBound, 1)
        let fraction = (frequencyHz - bounds.lowerBound) / span
        return plot.minX + CGFloat(fraction) * plot.width
    }

    static func y(
        levelDBFS: Float,
        bounds: ClosedRange<Float>,
        plot: CGRect
    ) -> CGFloat {
        let clamped = min(max(levelDBFS, bounds.lowerBound), bounds.upperBound)
        let fraction = (clamped - bounds.lowerBound)
            / (bounds.upperBound - bounds.lowerBound)
        return plot.maxY - CGFloat(fraction) * plot.height
    }
}

private enum IFDSPChartDrawing {
    static let fixedFrequencyTicks: [Float] = [0, 6_000, 12_000, 18_000, 24_000]

    static func drawFrequencyGrid(
        context: inout GraphicsContext,
        plot: CGRect,
        bounds: ClosedRange<Float>,
        centerHz: Float,
        horizontalOpacity: Double = 0.14
    ) {
        for tick in fixedFrequencyTicks where bounds.contains(tick) {
            let x = IFDSPChartGeometry.x(frequencyHz: tick, bounds: bounds, plot: plot)
            var line = Path()
            line.move(to: CGPoint(x: x, y: plot.minY))
            line.addLine(to: CGPoint(x: x, y: plot.maxY))
            let isCenter = abs(tick - centerHz) < 0.5
            context.stroke(
                line,
                with: .color(
                    isCenter
                        ? AzimuthPalette.bearing.opacity(0.78)
                        : Color.white.opacity(horizontalOpacity)
                ),
                style: StrokeStyle(lineWidth: isCenter ? 1.25 : 0.75)
            )

            let label = isCenter ? "12 kHz IF" : frequencyLabel(tick)
            context.draw(
                Text(label)
                    .font(.system(size: 9, weight: isCenter ? .bold : .medium, design: .monospaced))
                    .foregroundStyle(
                        isCenter ? AzimuthPalette.bearing : Color.white.opacity(0.58)
                    ),
                at: CGPoint(x: x, y: plot.maxY + 15),
                anchor: .center
            )
        }
    }

    static func drawLevelGrid(
        context: inout GraphicsContext,
        plot: CGRect,
        bounds: ClosedRange<Float>
    ) {
        let tickCount = 4
        for index in 0...tickCount {
            let fraction = Float(index) / Float(tickCount)
            let level = bounds.lowerBound
                + fraction * (bounds.upperBound - bounds.lowerBound)
            let y = IFDSPChartGeometry.y(levelDBFS: level, bounds: bounds, plot: plot)
            var line = Path()
            line.move(to: CGPoint(x: plot.minX, y: y))
            line.addLine(to: CGPoint(x: plot.maxX, y: y))
            context.stroke(
                line,
                with: .color(.white.opacity(0.14)),
                style: StrokeStyle(lineWidth: 0.75)
            )
            context.draw(
                Text(String(format: "%.0f", level))
                    .font(.system(size: 9, weight: .medium, design: .monospaced))
                    .foregroundStyle(.white.opacity(0.58)),
                at: CGPoint(x: plot.minX - 7, y: y),
                anchor: .trailing
            )
        }
        context.draw(
            Text("dBFS")
                .font(.system(size: 8, weight: .bold, design: .monospaced))
                .foregroundStyle(.white.opacity(0.48)),
            at: CGPoint(x: 9, y: plot.midY),
            anchor: .center
        )
    }

    private static func frequencyLabel(_ frequencyHz: Float) -> String {
        if abs(frequencyHz) < 0.5 { return "0" }
        return String(format: "%.0fk", frequencyHz / 1_000)
    }
}

/// Converts measured spectrum rows to one RGBA raster. The asynchronous Canvas
/// scales this single image instead of constructing tens of thousands of
/// SwiftUI paths and colors for every refresh.
enum IFDSPWaterfallRaster {
    static func makeImage(
        rowsDBFS: [[Float]],
        levelBounds: ClosedRange<Float>
    ) -> CGImage? {
        let width = rowsDBFS.lazy.map(\.count).max() ?? 0
        let height = rowsDBFS.count
        guard width > 0, height > 0 else { return nil }

        var pixels = [UInt8](repeating: 0, count: width * height * 4)
        for (rowIndex, row) in rowsDBFS.enumerated() {
            for (columnIndex, level) in row.enumerated() where level.isFinite {
                let pixelOffset = (rowIndex * width + columnIndex) * 4
                let color = rgba(for: level, bounds: levelBounds)
                pixels[pixelOffset] = color.red
                pixels[pixelOffset + 1] = color.green
                pixels[pixelOffset + 2] = color.blue
                pixels[pixelOffset + 3] = 255
            }
        }

        let data = Data(pixels) as CFData
        guard let provider = CGDataProvider(data: data) else { return nil }
        return CGImage(
            width: width,
            height: height,
            bitsPerComponent: 8,
            bitsPerPixel: 32,
            bytesPerRow: width * 4,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
            provider: provider,
            decode: nil,
            shouldInterpolate: false,
            intent: .defaultIntent
        )
    }

    private static func rgba(
        for level: Float,
        bounds: ClosedRange<Float>
    ) -> (red: UInt8, green: UInt8, blue: UInt8) {
        let clamped = min(max(level, bounds.lowerBound), bounds.upperBound)
        let fraction = (clamped - bounds.lowerBound)
            / (bounds.upperBound - bounds.lowerBound)

        switch fraction {
        case ..<0.20:
            return interpolate(
                fraction / 0.20,
                from: (0.01, 0.02, 0.05),
                to: (0.05, 0.12, 0.32)
            )
        case ..<0.45:
            return interpolate(
                (fraction - 0.20) / 0.25,
                from: (0.05, 0.12, 0.32),
                to: (0.10, 0.63, 0.91)
            )
        case ..<0.70:
            return interpolate(
                (fraction - 0.45) / 0.25,
                from: (0.10, 0.63, 0.91),
                to: (0.20, 0.92, 0.72)
            )
        case ..<0.88:
            return interpolate(
                (fraction - 0.70) / 0.18,
                from: (0.20, 0.92, 0.72),
                to: (1.00, 0.67, 0.20)
            )
        default:
            return interpolate(
                (fraction - 0.88) / 0.12,
                from: (1.00, 0.67, 0.20),
                to: (1.00, 0.20, 0.18)
            )
        }
    }

    private static func interpolate(
        _ rawFraction: Float,
        from: (Float, Float, Float),
        to: (Float, Float, Float)
    ) -> (red: UInt8, green: UInt8, blue: UInt8) {
        let fraction = min(max(rawFraction, 0), 1)
        return (
            component(from.0 + (to.0 - from.0) * fraction),
            component(from.1 + (to.1 - from.1) * fraction),
            component(from.2 + (to.2 - from.2) * fraction)
        )
    }

    private static func component(_ value: Float) -> UInt8 {
        UInt8((min(max(value, 0), 1) * 255).rounded())
    }
}
