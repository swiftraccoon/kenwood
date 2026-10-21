//! D-STAR CAT values, cached configuration and URCALL preparation controls.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, Pane};

fn value_line(label: &str, value: String) -> Line<'_> {
    Line::from(vec![
        Span::styled(
            format!("  {label:<18}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(value, Style::default().fg(Color::White)),
    ])
}

fn callsign(value: Option<&kenwood_thd75::types::DstarCallsignEntry>) -> String {
    value.map_or_else(
        || "Unknown".to_owned(),
        |entry| format!("{} / {}", entry.callsign.as_str(), entry.suffix.as_str()),
    )
}

pub(crate) fn render(app: &App, frame: &mut Frame<'_>, list_area: Rect, detail_area: Rect) {
    let block = Block::default()
        .title(" D-STAR CAT ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Main));
    let mut lines = Vec::new();
    if let Some(image) = app.confirmed_image() {
        let my_call = match image.dstar().my_callsign() {
            Ok(Some(value)) => value.as_str().to_owned(),
            Ok(None) => "Not configured (Menu 610)".to_owned(),
            Err(error) => format!("Unavailable: {error}"),
        };
        lines.push(value_line("Stored MY callsign", my_call));
    }
    lines.push(value_line(
        "URCALL / suffix",
        callsign(app.state.dstar_urcall.as_ref()),
    ));
    lines.push(value_line(
        "RPT1 / suffix",
        callsign(app.state.dstar_rpt1.as_ref()),
    ));
    lines.push(value_line(
        "RPT2 / suffix",
        callsign(app.state.dstar_rpt2.as_ref()),
    ));
    lines.push(value_line(
        "Gateway mode",
        app.state
            .dstar_gateway_mode
            .map_or_else(|| "Unknown".to_owned(), |value| format!("{value:?}")),
    ));
    lines.push(value_line(
        "D-STAR slot",
        app.state
            .dstar_slot
            .map_or_else(|| "Unknown".to_owned(), |value| value.as_raw().to_string()),
    ));
    if let Some(value) = &app.dstar_urcall_input {
        lines.push(value_line("URCALL input", format!("{value}_")));
    }
    if let Some(value) = &app.dstar_reflector_input {
        lines.push(value_line("Reflector input", format!("{value}_")));
    }
    frame.render_widget(Paragraph::new(lines).block(block), list_area);
    render_actions(app, frame, detail_area);
}

fn render_actions(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .title(" D-STAR Actions ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Detail));
    let mut lines = vec![
        Line::from(" [C] Set CQ URCALL"),
        Line::from(" [r] Prepare reflector link URCALL"),
        Line::from(" [U] Prepare reflector unlink URCALL"),
        Line::from(" [u] Enter URCALL"),
        Line::from(" [d] Show modem availability"),
        Line::from(""),
        Line::from(" Modem monitoring requires a supported"),
        Line::from(" Kenwood wire-configuration profile."),
        Line::from(" No reflector connection is opened."),
    ];
    if let Some(image) = app.confirmed_image() {
        let access = image.dstar();
        lines.push(value_line(
            "Stored repeaters",
            access.repeater_count().map_or_else(
                |error| format!("Unavailable: {error}"),
                |count| count.to_string(),
            ),
        ));
        lines.push(value_line(
            "Region size",
            format!("{} bytes", access.region_size()),
        ));
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}
