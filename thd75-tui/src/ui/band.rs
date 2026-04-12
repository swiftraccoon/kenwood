use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use kenwood_thd75::types::SMeterReading;

use crate::app::{App, BandState, InputMode, Pane};

/// Render a band panel (A or B) into the given area.
///
/// Displays frequency, mode, power, squelch, busy/RX indicator,
/// S-meter bar, step size, and attenuator state. When the pane is
/// focused and in frequency input mode, an additional input prompt
/// line is shown. Returns early for non-band panes.
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

    let mut lines = band_lines(band);

    // Show frequency input prompt when active on this pane
    if app.focus == pane
        && let InputMode::FreqInput(ref buf) = app.input_mode
    {
        lines.push(Line::from(vec![
            Span::styled("  Freq: ", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("{buf}▎ MHz"),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Build the 4-line display for a band panel:
/// 1. Frequency (bold white, MHz with 6 decimal places)
/// 2. Mode + Power + Squelch + RX indicator (green "RX" badge when busy)
/// 3. S-meter bar (green S0-S3, yellow S5-S7, red S9)
/// 4. Step size + ATT indicator (red when attenuator is on)
fn band_lines(band: &BandState) -> Vec<Line<'static>> {
    let freq = format!("{:.6} MHz", band.frequency.as_mhz());
    let freq_line = Line::from(Span::styled(
        format!("  {freq}"),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));

    let busy_span = if band.busy {
        Span::styled(" RX ", Style::default().fg(Color::Black).bg(Color::Green))
    } else {
        Span::raw("    ")
    };

    let mode_line = Line::from(vec![
        Span::styled(
            format!("  {}  ", band.mode),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled("Pwr:", Style::default().fg(Color::DarkGray)),
        Span::styled(
            power_label(band.power_level).to_string(),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw("  "),
        Span::styled("Sq:", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}", band.squelch.as_u8()),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw("  "),
        busy_span,
    ]);

    let s_meter_line = s_meter_line(band.s_meter);

    let step_str = band
        .step_size
        .map_or_else(|| "N/A".into(), |s| format!("{s}"));
    let mut extra = vec![
        Span::styled("  Step:", Style::default().fg(Color::DarkGray)),
        Span::styled(step_str, Style::default().fg(Color::Yellow)),
    ];
    if band.attenuator {
        extra.push(Span::styled(" ATT", Style::default().fg(Color::Red)));
    }
    let extra_line = Line::from(extra);

    vec![freq_line, mode_line, s_meter_line, extra_line]
}

/// Render S-meter bar from an `SMeterReading`.
///
/// Uses `as_u8()` for the raw reading (0-5) to compute bar width via
/// `s_unit()` label mapping, and `s_unit()` for the display label.
fn s_meter_line(reading: SMeterReading) -> Line<'static> {
    let label = reading.s_unit(); // e.g. "S0", "S3", "S9"
    // Use the raw-to-S-unit mapping for the bar width:
    // raw 0→S0, 1→S1, 2→S3, 3→S5, 4→S7, 5→S9
    let s_unit: u8 = match reading.as_u8() {
        1 => 1,
        2 => 3,
        3 => 5,
        4 => 7,
        5 => 9,
        _ => 0,
    };

    // Bar: 9 segments for S0-S9
    let filled = s_unit.min(9) as usize;
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
