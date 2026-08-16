use std::time::Instant;

use kenwood_thd75::memory::dstar::DstarReadError;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, DstarMode, McpState, Pane};

/// Format a last-heard entry's age as a human-readable "ago" string.
fn ago(entry: &kenwood_thd75::LastHeardEntry) -> String {
    let secs = entry.age(Instant::now()).as_secs();
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else {
        format!("{}h ago", secs / 3600)
    }
}

fn kv_line(label: &str, value: String, value_color: Color) -> Line<'_> {
    Line::from(vec![
        Span::styled(
            format!("  {label:<18}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(value, Style::default().fg(value_color)),
    ])
}

/// Format a D-STAR callsign for display, splitting the 8-char field into
/// a trimmed callsign and module letter if present.
fn fmt_callsign(cs: &str) -> String {
    let trimmed = cs.trim();
    if trimmed.is_empty() {
        "<not set>".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Render malformed fixed-width wire data without inventing text for it.
fn invalid_wire_bytes(bytes: &[u8], error: kenwood_thd75::WireTextError) -> String {
    let hexadecimal = bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!("<invalid {hexadecimal}: {error}>")
}

fn gateway_callsign(callsign: kenwood_thd75::ObservedDstarCallsign) -> String {
    callsign.text().map_or_else(
        |error| invalid_wire_bytes(callsign.as_bytes(), error),
        str::to_owned,
    )
}

fn gateway_suffix(suffix: kenwood_thd75::dstar_gateway::Suffix) -> String {
    suffix.text().map_or_else(
        |error| invalid_wire_bytes(suffix.as_bytes(), error),
        str::to_owned,
    )
}

fn gateway_text_message(message: &kenwood_thd75::SlowDataTextMessage) -> String {
    message.text().map_or_else(
        |error| invalid_wire_bytes(message.as_bytes(), error),
        str::to_owned,
    )
}

pub(crate) fn render(app: &App, frame: &mut Frame<'_>, list_area: Rect, detail_area: Rect) {
    match app.dstar_mode {
        DstarMode::Active => render_gateway(app, frame, list_area, detail_area),
        DstarMode::Inactive => render_cat_settings(app, frame, list_area, detail_area),
    }
}

// ---------------------------------------------------------------------------
// CAT settings view (gateway not active)
// ---------------------------------------------------------------------------

#[expect(
    clippy::too_many_lines,
    reason = "Draws the complete D-STAR CAT-settings section. Ratatui's immediate-mode \
              API means each visible cell is an explicit construction call; splitting \
              would move layout logic away from the constraints that bound it."
)]
fn render_cat_settings(app: &App, frame: &mut Frame<'_>, list_area: Rect, detail_area: Rect) {
    let block = Block::default()
        .title(" D-STAR Settings ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Main));

    let detail_block = Block::default()
        .title(" Quick Actions ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Detail));

    let mut lines: Vec<Line<'_>> = Vec::new();

    // Show MCP-based MY callsign if available
    if let McpState::Loaded { ref image, .. } = app.mcp {
        let (disp, col) = match image.dstar().my_callsign() {
            Ok(Some(callsign)) => (callsign.as_str().to_owned(), Color::Cyan),
            Ok(None) => ("Not configured (Menu 610)".to_owned(), Color::Yellow),
            Err(error @ DstarReadError::MissingRange { .. }) => {
                (format!("Incomplete MCP data: {error}"), Color::Red)
            }
            Err(error) => (format!("Invalid MCP data: {error}"), Color::Red),
        };
        lines.push(kv_line("MY Callsign", disp, col));
        lines.push(Line::from(""));
    }

    // URCALL from CAT poll
    let urcall_disp = fmt_callsign(&app.state.dstar_urcall);
    let urcall_suffix = app.state.dstar_urcall_suffix.trim().to_string();
    let urcall_full = if urcall_suffix.is_empty() {
        urcall_disp.clone()
    } else {
        format!("{urcall_disp}  {urcall_suffix}")
    };
    let urcall_color = if urcall_disp == "<not set>" {
        Color::DarkGray
    } else {
        Color::Cyan
    };
    lines.push(kv_line("URCALL", urcall_full, urcall_color));

    // RPT1
    let rpt1_disp = fmt_callsign(&app.state.dstar_rpt1);
    let rpt1_color = if rpt1_disp == "<not set>" {
        Color::DarkGray
    } else {
        Color::White
    };
    lines.push(kv_line("RPT1", rpt1_disp, rpt1_color));

    // RPT2
    let rpt2_disp = fmt_callsign(&app.state.dstar_rpt2);
    let rpt2_color = if rpt2_disp == "<not set>" {
        Color::DarkGray
    } else {
        Color::White
    };
    lines.push(kv_line("RPT2", rpt2_disp, rpt2_color));

    lines.push(Line::from(""));

    // Gateway mode
    let gw_str = app
        .state
        .dstar_gateway_mode
        .map_or_else(|| "Unknown".to_string(), |g| format!("{g:?}"));
    lines.push(kv_line("Gateway Mode", gw_str, Color::White));

    // D-STAR slot
    let slot_str = app
        .state
        .dstar_slot
        .map_or_else(|| "Unknown".to_string(), |s| format!("{}", s.as_raw()));
    lines.push(kv_line("D-STAR Slot", slot_str, Color::White));

    // Input prompts
    lines.push(Line::from(""));
    if let Some(ref buf) = app.dstar_urcall_input {
        lines.push(Line::from(vec![
            Span::styled("  URCALL: ", Style::default().fg(Color::Yellow)),
            Span::styled(format!("{buf}_"), Style::default().fg(Color::White)),
        ]));
    } else if let Some(ref buf) = app.dstar_reflector_input {
        lines.push(Line::from(vec![
            Span::styled(
                "  Reflector (e.g. REF030 C): ",
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(format!("{buf}_"), Style::default().fg(Color::White)),
        ]));
    } else {
        lines.push(Line::from(Span::styled(
            " [d] Enter Gateway Mode  [u] Set URCALL",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            " [r] Prepare Link       [U] Prepare Unlink",
            Style::default().fg(Color::DarkGray),
        )));
    }

    frame.render_widget(Paragraph::new(lines).block(block), list_area);

    // --- Right pane: quick actions ---
    let mut detail_lines: Vec<Line<'_>> = Vec::new();

    detail_lines.push(Line::from(Span::styled(
        " D-STAR Quick Actions",
        Style::default().fg(Color::Yellow),
    )));
    detail_lines.push(Line::from(""));

    let actions = [
        ("[C]", "CQ (set URCALL to CQCQCQ)"),
        ("[r]", "Prepare reflector link URCALL"),
        ("[U]", "Prepare reflector unlink URCALL"),
        ("[u]", "Set URCALL manually"),
        ("[d]", "Enter gateway mode (MMDVM)"),
    ];

    for (key, desc) in actions {
        detail_lines.push(Line::from(vec![
            Span::styled(format!("  {key:<6}"), Style::default().fg(Color::Yellow)),
            Span::styled(desc.to_string(), Style::default().fg(Color::White)),
        ]));
    }

    // MCP D-STAR info if available
    if let McpState::Loaded { ref image, .. } = app.mcp {
        let dstar = image.dstar();

        detail_lines.push(Line::from(""));
        detail_lines.push(Line::from(Span::styled(
            " MCP D-STAR Data",
            Style::default().fg(Color::Yellow),
        )));
        detail_lines.push(Line::from(""));
        let (repeater_count, repeater_count_color) = match dstar.repeater_count() {
            Ok(count) => (count.to_string(), Color::White),
            Err(error @ DstarReadError::MissingRange { .. }) => {
                (format!("Incomplete MCP data: {error}"), Color::Red)
            }
            Err(error) => (format!("Invalid MCP data: {error}"), Color::Red),
        };
        detail_lines.push(kv_line("Repeaters", repeater_count, repeater_count_color));

        let region_sz = dstar.region_size();
        detail_lines.push(kv_line(
            "Region Size",
            format!("{region_sz} bytes"),
            Color::White,
        ));
    }

    frame.render_widget(
        Paragraph::new(detail_lines).block(detail_block),
        detail_area,
    );
}

// ---------------------------------------------------------------------------
// Gateway mode view (`DstarGateway` active)
// ---------------------------------------------------------------------------

#[expect(
    clippy::too_many_lines,
    reason = "Draws the complete D-STAR gateway view (session status, stream history, \
              last-heard list). Ratatui's immediate-mode API means each visible cell \
              is an explicit construction call; splitting would move layout logic away \
              from the constraints that bound it."
)]
fn render_gateway(app: &App, frame: &mut Frame<'_>, list_area: Rect, detail_area: Rect) {
    // --- Left pane: last heard list ---
    let count = app.dstar_last_heard.len();
    let title = format!(" D-STAR Gateway ({count} heard) ");

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Main));

    let mut lines: Vec<Line<'_>> = Vec::new();

    if app.dstar_last_heard.is_empty() {
        lines.push(Line::from(Span::styled(
            "  Listening...",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let visible_height = list_area.height.saturating_sub(4) as usize;
        let start = if app.dstar_last_heard_index >= visible_height {
            app.dstar_last_heard_index - visible_height + 1
        } else {
            0
        };
        let end = (start + visible_height).min(count);

        for (i, entry) in app
            .dstar_last_heard
            .get(start..end)
            .unwrap_or(&[])
            .iter()
            .enumerate()
        {
            let idx = start + i;
            let is_selected = idx == app.dstar_last_heard_index;

            let callsign = format!("{:<9}", gateway_callsign(entry.callsign));
            let dest = format!("{:<9}", gateway_callsign(entry.destination));
            let time = ago(entry);

            let style = if is_selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            let marker = if is_selected { ">" } else { " " };
            lines.push(Line::from(Span::styled(
                format!(" {marker} {callsign} -> {dest} {time}"),
                style,
            )));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " [j/k] Navigate  [d] Exit Gateway Mode",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(Paragraph::new(lines).block(block), list_area);

    // --- Right pane: detail ---
    let detail_block = Block::default()
        .title(" Current Transmission ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Detail));

    let mut detail_lines: Vec<Line<'_>> = Vec::new();

    if let Some(ref header) = app.dstar_rx_header {
        let status_str = if app.dstar_rx_active {
            "Receiving voice..."
        } else {
            "Idle"
        };
        let status_color = if app.dstar_rx_active {
            Color::Green
        } else {
            Color::DarkGray
        };

        let my_call = gateway_callsign(header.my_call.into());
        let my_suffix = gateway_suffix(header.my_suffix);
        let from = if my_suffix.is_empty() {
            my_call
        } else {
            format!("{my_call} {my_suffix}")
        };
        detail_lines.push(kv_line("From", from, Color::Cyan));
        detail_lines.push(kv_line(
            "To",
            gateway_callsign(header.ur_call.into()),
            Color::White,
        ));
        detail_lines.push(kv_line(
            "RPT1",
            gateway_callsign(header.rpt1.into()),
            Color::White,
        ));
        detail_lines.push(kv_line(
            "RPT2",
            gateway_callsign(header.rpt2.into()),
            Color::White,
        ));
        detail_lines.push(kv_line("Status", status_str.to_string(), status_color));

        if let Some(ref text) = app.dstar_text_message {
            detail_lines.push(Line::from(""));
            detail_lines.push(kv_line(
                "Text Message",
                gateway_text_message(text),
                Color::Yellow,
            ));
        }
    } else if let Some(entry) = app.dstar_last_heard.get(app.dstar_last_heard_index) {
        // Show selected station info when no active transmission
        detail_lines.push(Line::from(Span::styled(
            format!(" {}", gateway_callsign(entry.callsign)),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        detail_lines.push(Line::from(""));

        let suffix = gateway_suffix(entry.suffix);
        if !suffix.is_empty() {
            detail_lines.push(kv_line("Suffix", suffix, Color::White));
        }
        detail_lines.push(kv_line(
            "Destination",
            gateway_callsign(entry.destination),
            Color::White,
        ));
        detail_lines.push(kv_line(
            "RPT1",
            gateway_callsign(entry.repeater1),
            Color::White,
        ));
        detail_lines.push(kv_line(
            "RPT2",
            gateway_callsign(entry.repeater2),
            Color::White,
        ));
        detail_lines.push(kv_line("Last heard", ago(entry), Color::White));
    } else {
        detail_lines.push(Line::from(Span::styled(
            " No transmission yet",
            Style::default().fg(Color::DarkGray),
        )));
    }

    frame.render_widget(
        Paragraph::new(detail_lines).block(detail_block),
        detail_area,
    );
}
