use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

pub fn render(frame: &mut Frame) {
    let area = centered_rect(60, 70, frame.area());

    // Clear the area behind the popup
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Keybindings ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let lines = vec![
        header("Navigation"),
        binding("Tab / Shift-Tab", "Cycle pane focus"),
        binding("1-4", "Jump to pane (BandA/B/Main/Detail)"),
        binding(
            "c / s / S / a / m",
            "Switch view (s=CAT settings, S=MCP settings)",
        ),
        Line::from(""),
        header("Channel List"),
        binding("j/k", "Navigate"),
        binding("g / G", "First / last"),
        binding("Enter", "Tune target band"),
        binding("/", "Search by name"),
        binding("Esc", "Clear filter"),
        Line::from(""),
        header("Band Pane"),
        binding("k/j", "Step freq up/down"),
        binding("f", "Enter frequency (MHz)"),
        binding("p", "Cycle power (H/M/L/EL)"),
        binding("t", "Toggle attenuator"),
        binding("[ / ]", "Squelch down/up"),
        Line::from(""),
        header("Settings"),
        binding("Enter", "Toggle setting"),
        binding("+ / -", "Adjust value"),
        Line::from(""),
        header("MCP"),
        binding("r", "Read memory from radio"),
        binding("w", "Write memory to radio"),
        Line::from(""),
        header("Global"),
        binding("?", "This help"),
        binding("q / Ctrl-C", "Quit"),
    ];

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn header(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {text}"),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))
}

fn binding(key: &str, desc: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("    {key:<20}"), Style::default().fg(Color::Yellow)),
        Span::styled(desc.to_string(), Style::default().fg(Color::White)),
    ])
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}
