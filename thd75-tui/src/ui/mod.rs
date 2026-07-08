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

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};

use crate::app::{App, MainView, Pane};

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
        MainView::DStar => {
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
