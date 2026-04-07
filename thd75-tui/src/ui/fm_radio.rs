use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, Pane};

pub(crate) fn render(app: &App, frame: &mut Frame<'_>, list_area: Rect, detail_area: Rect) {
    let block = Block::default()
        .title(" FM Radio ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Main));

    let (status_text, status_color) = if app.fm_radio_on {
        ("On", Color::Green)
    } else {
        ("Off", Color::DarkGray)
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
            "  [f] Toggle FM Radio On/Off",
            Style::default().fg(Color::Yellow),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  FM Radio mode uses Band B (76-108 MHz WFM).",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  When active, APRS and D-STAR continue on Band A.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  The radio auto-mutes FM on incoming signals.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  Note: FR is write-only on D75 — status is tracked",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  locally and may not reflect radio state at startup.",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    frame.render_widget(Paragraph::new(lines).block(block), list_area);

    // Detail pane: show Band B info when FM is on
    let detail_block = Block::default()
        .title(" Band B (FM) ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Detail));

    let detail_lines = if app.fm_radio_on {
        let freq = app.state.band_b.frequency.as_mhz();
        let mode = &app.state.band_b.mode;
        vec![
            Line::from(""),
            Line::from(vec![
                Span::styled("  Frequency:  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{freq:.3} MHz"),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled("  Mode:       ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{mode}"), Style::default().fg(Color::White)),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "  Use Band B pane (2) to tune FM stations.",
                Style::default().fg(Color::DarkGray),
            )),
        ]
    } else {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                "  FM Radio is off.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "  Press [f] to enable.",
                Style::default().fg(Color::DarkGray),
            )),
        ]
    };

    frame.render_widget(
        Paragraph::new(detail_lines).block(detail_block),
        detail_area,
    );
}
