mod aprs;
mod band;
mod channels;
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

    // Band row: split horizontally for Band A and Band B
    let band_row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[0]);

    band::render(app, frame, band_row[0], Pane::BandA);
    band::render(app, frame, band_row[1], Pane::BandB);

    // Main content row: split for list + detail
    let main_row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[1]);

    // Main content + Detail
    match app.main_view {
        MainView::Channels => {
            channels::render_list(app, frame, main_row[0]);
            channels::render_detail(app, frame, main_row[1]);
        }
        MainView::SettingsCat => {
            settings::render_cat(app, frame, main_row[0], main_row[1]);
        }
        MainView::SettingsMcp => {
            settings::render_mcp(app, frame, main_row[0], main_row[1]);
        }
        MainView::Aprs => {
            aprs::render(app, frame, main_row[0], main_row[1]);
        }
        MainView::Mcp => {
            mcp::render(app, frame, main_row[0], main_row[1]);
        }
    }

    // Status bar
    status_bar::render(app, frame, chunks[2]);

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
