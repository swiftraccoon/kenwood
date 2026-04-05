use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};

use crate::app::{App, McpState, Pane};

pub(crate) fn render(app: &App, frame: &mut Frame<'_>, list_area: Rect, detail_area: Rect) {
    let block = Block::default()
        .title(" MCP Programming ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Main));

    match &app.mcp {
        McpState::Idle => {
            let text = " Press [r] to read memory from radio\n Press [w] to write memory to radio";
            frame.render_widget(Paragraph::new(text).block(block), list_area);
        }
        McpState::Reading { page, total } | McpState::Writing { page, total } => {
            let label = if matches!(app.mcp, McpState::Reading { .. }) {
                "Reading"
            } else {
                "Writing"
            };
            let pct = if *total > 0 {
                (f64::from(*page) / f64::from(*total)).min(1.0)
            } else {
                0.0
            };

            let inner = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(1)])
                .split(block.inner(list_area));

            frame.render_widget(block, list_area);

            let gauge = Gauge::default()
                .block(Block::default().title(format!(" {label} ")))
                .gauge_style(Style::default().fg(Color::Cyan))
                .ratio(pct)
                .label(format!("{}/{} ({:.0}%)", page, total, pct * 100.0));
            frame.render_widget(gauge, inner[0]);
        }
        McpState::Loaded { modified, .. } => {
            let status = if *modified { " (modified)" } else { "" };
            let text = format!(
                " Memory loaded{status}\n\n Press [r] to re-read\n Press [w] to write to radio\n\n Switch to [c]hannels to browse"
            );
            frame.render_widget(Paragraph::new(text).block(block), list_area);
        }
        McpState::Reconnecting => {
            let text = " Reconnecting to radio after MCP write...\n Waiting for USB device...";
            frame.render_widget(Paragraph::new(text).block(block), list_area);
        }
    }

    // Detail pane: empty for MCP view
    let detail_block = Block::default()
        .title(" MCP Info ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Detail));
    frame.render_widget(Paragraph::new("").block(detail_block), detail_area);
}
