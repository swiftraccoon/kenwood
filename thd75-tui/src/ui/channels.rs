use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::app::{App, InputMode, McpState, Pane};

pub fn render_list(app: &App, frame: &mut Frame, area: Rect) {
    let title = if let InputMode::Search(ref buf) = app.input_mode {
        format!(" Search: {buf}▎ ")
    } else if !app.search_filter.is_empty() {
        format!(" Channels [filter: {}] ", app.search_filter)
    } else {
        " Channels ".to_string()
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Main));

    match &app.mcp {
        McpState::Loaded { image, .. } => {
            let channels = image.channels();
            let used = app.filtered_channels();
            let items: Vec<ListItem> = used
                .iter()
                .map(|&i| {
                    let entry = channels.get(i);
                    let name = entry
                        .as_ref()
                        .map(|e| e.name.clone())
                        .unwrap_or_default();
                    let freq = entry
                        .as_ref()
                        .map(|e| format!("{:.3}", e.flash.rx_frequency.as_mhz()))
                        .unwrap_or_default();
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            format!("{i:>4}: "),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(
                            format!("{name:<12}"),
                            Style::default().fg(Color::White),
                        ),
                        Span::styled(
                            format!(" {freq}"),
                            Style::default().fg(Color::Cyan),
                        ),
                    ]))
                })
                .collect();

            let mut list_state = ListState::default();
            list_state.select(Some(app.channel_list_index.min(items.len().saturating_sub(1))));

            let list = List::new(items)
                .block(block)
                .highlight_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("▸ ");

            frame.render_stateful_widget(list, area, &mut list_state);
        }
        _ => {
            let msg = " No MCP data loaded.\n Press [m] then [r] to read from radio.";
            frame.render_widget(Paragraph::new(msg).block(block), area);
        }
    }
}

pub fn render_detail(app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" Detail ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Detail));

    match &app.mcp {
        McpState::Loaded { image, .. } => {
            let channels = image.channels();
            let used = app.filtered_channels();
            if let Some(&ch_num) = used.get(app.channel_list_index) {
                if let Some(entry) = channels.get(ch_num) {
                    let fc = &entry.flash;

                    // Tone/squelch summary string
                    let tone_info = if fc.tone_enabled {
                        format!("CTCSS TX {}", fc.tone_code.index())
                    } else if fc.ctcss_enabled {
                        format!("CTCSS {}/{}", fc.tone_code.index(), fc.ctcss_code.index())
                    } else if fc.dtcs_enabled {
                        format!("DCS {:03}", u16::from(fc.dcs_code.index()))
                    } else {
                        "None".to_string()
                    };

                    // Duplex direction string
                    let duplex_info = match fc.duplex {
                        kenwood_thd75::types::FlashDuplex::Simplex => "Simplex".to_string(),
                        kenwood_thd75::types::FlashDuplex::Plus => {
                            format!("+{:.3} MHz", fc.tx_offset.as_mhz())
                        }
                        kenwood_thd75::types::FlashDuplex::Minus => {
                            format!("-{:.3} MHz", fc.tx_offset.as_mhz())
                        }
                    };

                    let lines = vec![
                        Line::from(vec![
                            Span::styled("  Channel: ", Style::default().fg(Color::DarkGray)),
                            Span::styled(format!("{ch_num}"), Style::default().fg(Color::White)),
                        ]),
                        Line::from(vec![
                            Span::styled("  Name:    ", Style::default().fg(Color::DarkGray)),
                            Span::styled(entry.name.clone(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                        ]),
                        Line::from(""),
                        Line::from(vec![
                            Span::styled("  RX:      ", Style::default().fg(Color::DarkGray)),
                            Span::styled(
                                format!("{:.6} MHz", fc.rx_frequency.as_mhz()),
                                Style::default().fg(Color::Green),
                            ),
                        ]),
                        Line::from(vec![
                            Span::styled("  Duplex:  ", Style::default().fg(Color::DarkGray)),
                            Span::styled(duplex_info, Style::default().fg(Color::Yellow)),
                        ]),
                        Line::from(vec![
                            Span::styled("  Mode:    ", Style::default().fg(Color::DarkGray)),
                            Span::styled(format!("{}", fc.mode), Style::default().fg(Color::White)),
                        ]),
                        Line::from(vec![
                            Span::styled("  Tone:    ", Style::default().fg(Color::DarkGray)),
                            Span::styled(tone_info, Style::default().fg(Color::White)),
                        ]),
                        Line::from(""),
                        Line::from(vec![
                            Span::styled(
                                format!("  [Enter] Tune Band {}", if app.target_band == kenwood_thd75::types::Band::B { "B" } else { "A" }),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]),
                    ];
                    frame.render_widget(Paragraph::new(lines).block(block), area);
                    return;
                }
            }
            frame.render_widget(Paragraph::new("  No channel selected").block(block), area);
        }
        _ => {
            frame.render_widget(Paragraph::new("").block(block), area);
        }
    }
}
