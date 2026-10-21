use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, Pane};

/// Show FM availability and independently observed Band B values.
pub(crate) fn render(app: &App, frame: &mut Frame<'_>, list_area: Rect, detail_area: Rect) {
    let block = Block::default()
        .title(" FM Radio ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Main));

    let (status_text, status_color) = match app.fm_radio_on {
        Some(true) => ("On", Color::Green),
        Some(false) => ("Off", Color::DarkGray),
        None => ("Unavailable", Color::DarkGray),
    };
    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  Status:     ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                status_text,
                Style::default()
                    .fg(status_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Change FM radio mode on the radio.",
            Style::default().fg(Color::Yellow),
        )),
        Line::from(Span::styled(
            "  FM controls are unavailable in this panel.",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    frame.render_widget(Paragraph::new(lines).block(block), list_area);

    let detail_block = Block::default()
        .title(" Band B Observations ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Detail));
    let frequency = app.state.band_b.frequency.map_or_else(
        || "Unavailable".to_owned(),
        |frequency| format!("{:.6} MHz", frequency.as_mhz()),
    );
    let mode = app
        .state
        .band_b
        .mode
        .map_or_else(|| "Unavailable".to_owned(), |mode| mode.to_string());
    let detail_lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  Frequency:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(frequency, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  Mode:       ", Style::default().fg(Color::DarkGray)),
            Span::styled(mode, Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Select Band B (2) to view its controls.",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    frame.render_widget(
        Paragraph::new(detail_lines).block(detail_block),
        detail_area,
    );
}
