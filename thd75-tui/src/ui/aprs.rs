use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, McpState, Pane};

fn kv_line<'a>(label: &'a str, value: String, value_color: Color) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("  {label:<22}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(value, Style::default().fg(value_color)),
    ])
}

pub fn render(app: &App, frame: &mut Frame, list_area: Rect, detail_area: Rect) {
    let block = Block::default()
        .title(" APRS ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Main));

    let detail_block = Block::default()
        .title(" APRS Region Info ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Detail));

    let McpState::Loaded { ref image, .. } = app.mcp else {
        frame.render_widget(
            Paragraph::new(" No MCP data loaded.\n Press [m] then [r] to read from radio.")
                .block(block),
            list_area,
        );
        frame.render_widget(Paragraph::new("").block(detail_block), detail_area);
        return;
    };

    let aprs = image.aprs();

    // -------------------------------------------------------------------------
    // Left pane: APRS config fields
    // -------------------------------------------------------------------------
    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::from(Span::styled(
        " APRS Configuration",
        Style::default().fg(Color::Yellow),
    )));
    lines.push(Line::from(""));

    {
        let cs = aprs.my_callsign();
        let (disp, col) = if cs.is_empty() {
            ("<not set>".into(), Color::DarkGray)
        } else {
            (cs, Color::Cyan)
        };
        lines.push(kv_line("My callsign", disp, col));
    }

    {
        let interval = aprs.beacon_interval();
        let (disp, col) = if interval == 0 {
            ("Off".into(), Color::DarkGray)
        } else {
            (format!("{interval} s"), Color::White)
        };
        lines.push(kv_line("Beacon interval", disp, col));
    }

    lines.push(kv_line("Packet path", aprs.packet_path(), Color::White));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " Note: Field offsets are estimated from D74",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(Span::styled(
        " layout — unconfirmed on D75.",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(Paragraph::new(lines).block(block), list_area);

    // -------------------------------------------------------------------------
    // Right pane: APRS region info
    // -------------------------------------------------------------------------
    let mut detail_lines: Vec<Line<'_>> = Vec::new();

    detail_lines.push(Line::from(Span::styled(
        " APRS Memory Regions",
        Style::default().fg(Color::Yellow),
    )));
    detail_lines.push(Line::from(""));

    let region_info = [
        ("Status header", "0x15100", "256 bytes"),
        ("Data / settings", "0x15200", "~16 KB"),
        ("Position data", "0x25100", "19,200 bytes (confirmed)"),
    ];

    for (name, offset, size) in region_info {
        detail_lines.push(Line::from(vec![
            Span::styled(
                format!("  {name:<18}"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("{offset}  {size}"),
                Style::default().fg(Color::White),
            ),
        ]));
    }

    detail_lines.push(Line::from(""));

    let has_pos = aprs.has_position_data();
    let (pos_str, pos_col) = if has_pos {
        ("Yes", Color::Cyan)
    } else {
        ("No", Color::DarkGray)
    };
    detail_lines.push(Line::from(vec![
        Span::styled(
            "  Position data present  ",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(pos_str, Style::default().fg(pos_col)),
    ]));

    let region_sz = aprs.region_size();
    detail_lines.push(Line::from(vec![
        Span::styled(
            "  Total region size      ",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("{region_sz} bytes"),
            Style::default().fg(Color::White),
        ),
    ]));

    frame.render_widget(
        Paragraph::new(detail_lines).block(detail_block),
        detail_area,
    );
}
