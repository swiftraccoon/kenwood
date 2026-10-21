use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};

use crate::app::{App, MainView, Pane};

mod aprs;
mod band;
mod channels;
mod dstar;
mod fm_radio;
mod gps;
mod help;
mod mcp;
mod settings;
mod status_bar;

/// Render the full TUI frame.
pub(crate) fn render(app: &App, frame: &mut Frame<'_>) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6), // Band row (A + B side by side)
            Constraint::Min(8),    // Main content row
            Constraint::Length(1), // Status bar
        ])
        .split(frame.area());
    // `Layout::split` returns exactly one `Rect` per constraint, so
    // these destructures always match; the `else` arms only guard the
    // impossible shape without panicking mid-render.
    let [band_area, main_area, status_area] = *chunks else {
        return;
    };

    // Band row: split horizontally for Band A and Band B
    let band_row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(band_area);
    let [left_band, right_band] = *band_row else {
        return;
    };

    band::render(app, frame, left_band, Pane::BandA);
    band::render(app, frame, right_band, Pane::BandB);

    // Main content row: split for list + detail
    let main_row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(main_area);
    let [list_area, detail_area] = *main_row else {
        return;
    };

    // Main content + Detail
    match app.main_view {
        MainView::Channels => {
            channels::render_list(app, frame, list_area);
            channels::render_detail(app, frame, detail_area);
        }
        MainView::SettingsCat => {
            settings::render_cat(app, frame, list_area, detail_area);
        }
        MainView::SettingsMcp => {
            settings::render_mcp(app, frame, list_area, detail_area);
        }
        MainView::Aprs => {
            aprs::render(app, frame, list_area, detail_area);
        }
        MainView::Dstar => {
            dstar::render(app, frame, list_area, detail_area);
        }
        MainView::Gps => {
            gps::render(app, frame, list_area, detail_area);
        }
        MainView::Mcp => {
            mcp::render(app, frame, list_area, detail_area);
        }
        MainView::FmRadio => {
            fm_radio::render(app, frame, list_area, detail_area);
        }
    }

    // Status bar
    status_bar::render(app, frame, status_area);

    // Help overlay (on top of everything)
    if app.show_help {
        help::render(frame);
    }
}

fn border_style(app: &App, pane: Pane) -> ratatui::style::Style {
    use ratatui::style::{Color, Style};
    if app.focus == pane {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Cell;

    use kenwood_thd75::types::{AfGainLevel, Frequency, SMeterReading, SquelchLevel};

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn screen(app: &App) -> Result<String, Box<dyn std::error::Error>> {
        let mut terminal = Terminal::new(TestBackend::new(240, 54))?;
        let _frame = terminal.draw(|frame| render(app, frame))?;
        Ok(terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(Cell::symbol)
            .collect())
    }

    #[test]
    fn unavailable_observations_remain_distinct_from_observed_off_and_zero() -> TestResult {
        let mut app = App::with_cache_path("fixture".to_owned(), None);
        app.main_view = MainView::SettingsCat;
        let unknown = screen(&app)?;
        for missing in [
            "BAT:Unavailable",
            "Vol:Unavailable",
            "BT:Unavailable",
            "VOX:Unavailable",
            "ATT: Unavailable",
        ] {
            assert!(unknown.contains(missing), "missing {missing:?}: {unknown}");
        }
        assert!(
            !unknown.contains("0.000000 MHz"),
            "missing frequency was fabricated"
        );
        assert!(
            !unknown.contains("ATT: Off"),
            "missing attenuator state became off"
        );

        app.state.band_a.frequency = Some(Frequency::new(146_520_000));
        app.state.band_a.busy = Some(false);
        app.state.band_a.attenuator = Some(false);
        app.state.band_a.squelch = Some(SquelchLevel::OPEN);
        app.state.band_a.s_meter = Some(SMeterReading::ZERO);
        app.state.af_gain = Some(AfGainLevel::ZERO);
        app.state.bluetooth = Some(false);
        app.state.vox = Some(false);
        let observed = screen(&app)?;
        for value in [
            "146.520000 MHz",
            "Idle",
            "ATT: Off",
            " S0",
            "SQ0",
            "BT:Off",
            "VOX:Off",
        ] {
            assert!(observed.contains(value), "missing {value:?}: {observed}");
        }
        assert!(
            !observed.contains("Vol:Unavailable"),
            "observed zero gain disappeared"
        );

        app.main_view = MainView::FmRadio;
        let fm = screen(&app)?;
        assert!(
            fm.contains("Status:     Unavailable"),
            "unknown FM state became off: {fm}"
        );
        assert!(
            !fm.contains("Toggle FM"),
            "unavailable control was advertised"
        );
        Ok(())
    }
}
