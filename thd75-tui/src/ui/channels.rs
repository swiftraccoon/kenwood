use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use kenwood_thd75::types::{ChannelTransmitValue, CrossToneType, ShiftDirection, ToneMode};

use crate::app::{App, ChannelEditField, InputMode, McpState, Pane};

pub(crate) fn render_list(app: &App, frame: &mut Frame<'_>, area: Rect) {
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

    if let McpState::Loaded { image, .. } = &app.mcp {
        let channels = image.channels();
        let used = match app.filtered_channels() {
            Ok(used) => used,
            Err(error) => {
                let message = format!(" Channel data error:\n {error}");
                frame.render_widget(Paragraph::new(message).block(block), area);
                return;
            }
        };
        let items = used
            .iter()
            .map(|&i| {
                let entry = channels.get(i)?;
                let name = entry.name().to_string();
                let freq = entry.programmed().map_or_else(
                    || "empty".to_owned(),
                    |channel| format!("{:.3}", channel.receive_frequency.as_mhz()),
                );
                Ok(ListItem::new(Line::from(vec![
                    Span::styled(format!("{i:>4}: "), Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{name:<12}"), Style::default().fg(Color::White)),
                    Span::styled(format!(" {freq}"), Style::default().fg(Color::Cyan)),
                ])))
            })
            .collect::<Result<Vec<ListItem<'_>>, kenwood_thd75::memory::MemoryError>>();
        let items = match items {
            Ok(items) => items,
            Err(error) => {
                let message = format!(" Channel data error:\n {error}");
                frame.render_widget(Paragraph::new(message).block(block), area);
                return;
            }
        };

        let mut list_state = ListState::default();
        list_state.select(Some(
            app.channel_list_index.min(items.len().saturating_sub(1)),
        ));

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
    } else {
        let msg = " No MCP data loaded.\n Press [m] then [r] to read from radio.";
        frame.render_widget(Paragraph::new(msg).block(block), area);
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Channel detail view renders every MCP channel field (frequency, offset, \
              shift, tone, CTCSS, DCS, etc.) inline as Ratatui spans. Splitting per-field \
              helpers would obscure the channel memory layout that this view visualizes."
)]
pub(crate) fn render_detail(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .title(" Detail ")
        .borders(Borders::ALL)
        .border_style(super::border_style(app, Pane::Detail));

    match &app.mcp {
        McpState::Loaded { image, .. } => {
            let channels = image.channels();
            let used = match app.filtered_channels() {
                Ok(used) => used,
                Err(error) => {
                    let message = format!(" Channel data error:\n {error}");
                    frame.render_widget(Paragraph::new(message).block(block), area);
                    return;
                }
            };
            if let Some(&ch_num) = used.get(app.channel_list_index) {
                let entry = match channels.get(ch_num) {
                    Ok(entry) => entry,
                    Err(error) => {
                        let message = format!(" Channel data error:\n {error}");
                        frame.render_widget(Paragraph::new(message).block(block), area);
                        return;
                    }
                };
                let Some(channel) = entry.programmed() else {
                    frame.render_widget(Paragraph::new(" Empty channel slot").block(block), area);
                    return;
                };

                // Tone/squelch summary string
                let tone_info = match channel.tone_mode {
                    ToneMode::Off => "None".to_string(),
                    ToneMode::Tone => format!("CTCSS TX {}", channel.tone_code.as_raw()),
                    ToneMode::Ctcss => {
                        format!(
                            "CTCSS {}/{}",
                            channel.tone_code.as_raw(),
                            channel.ctcss_code.as_raw()
                        )
                    }
                    ToneMode::Dcs => format!("DCS {:03}", channel.dcs_code.code_value()),
                    ToneMode::CrossTone => {
                        let cross = match channel.cross_tone.tone_type() {
                            CrossToneType::DcsOff => "D/O",
                            CrossToneType::ToneDcs => "T/D",
                            CrossToneType::DcsCtcss => "D/C",
                            CrossToneType::ToneCtcss => "T/C",
                        };
                        format!("Cross {cross}")
                    }
                };

                // Duplex direction string
                let duplex_info = match channel.transmit_value() {
                    ChannelTransmitValue::SplitTransmitFrequency(frequency) => {
                        format!("Split TX {:.6} MHz", frequency.as_mhz())
                    }
                    ChannelTransmitValue::RepeaterOffset(offset) => match channel.shift {
                        ShiftDirection::Simplex => "Simplex".to_string(),
                        ShiftDirection::Plus => format!("+{:.3} MHz", offset.as_mhz()),
                        ShiftDirection::Minus => format!("-{:.3} MHz", offset.as_mhz()),
                        ShiftDirection::Minus7Point6MHz => "-7.600 MHz".to_string(),
                    },
                };

                let mut lines = vec![
                    Line::from(vec![
                        Span::styled("  Channel: ", Style::default().fg(Color::DarkGray)),
                        Span::styled(format!("{ch_num}"), Style::default().fg(Color::White)),
                    ]),
                    Line::from(vec![
                        Span::styled("  Name:    ", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            entry.name().to_string(),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("  RX:      ", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            format!("{:.6} MHz", channel.receive_frequency.as_mhz()),
                            Style::default().fg(Color::Green),
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled("  Duplex:  ", Style::default().fg(Color::DarkGray)),
                        Span::styled(duplex_info, Style::default().fg(Color::Yellow)),
                    ]),
                    Line::from(vec![
                        Span::styled("  Mode:    ", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            format!("{}", channel.mode),
                            Style::default().fg(Color::White),
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled("  Tone:    ", Style::default().fg(Color::DarkGray)),
                        Span::styled(tone_info, Style::default().fg(Color::White)),
                    ]),
                ];

                lines.push(Line::from(""));
                if app.channel_edit_mode {
                    lines.push(Line::from(Span::styled(
                        "  ── Edit Mode ──",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )));

                    let fields = [
                        ChannelEditField::Name,
                        ChannelEditField::OperatingMode,
                        ChannelEditField::ToneMode,
                        ChannelEditField::ToneFreq,
                        ChannelEditField::Duplex,
                        ChannelEditField::Offset,
                    ];
                    for field in fields {
                        let marker = if field == app.channel_edit_field {
                            "\u{25b8} "
                        } else {
                            "  "
                        };
                        let color = if field == app.channel_edit_field {
                            Color::Cyan
                        } else {
                            Color::DarkGray
                        };
                        lines.push(Line::from(Span::styled(
                            format!("  {marker}{:<12}", field.label()),
                            Style::default().fg(color),
                        )));
                    }

                    if !app.channel_edit_buffer.is_empty() {
                        lines.push(Line::from(""));
                        lines.push(Line::from(vec![
                            Span::styled("  Input: ", Style::default().fg(Color::DarkGray)),
                            Span::styled(
                                format!("{}\u{258e}", app.channel_edit_buffer),
                                Style::default().fg(Color::White),
                            ),
                        ]));
                    }

                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        "  Tab=next  Enter=apply  Esc=cancel",
                        Style::default().fg(Color::DarkGray),
                    )));
                } else {
                    lines.push(Line::from(vec![Span::styled(
                        format!(
                            "  [Enter] Tune Band {}  [e] Edit",
                            if app.target_band == kenwood_thd75::types::Band::B {
                                "B"
                            } else {
                                "A"
                            }
                        ),
                        Style::default().fg(Color::DarkGray),
                    )]));
                }
                frame.render_widget(Paragraph::new(lines).block(block), area);
                return;
            }
            frame.render_widget(Paragraph::new("  No channel selected").block(block), area);
        }
        _ => {
            frame.render_widget(Paragraph::new("").block(block), area);
        }
    }
}
