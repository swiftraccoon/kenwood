#![expect(
    clippy::too_many_lines,
    reason = "APRS pane render functions draw complete UI sections (station list with \
              columns, message history with statuses, digipeater path spans) — Ratatui's \
              immediate-mode API means each visible cell is an explicit construction \
              call. Splitting per-section helpers would move the layout logic away from \
              the constraints that bound it, making the view harder to read end-to-end."
)]

use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, AprsMessageState, AprsMode, McpState, Pane};

/// Format a duration since `then` as a human-readable "ago" string.
fn ago(then: Instant) -> String {
    let secs = then.elapsed().as_secs();
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else {
        format!("{}h ago", secs / 3600)
    }
}

/// Format latitude/longitude for display.
fn fmt_lat(lat: f64) -> String {
    let ns = if lat >= 0.0 { 'N' } else { 'S' };
    format!("{:.3}\u{00b0}{ns}", lat.abs())
}

fn fmt_lon(lon: f64) -> String {
    let ew = if lon >= 0.0 { 'E' } else { 'W' };
    format!("{:.3}\u{00b0}{ew}", lon.abs())
}

fn kv_line(label: &str, value: String, value_color: Color) -> Line<'_> {
    Line::from(vec![
        Span::styled(
            format!("  {label:<22}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(value, Style::default().fg(value_color)),
    ])
}

pub(crate) fn render(app: &App, frame: &mut Frame<'_>, list_area: Rect, detail_area: Rect) {
    match app.aprs_mode {
        AprsMode::Active => render_live(app, frame, list_area, detail_area),
        AprsMode::Inactive => render_mcp_config(app, frame, list_area, detail_area),
    }
}

// ---------------------------------------------------------------------------
// Live APRS view (KISS mode active)
// ---------------------------------------------------------------------------

fn render_live(app: &App, frame: &mut Frame<'_>, list_area: Rect, detail_area: Rect) {
    // --- Left pane: station list ---
    let station_count = app.aprs_stations.len();
    let title = format!(" APRS Stations ({station_count} heard) ");

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Main));

    let mut lines: Vec<Line<'_>> = Vec::new();

    if app.aprs_stations.is_empty() {
        lines.push(Line::from(Span::styled(
            "  Listening...",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        // Compute visible range for scrolling.
        let visible_height = list_area.height.saturating_sub(2) as usize; // borders
        let start = if app.aprs_station_index >= visible_height {
            app.aprs_station_index - visible_height + 1
        } else {
            0
        };
        let end = (start + visible_height).min(station_count);

        for (i, station) in app.aprs_stations[start..end].iter().enumerate() {
            let idx = start + i;
            let is_selected = idx == app.aprs_station_index;

            let callsign = format!("{:<10}", station.callsign);
            let pos = match (station.latitude, station.longitude) {
                (Some(lat), Some(lon)) => format!("{} {}", fmt_lat(lat), fmt_lon(lon)),
                _ => "No position".to_string(),
            };
            let time = ago(station.last_heard);

            let style = if is_selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            let marker = if is_selected { ">" } else { " " };
            lines.push(Line::from(Span::styled(
                format!(" {marker} {callsign} {pos:<26} {time}"),
                style,
            )));
        }
    }

    // Compose prompt overlay
    if let Some(ref buf) = app.aprs_compose {
        lines.push(Line::from(""));
        let target = app
            .aprs_stations
            .get(app.aprs_station_index)
            .map_or("?", |s| s.callsign.as_str());
        lines.push(Line::from(vec![
            Span::styled(
                format!("  Msg to {target}: "),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(format!("{buf}_"), Style::default().fg(Color::White)),
        ]));
    }

    // Footer hint
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " [j/k] Navigate  [M] Message  [b] Beacon  [a] Stop APRS",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(Paragraph::new(lines).block(block), list_area);

    // --- Right pane: station detail + messages ---
    let detail_block = Block::default()
        .title(" Station Detail ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Detail));

    let mut detail_lines: Vec<Line<'_>> = Vec::new();

    if let Some(station) = app.aprs_stations.get(app.aprs_station_index) {
        detail_lines.push(Line::from(Span::styled(
            format!(" {}", station.callsign),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        detail_lines.push(Line::from(""));

        // Position
        match (station.latitude, station.longitude) {
            (Some(lat), Some(lon)) => {
                detail_lines.push(kv_line(
                    "Position",
                    format!("{} {}", fmt_lat(lat), fmt_lon(lon)),
                    Color::White,
                ));
            }
            _ => {
                detail_lines.push(kv_line("Position", "Unknown".into(), Color::DarkGray));
            }
        }

        // Speed / course
        if let Some(speed) = station.speed_knots {
            let mph = f64::from(speed) * 1.15078;
            let course_str = station
                .course_degrees
                .map_or(String::new(), |c| format!(" heading {c}\u{00b0}"));
            detail_lines.push(kv_line(
                "Speed",
                format!("{mph:.0} mph{course_str}"),
                Color::White,
            ));
        }

        // Last heard
        detail_lines.push(kv_line("Last heard", ago(station.last_heard), Color::White));

        // Packet count
        detail_lines.push(kv_line(
            "Packets",
            station.packet_count.to_string(),
            Color::White,
        ));

        // Digipeater path
        if !station.last_path.is_empty() {
            detail_lines.push(kv_line("Path", station.last_path.join(","), Color::White));
        }

        // Symbol
        if let (Some(tbl), Some(code)) = (station.symbol_table, station.symbol_code) {
            detail_lines.push(kv_line("Symbol", format!("{tbl}{code}"), Color::White));
        }

        // Comment
        if let Some(ref comment) = station.comment {
            detail_lines.push(kv_line("Comment", comment.clone(), Color::White));
        }

        // --- Messages section ---
        let pending = app
            .aprs_messages
            .iter()
            .filter(|m| m.state == AprsMessageState::Pending)
            .count();
        let delivered = app
            .aprs_messages
            .iter()
            .filter(|m| m.state == AprsMessageState::Delivered)
            .count();

        if !app.aprs_messages.is_empty() {
            detail_lines.push(Line::from(""));
            detail_lines.push(Line::from(Span::styled(
                format!(" Messages ({pending} pending, {delivered} delivered):"),
                Style::default().fg(Color::Yellow),
            )));

            // Show most recent messages (up to 10).
            for msg in app.aprs_messages.iter().rev().take(10) {
                let (status_str, color) = match msg.state {
                    AprsMessageState::Pending => ("...", Color::Yellow),
                    AprsMessageState::Delivered => ("ack", Color::Green),
                    AprsMessageState::Rejected => ("rej", Color::Red),
                    AprsMessageState::Expired => ("exp", Color::Red),
                };
                detail_lines.push(Line::from(vec![
                    Span::styled(
                        format!("  -> {}: ", msg.addressee),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(msg.text.clone(), Style::default().fg(Color::White)),
                    Span::styled(format!(" [{status_str}]"), Style::default().fg(color)),
                ]));
            }
        }
    } else {
        detail_lines.push(Line::from(Span::styled(
            " No station selected",
            Style::default().fg(Color::DarkGray),
        )));
    }

    frame.render_widget(
        Paragraph::new(detail_lines).block(detail_block),
        detail_area,
    );
}

// ---------------------------------------------------------------------------
// MCP config view (KISS not active)
// ---------------------------------------------------------------------------

fn render_mcp_config(app: &App, frame: &mut Frame<'_>, list_area: Rect, detail_area: Rect) {
    let block = Block::default()
        .title(" APRS ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Main));

    let detail_block = Block::default()
        .title(" APRS Region Info ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Detail));

    let McpState::Loaded { ref image, .. } = app.mcp else {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(" No MCP data loaded."),
                Line::from(" Press [m] then [r] to read from radio."),
                Line::from(""),
                Line::from(Span::styled(
                    " Press [a] to enter live APRS mode.",
                    Style::default().fg(Color::Cyan),
                )),
            ])
            .block(block),
            list_area,
        );
        frame.render_widget(Paragraph::new("").block(detail_block), detail_area);
        return;
    };

    let aprs = image.aprs();

    // -------------------------------------------------------------------------
    // Left pane: APRS config fields
    // -------------------------------------------------------------------------
    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::from(Span::styled(
        " APRS Configuration",
        Style::default().fg(Color::Yellow),
    )));
    lines.push(Line::from(""));

    {
        let cs = aprs.my_callsign();
        let (disp, col) = if cs.is_empty() {
            ("<not set>".into(), Color::DarkGray)
        } else {
            (cs, Color::Cyan)
        };
        lines.push(kv_line("My callsign", disp, col));
    }

    {
        let interval = aprs.beacon_interval();
        let (disp, col) = if interval == 0 {
            ("Off".into(), Color::DarkGray)
        } else {
            (format!("{interval} s"), Color::White)
        };
        lines.push(kv_line("Beacon interval", disp, col));
    }

    lines.push(kv_line("Packet path", aprs.packet_path(), Color::White));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " Press [a] to enter live APRS mode.",
        Style::default().fg(Color::Cyan),
    )));

    frame.render_widget(Paragraph::new(lines).block(block), list_area);

    // -------------------------------------------------------------------------
    // Right pane: APRS region info
    // -------------------------------------------------------------------------
    let mut detail_lines: Vec<Line<'_>> = Vec::new();

    detail_lines.push(Line::from(Span::styled(
        " APRS Memory Regions",
        Style::default().fg(Color::Yellow),
    )));
    detail_lines.push(Line::from(""));

    let region_info = [
        ("Status header", "0x15100", "256 bytes"),
        ("Data / settings", "0x15200", "~16 KB"),
        ("Position data", "0x25100", "19,200 bytes (confirmed)"),
    ];

    for (name, offset, size) in region_info {
        detail_lines.push(Line::from(vec![
            Span::styled(
                format!("  {name:<18}"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("{offset}  {size}"),
                Style::default().fg(Color::White),
            ),
        ]));
    }

    detail_lines.push(Line::from(""));

    let has_pos = aprs.has_position_data();
    let (pos_str, pos_col) = if has_pos {
        ("Yes", Color::Cyan)
    } else {
        ("No", Color::DarkGray)
    };
    detail_lines.push(Line::from(vec![
        Span::styled(
            "  Position data present  ",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(pos_str, Style::default().fg(pos_col)),
    ]));

    let region_sz = aprs.region_size();
    detail_lines.push(Line::from(vec![
        Span::styled(
            "  Total region size      ",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("{region_sz} bytes"),
            Style::default().fg(Color::White),
        ),
    ]));

    frame.render_widget(
        Paragraph::new(detail_lines).block(detail_block),
        detail_area,
    );
}
