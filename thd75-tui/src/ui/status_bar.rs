use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;

pub fn render(app: &App, frame: &mut Frame, area: Rect) {
    let mut spans: Vec<Span<'_>> = Vec::new();

    // Connection
    if app.connected {
        spans.push(Span::styled(
            format!(" {} ", app.port_path),
            Style::default().fg(Color::Green),
        ));
    } else {
        spans.push(Span::styled(
            " Disconnected ",
            Style::default().fg(Color::Red),
        ));
    }

    // Firmware
    if !app.state.firmware_version.is_empty() {
        spans.push(Span::styled(
            format!("FW:{} ", app.state.firmware_version),
            Style::default().fg(Color::DarkGray),
        ));
    }

    // Live toggles as compact indicators
    let s = &app.state;
    let mut flags = Vec::new();
    flags.push(format!("BL:{}", s.backlight));
    flags.push(format!("Vol:{}", s.af_gain));
    if !s.lock { flags.push("LCK".into()); }       // CAT inverted on D75
    if s.bluetooth { flags.push("BT".into()); }
    if s.vox { flags.push("VOX".into()); }
    if s.gps_enabled { flags.push("GPS".into()); }
    if s.dual_band { flags.push("1BD".into()); }   // CAT inverted on D75

    spans.push(Span::styled(
        format!(" {} ", flags.join(" ")),
        Style::default().fg(Color::DarkGray),
    ));

    // Target band indicator
    let band_label = if app.target_band == kenwood_thd75::types::Band::B { "B" } else { "A" };
    spans.push(Span::styled(
        format!("→{band_label} "),
        Style::default().fg(Color::Cyan),
    ));

    // Status message
    if let Some(ref msg) = app.status_message {
        spans.push(Span::styled(format!("{msg} "), Style::default().fg(Color::Yellow)));
    }

    // Help hint
    spans.push(Span::styled(
        "[?]Help ",
        Style::default().fg(Color::DarkGray),
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
