use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, DStarMode, McpState, Pane};

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

pub(crate) fn render(app: &App, frame: &mut Frame<'_>, list_area: Rect, detail_area: Rect) {
    match app.dstar_mode {
        DStarMode::Active => render_gateway(app, frame, list_area, detail_area),
        DStarMode::Inactive => render_cat_config(app, frame, list_area, detail_area),
    }
}

// ---------------------------------------------------------------------------
// CAT config view (gateway not active)
// ---------------------------------------------------------------------------

fn render_cat_config(app: &App, frame: &mut Frame<'_>, list_area: Rect, detail_area: Rect) {
    let block = Block::default()
        .title(" D-STAR Configuration ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Main));

    let detail_block = Block::default()
        .title(" Quick Actions ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Detail));

    let mut lines: Vec<Line<'_>> = Vec::new();

    // Show MCP-based MY callsign if available
    if let McpState::Loaded { ref image, .. } = app.mcp {
        let my_cs = image.dstar().my_callsign();
        let (disp, col) = if my_cs.is_empty() {
            ("<not set>".into(), Color::DarkGray)
        } else {
            (my_cs, Color::Cyan)
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
        .map_or_else(|| "Unknown".to_string(), |s| format!("{}", s.as_u8()));
    lines.push(kv_line("D-STAR Slot", slot_str, Color::White));

    // Callsign slot
    let cs_slot_str = app
        .state
        .dstar_callsign_slot
        .map_or_else(|| "Unknown".to_string(), |s| format!("{}", s.as_u8()));
    lines.push(kv_line("Callsign Slot", cs_slot_str, Color::White));

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
            " [r] Connect Reflector   [U] Unlink Reflector",
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
        ("[r]", "Connect to reflector"),
        ("[U]", "Unlink from reflector"),
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
        let rpt_count = dstar.repeater_count();

        detail_lines.push(Line::from(""));
        detail_lines.push(Line::from(Span::styled(
            " MCP D-STAR Data",
            Style::default().fg(Color::Yellow),
        )));
        detail_lines.push(Line::from(""));
        detail_lines.push(kv_line("Repeaters", format!("{rpt_count}"), Color::White));

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
// Gateway mode view (DStarGateway active)
// ---------------------------------------------------------------------------

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

        for (i, entry) in app.dstar_last_heard[start..end].iter().enumerate() {
            let idx = start + i;
            let is_selected = idx == app.dstar_last_heard_index;

            let callsign = format!("{:<9}", entry.callsign.trim());
            let dest = format!("{:<9}", entry.destination.trim());
            let time = ago(entry.timestamp);

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

        detail_lines.push(kv_line(
            "From",
            format!("{} {}", header.my_call.as_str(), header.my_suffix.as_str()),
            Color::Cyan,
        ));
        detail_lines.push(kv_line(
            "To",
            header.ur_call.as_str().into_owned(),
            Color::White,
        ));
        detail_lines.push(kv_line(
            "RPT1",
            header.rpt1.as_str().into_owned(),
            Color::White,
        ));
        detail_lines.push(kv_line(
            "RPT2",
            header.rpt2.as_str().into_owned(),
            Color::White,
        ));
        detail_lines.push(kv_line("Status", status_str.to_string(), status_color));

        if let Some(ref text) = app.dstar_text_message {
            detail_lines.push(Line::from(""));
            detail_lines.push(kv_line("Text Message", text.clone(), Color::Yellow));
        }
    } else if let Some(entry) = app.dstar_last_heard.get(app.dstar_last_heard_index) {
        // Show selected station info when no active transmission
        detail_lines.push(Line::from(Span::styled(
            format!(" {}", entry.callsign.trim()),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        detail_lines.push(Line::from(""));

        let suffix = entry.suffix.trim();
        if !suffix.is_empty() {
            detail_lines.push(kv_line("Suffix", suffix.to_string(), Color::White));
        }
        detail_lines.push(kv_line(
            "Destination",
            entry.destination.trim().to_string(),
            Color::White,
        ));
        detail_lines.push(kv_line(
            "RPT1",
            entry.repeater1.trim().to_string(),
            Color::White,
        ));
        detail_lines.push(kv_line(
            "RPT2",
            entry.repeater2.trim().to_string(),
            Color::White,
        ));
        detail_lines.push(kv_line("Last heard", ago(entry.timestamp), Color::White));
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
