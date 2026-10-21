use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use kenwood_thd75::types::SMeterReading;

use crate::app::{App, BandState, Pane};

/// Render a band panel (A or B) into the given area.
///
/// Displays frequency, mode, power, squelch, busy/RX indicator,
/// S-meter bar, step size, and attenuator state. Returns early for non-band
/// panes.
pub(crate) fn render(app: &App, frame: &mut Frame<'_>, area: Rect, pane: Pane) {
    let (title, band) = match pane {
        Pane::BandA => (" Band A ", &app.state.band_a),
        Pane::BandB => (" Band B ", &app.state.band_b),
        _ => return,
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(super::border_style(app, pane));

    frame.render_widget(Paragraph::new(band_lines(band)).block(block), area);
}

/// Show the latest observations, marking every missing value unavailable.
fn band_lines(band: &BandState) -> Vec<Line<'static>> {
    let freq = band.frequency.map_or_else(
        || "Unavailable".to_owned(),
        |frequency| format!("{:.6} MHz", frequency.as_mhz()),
    );
    let freq_line = Line::from(Span::styled(
        format!("  {freq}"),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));

    let busy_span = match band.busy {
        Some(true) => Span::styled(" RX ", Style::default().fg(Color::Black).bg(Color::Green)),
        Some(false) => Span::raw("Idle"),
        None => Span::styled("RX: unavailable", Style::default().fg(Color::DarkGray)),
    };

    let mode_line = Line::from(vec![
        Span::styled(
            format!(
                "  {}  ",
                band.mode
                    .map_or_else(|| "Unavailable".to_owned(), |mode| mode.to_string())
            ),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled("Pwr:", Style::default().fg(Color::DarkGray)),
        Span::styled(
            band.power_level.map_or("Unavailable", power_label),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw("  "),
        Span::styled("Sq:", Style::default().fg(Color::DarkGray)),
        Span::styled(
            band.squelch
                .map_or_else(|| "Unavailable".to_owned(), |squelch| squelch.to_string()),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw("  "),
        busy_span,
    ]);

    let s_meter_line = band.s_meter.map_or_else(
        || {
            Line::from(Span::styled(
                "  S Unavailable",
                Style::default().fg(Color::DarkGray),
            ))
        },
        s_meter_line,
    );

    let step_str = band
        .step_size
        .map_or_else(|| "Unavailable".into(), |s| format!("{s}"));
    let mut extra = vec![
        Span::styled("  Step:", Style::default().fg(Color::DarkGray)),
        Span::styled(step_str, Style::default().fg(Color::Yellow)),
    ];
    extra.push(match band.attenuator {
        Some(true) => Span::styled(" ATT: On", Style::default().fg(Color::Red)),
        Some(false) => Span::raw(" ATT: Off"),
        None => Span::styled(" ATT: Unavailable", Style::default().fg(Color::DarkGray)),
    });
    let extra_line = Line::from(extra);

    vec![freq_line, mode_line, s_meter_line, extra_line]
}

/// Render the model's S-unit label and corresponding nine-segment bar.
fn s_meter_line(reading: SMeterReading) -> Line<'static> {
    let label = reading.s_unit();
    let s_unit = reading.s_unit_value();
    let filled = usize::from(s_unit);
    let empty = 9 - filled;
    let bar: String = "▓".repeat(filled) + &"░".repeat(empty);

    let color = if s_unit >= 9 {
        Color::Red
    } else if s_unit >= 5 {
        Color::Yellow
    } else {
        Color::Green
    };

    Line::from(vec![
        Span::styled("  S ", Style::default().fg(Color::DarkGray)),
        Span::styled(bar, Style::default().fg(color)),
        Span::styled(format!(" {label}"), Style::default().fg(Color::White)),
    ])
}

/// Map power level to a compact display label: H/M/L/EL.
const fn power_label(level: kenwood_thd75::types::PowerLevel) -> &'static str {
    use kenwood_thd75::types::PowerLevel;
    match level {
        PowerLevel::High => "H",
        PowerLevel::Medium => "M",
        PowerLevel::Low => "L",
        PowerLevel::ExtraLow => "EL",
    }
}
