// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import MapKit
import SwiftUI

/// One map pin: latest known position per callsign, plus a live pin
/// for the currently-transmitting station. Pure so tests can pin the
/// dedupe rules without MapKit.
struct StationAnnotationModel: Equatable, Identifiable {
    let id: String
    let callsign: String
    let latitude: Double
    let longitude: Double
    let isLive: Bool
    let station: StationRef

    var coordinate: CLLocationCoordinate2D {
        CLLocationCoordinate2D(latitude: latitude, longitude: longitude)
    }
}

/// The station map. `.canvas` fills the wide layout edge-to-edge under
/// the toolbar; `.card` is the narrow layout's fixed-height tile. The
/// camera auto-fits pins; when the live station reports GPS the camera
/// pans (span preserved, never a destructive zoom) to include it.
struct StationMapPane: View {
    enum Style {
        case canvas
        case card
    }

    let heard: [ReflectorCoordinator.HeardEntry]
    let liveStream: ReflectorCoordinator.StreamSnapshot?
    let dimmed: Bool
    let style: Style
    @Binding var selectedStationID: String?

    /// Never frame tighter than this (~650 km of context). One lone
    /// pin under `.automatic` framing zooms to street level, which is
    /// useless for a reflector network view.
    static let minSpanDegrees: Double = 6
    /// Margin factor applied around the station bounding box.
    private static let fitMargin: Double = 1.4
    /// Empty-state backdrop before anything is heard.
    private static let continentalRegion = MKCoordinateRegion(
        center: CLLocationCoordinate2D(latitude: 39.8, longitude: -98.6),
        span: MKCoordinateSpan(latitudeDelta: 42, longitudeDelta: 60)
    )

    @State private var camera: MapCameraPosition = .region(Self.continentalRegion)
    @State private var lastSpan: MKCoordinateSpan?
    @State private var poppedStation: StationRef?

    /// Region that frames every station with margin, floored at
    /// `minSpanDegrees` per axis. Pure so the framing rules are
    /// unit-tested. Antimeridian wrap is not handled.
    static func fittingRegion(for annotations: [StationAnnotationModel]) -> MKCoordinateRegion? {
        guard let firstLat = annotations.map(\.latitude).min(),
              let lastLat = annotations.map(\.latitude).max(),
              let firstLon = annotations.map(\.longitude).min(),
              let lastLon = annotations.map(\.longitude).max()
        else { return nil }
        return MKCoordinateRegion(
            center: CLLocationCoordinate2D(
                latitude: (firstLat + lastLat) / 2,
                longitude: (firstLon + lastLon) / 2
            ),
            span: MKCoordinateSpan(
                latitudeDelta: max((lastLat - firstLat) * fitMargin, minSpanDegrees),
                longitudeDelta: max((lastLon - firstLon) * fitMargin, minSpanDegrees)
            )
        )
    }

    /// Newest-first `recentlyHeard` order means the first positioned
    /// entry per callsign is its latest fix. The live stream's fix
    /// (when present) supersedes that station's historical pin.
    static func annotations(
        heard: [ReflectorCoordinator.HeardEntry],
        liveStream: ReflectorCoordinator.StreamSnapshot?
    ) -> [StationAnnotationModel] {
        var models: [StationAnnotationModel] = []
        var seen = Set<String>()

        if let live = liveStream, let pos = live.latestPosition {
            models.append(StationAnnotationModel(
                id: "live:\(live.mycall)",
                callsign: live.mycall,
                latitude: pos.latitude,
                longitude: pos.longitude,
                isLive: true,
                station: StationRef(stream: live)
            ))
            seen.insert(live.mycall)
        }

        for entry in heard {
            guard let pos = entry.position, !seen.contains(entry.mycall) else { continue }
            seen.insert(entry.mycall)
            models.append(StationAnnotationModel(
                id: StationRef(entry: entry).id,
                callsign: entry.mycall,
                latitude: pos.latitude,
                longitude: pos.longitude,
                isLive: false,
                station: StationRef(entry: entry)
            ))
        }
        return models
    }

    private var annotations: [StationAnnotationModel] {
        Self.annotations(heard: heard, liveStream: liveStream)
    }

    var body: some View {
        switch style {
        case .canvas:
            mapBody
        case .card:
            mapBody
                .frame(height: 240)
                .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        }
    }

    /// Callsign set driving camera refits. Keyed on callsigns, not
    /// annotation IDs, so a repeat transmission or position refresh
    /// from a known station never yanks a camera the operator has
    /// panned; only a genuinely new station reframes the view.
    private var callsignKey: String {
        annotations.map(\.callsign).sorted().joined(separator: ",")
    }

    private var mapBody: some View {
        Map(position: $camera) {
            ForEach(annotations) { model in
                Annotation(model.callsign, coordinate: model.coordinate) {
                    pin(model)
                }
            }
        }
        // Satellite imagery with labels. Elevation must stay .flat:
        // the .realistic 3D-terrain renderer can fail drawable
        // acquisition under load (camera pan + pulsing annotation +
        // material blur above it) and its fallback path trips a Metal
        // API Validation assertion that aborts debug builds
        // (observed on-device, iPad, 2026-07-19).
        .mapStyle(.hybrid(elevation: .flat))
        .onAppear { refit(animated: false) }
        .onChange(of: callsignKey) { _, _ in
            refit(animated: true)
        }
        .onMapCameraChange { context in
            lastSpan = context.region.span
        }
        .onChange(of: liveStream?.latestPosition) { _, newPosition in
            guard let pos = newPosition else { return }
            // Pan to the transmitting station, preserving the span the
            // operator chose: pan, never destructive zoom.
            let span = lastSpan ?? MKCoordinateSpan(latitudeDelta: 8, longitudeDelta: 8)
            withAnimation(.easeInOut(duration: 0.6)) {
                camera = .region(MKCoordinateRegion(
                    center: CLLocationCoordinate2D(latitude: pos.latitude, longitude: pos.longitude),
                    span: span
                ))
            }
        }
        .overlay {
            if dimmed {
                ZStack {
                    Rectangle().fill(.black.opacity(0.45))
                    Text(annotations.isEmpty
                         ? "Stations you hear will appear here."
                         : "Not linked. Showing last heard positions.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .padding(10)
                        .background(.regularMaterial, in: Capsule())
                }
                .allowsHitTesting(false)
            } else if annotations.isEmpty {
                Text("No positions heard yet.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .padding(10)
                    .background(.regularMaterial, in: Capsule())
                    .allowsHitTesting(false)
            }
        }
        .popover(item: $poppedStation) { station in
            StationPopover(station: station)
        }
    }

    private func refit(animated: Bool) {
        guard let region = Self.fittingRegion(for: annotations) else { return }
        if animated {
            withAnimation(.easeInOut(duration: 0.8)) {
                camera = .region(region)
            }
        } else {
            camera = .region(region)
        }
    }

    private func pin(_ model: StationAnnotationModel) -> some View {
        Button {
            selectedStationID = model.station.id
            poppedStation = model.station
        } label: {
            ZStack {
                if model.isLive {
                    Circle()
                        .fill(.green.opacity(0.35))
                        .frame(width: 30, height: 30)
                        .phaseAnimator([0.6, 1.15]) { view, scale in
                            view.scaleEffect(scale)
                        } animation: { _ in
                            .easeInOut(duration: 0.9)
                        }
                }
                Circle()
                    .fill(model.isLive ? Color.green : Color.white)
                    .stroke(selectedStationID == model.station.id ? Color.accentColor : .black.opacity(0.4),
                            lineWidth: selectedStationID == model.station.id ? 3 : 1)
                    .frame(width: 14, height: 14)
            }
            .contentShape(.circle)
        }
        .buttonStyle(.plain)
        .contextMenu {
            StationActionMenuItems(station: model.station)
        }
        .accessibilityLabel(model.isLive
            ? "\(model.callsign), transmitting now, position reported"
            : "\(model.callsign), position reported")
        .accessibilityHint("Shows station details and actions")
    }
}
