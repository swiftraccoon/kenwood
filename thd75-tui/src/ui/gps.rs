use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, Pane};

fn kv_line(label: &str, value: String, value_color: Color) -> Line<'_> {
    Line::from(vec![
        Span::styled(
            format!("  {label:<18}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(value, Style::default().fg(value_color)),
    ])
}

const fn on_off(b: bool) -> &'static str {
    if b { "On" } else { "Off" }
}

pub(crate) fn render(app: &App, frame: &mut Frame<'_>, left_area: Rect, right_area: Rect) {
    render_status(app, frame, left_area);
    render_config(app, frame, right_area);
}

// ---------------------------------------------------------------------------
// Left pane: GPS Status
// ---------------------------------------------------------------------------

fn render_status(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .title(" GPS Status ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Main));

    let s = &app.state;

    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::from(""));
    lines.push(kv_line(
        "GPS",
        on_off(s.gps_enabled).into(),
        if s.gps_enabled {
            Color::Green
        } else {
            Color::DarkGray
        },
    ));
    lines.push(kv_line(
        "PC Output",
        on_off(s.gps_pc_output).into(),
        if s.gps_pc_output {
            Color::Green
        } else {
            Color::DarkGray
        },
    ));

    lines.push(Line::from(""));
    if s.gps_enabled {
        lines.push(Line::from(Span::styled(
            "  Position data requires NMEA PC output.",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            "  GPS fix info is not available via CAT.",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "  GPS is disabled.",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            "  Press [g] to enable.",
            Style::default().fg(Color::DarkGray),
        )));
    }

    // Key hints
    lines.push(Line::from(""));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" [g]", Style::default().fg(Color::Yellow)),
        Span::styled(" Toggle GPS  ", Style::default().fg(Color::White)),
        Span::styled("[p]", Style::default().fg(Color::Yellow)),
        Span::styled(" Toggle PC Output", Style::default().fg(Color::White)),
    ]));

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

// ---------------------------------------------------------------------------
// Right pane: GPS Configuration (NMEA sentences)
// ---------------------------------------------------------------------------

fn render_config(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .title(" GPS Configuration ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Detail));

    let s = &app.state;

    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  NMEA Sentences",
        Style::default().fg(Color::Cyan),
    )));
    lines.push(Line::from(""));

    if let Some((gga, gll, gsa, gsv, rmc, vtg)) = s.gps_sentences {
        // Two-column layout for sentence flags
        lines.push(sentence_row("GGA", gga, "GLL", gll));
        lines.push(sentence_row("GSA", gsa, "GSV", gsv));
        lines.push(sentence_row("RMC", rmc, "VTG", vtg));
    } else {
        lines.push(Line::from(Span::styled(
            "  (not available)",
            Style::default().fg(Color::DarkGray),
        )));
    }

    lines.push(Line::from(""));

    // GPS mode info
    if let Some(mode) = s.gps_mode {
        lines.push(kv_line("Mode", format!("{mode}"), Color::Yellow));
    }

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Render a two-column row of sentence enable flags.
fn sentence_row<'a>(label_a: &'a str, val_a: bool, label_b: &'a str, val_b: bool) -> Line<'a> {
    let color = |v: bool| {
        if v { Color::Green } else { Color::DarkGray }
    };
    Line::from(vec![
        Span::styled(
            format!("  {label_a}:  "),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("{:<6}", on_off(val_a)),
            Style::default().fg(color(val_a)),
        ),
        Span::styled(
            format!("{label_b}:  "),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(on_off(val_b).to_string(), Style::default().fg(color(val_b))),
    ])
}
