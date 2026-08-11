use std::io::stdout;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Terminal;

use crate::chat::{install_interrupt_handler, run_task, was_interrupted, clear_interrupt, Config};
use crate::llm::RealLlmClient;
use crate::state::exit_code_for_status;

#[derive(Debug, Clone)]
pub enum ChatEvent {
    Turn { number: u32, max: u32 },
    Assistant { text: String },
    ToolCall { name: String, args: String },
    ToolResult { name: String, result: String },
    Todo { summary: String },
    Status { status: String },
    Error { message: String },
}

pub fn run(config: Config) -> i32 {
    if !is_tty() {
        eprintln!("agentic chat needs a terminal. use 'agentic run' for headless mode.");
        return 1;
    }

    install_interrupt_handler();

    let (sender, receiver) = mpsc::channel::<ChatEvent>();

    let agent_config = config.clone();
    let agent_thread = thread::spawn(move || {
        let client = match RealLlmClient::resolve(
            agent_config.binary_flag.as_deref(),
            &agent_config.config_path,
            agent_config.model.as_deref(),
        ) {
            Ok(c) => c,
            Err(e) => {
                let _ = sender.send(ChatEvent::Error { message: e.human_message() });
                return;
            }
        };

        let _ = sender.send(ChatEvent::Turn { number: 0, max: agent_config.max_turns });

        match run_task_with_events(&client, &agent_config, sender.clone()) {
            Ok(outcome) => {
                let _ = sender.send(ChatEvent::Status { status: outcome.status.clone() });
            }
            Err(e) => {
                let _ = sender.send(ChatEvent::Error { message: e });
            }
        }
    });

    let exit_code = render_tui(receiver);

    let _ = agent_thread.join();

    exit_code
}

fn run_task_with_events(
    client: &RealLlmClient,
    config: &Config,
    sender: mpsc::Sender<ChatEvent>,
) -> Result<crate::chat::Outcome, String> {
    let mut callback = |event: &crate::chat::LoopEvent| {
        match event {
            crate::chat::LoopEvent::TurnStart { turn, max } => {
                let _ = sender.send(ChatEvent::Turn { number: *turn, max: *max });
            }
            crate::chat::LoopEvent::AssistantResponse { text } => {
                let _ = sender.send(ChatEvent::Assistant { text: text.clone() });
            }
            crate::chat::LoopEvent::ToolCalled { name, args } => {
                let _ = sender.send(ChatEvent::ToolCall { name: name.clone(), args: args.clone() });
            }
            crate::chat::LoopEvent::ToolFinished { name, result } => {
                let summary = if result.len() > 200 { format!("{}...", &result[..200]) } else { result.clone() };
                let _ = sender.send(ChatEvent::ToolResult { name: name.clone(), result: summary });
            }
            crate::chat::LoopEvent::TodoChanged { summary } => {
                let _ = sender.send(ChatEvent::Todo { summary: summary.clone() });
            }
        }
    };

    crate::chat::run_task_callback(client, config, None, &mut callback)
}

fn render_tui(receiver: mpsc::Receiver<ChatEvent>) -> i32 {
    let mut events: Vec<ChatEvent> = Vec::new();
    let mut list_state = ListState::default();
    list_state.select(Some(0));
    let mut final_status = "running".to_string();
    let mut error_msg: Option<String> = None;
    let mut detail_popup: Option<String> = None;

    let _ = enable_raw_mode();
    let _ = execute!(&stdout(), EnterAlternateScreen);
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = match Terminal::new(backend) {
        Ok(t) => t,
        Err(e) => {
            let _ = disable_raw_mode();
            eprintln!("could not start terminal: {e}");
            return 1;
        }
    };

    loop {
        while let Ok(ev) = receiver.try_recv() {
            match &ev {
                ChatEvent::Status { status } => final_status = status.clone(),
                ChatEvent::Error { message } => error_msg = Some(message.clone()),
                _ => {}
            }
            events.push(ev);
        }

        let _ = terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(3), Constraint::Length(1)])
                .split(f.area());

            let items: Vec<ListItem> = events
                .iter()
                .map(|ev| ListItem::new(Line::from(format_event(ev))))
                .collect();
            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(" agentic "))
                .highlight_style(Style::default().bg(Color::DarkGray));
            f.render_stateful_widget(list, chunks[0], &mut list_state);

            let status_text = if let Some(ref err) = error_msg {
                format!(" error: {} ", truncate(err, 60))
            } else {
                let turn = events.iter().rev().find_map(|e| {
                    if let ChatEvent::Turn { number, .. } = e { Some(*number) } else { None }
                }).unwrap_or(0);
                let todo = events.iter().rev().find_map(|e| {
                    if let ChatEvent::Todo { summary } = e { Some(summary.as_str()) } else { None }
                }).unwrap_or("");
                format!(" turn {} | {} | {} ", turn, final_status, truncate(todo, 40))
            };
            let status_bar = Paragraph::new(status_text)
                .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(status_bar, chunks[1]);

            let hint = if final_status == "running" {
                " q: quit | j/k: scroll | Enter: detail "
            } else {
                " q: quit | Enter: detail "
            };
            f.render_widget(Paragraph::new(hint), chunks[2]);

            if let Some(ref detail) = detail_popup {
                let area = Layout::default()
                    .constraints([Constraint::Min(0)])
                    .horizontal_margin(5)
                    .vertical_margin(3)
                    .split(f.area())[0];
                let para = Paragraph::new(detail.as_str())
                    .block(Block::default().borders(Borders::ALL).title(" detail (esc to close) "))
                    .wrap(Wrap { trim: false })
                    .scroll((0, 0));
                f.render_widget(para, area);
            }
        });

        if error_msg.is_some() || final_status == "completed" || final_status == "blocked" || final_status == "interrupted" {
            if !event::poll(Duration::from_millis(150)).unwrap_or(false) {
                continue;
            }
            if let Ok(Event::Key(key)) = event::read() {
                if key.kind == KeyEventKind::Press && key.code == KeyCode::Char('q') {
                    break;
                }
            }
            continue;
        }

        if !event::poll(Duration::from_millis(100)).unwrap_or(false) {
            continue;
        }
        if let Ok(Event::Key(key)) = event::read() {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Char('c') => {
                    crate::chat::set_interrupted();
                    break;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    if let Some(i) = list_state.selected() {
                        if i + 1 < events.len() {
                            list_state.select(Some(i + 1));
                        }
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if let Some(i) = list_state.selected() {
                        if i > 0 {
                            list_state.select(Some(i - 1));
                        }
                    }
                }
                KeyCode::Char('G') => {
                    if !events.is_empty() {
                        list_state.select(Some(events.len() - 1));
                    }
                }
                KeyCode::Char('g') => {
                    list_state.select(Some(0));
                }
                KeyCode::Enter => {
                    if let Some(i) = list_state.selected() {
                        if i < events.len() {
                            detail_popup = Some(full_text(&events[i]));
                        }
                    }
                }
                _ => {}
            }
        }

        if detail_popup.is_some() {
            if !event::poll(Duration::from_millis(200)).unwrap_or(false) {
                continue;
            }
            if let Ok(Event::Key(key)) = event::read() {
                if key.kind == KeyEventKind::Press && (key.code == KeyCode::Esc || key.code == KeyCode::Enter) {
                    detail_popup = None;
                }
            }
            continue;
        }
    }

    let _ = disable_raw_mode();
    let _ = execute!(&stdout(), LeaveAlternateScreen);

    if let Some(ref msg) = error_msg {
        eprintln!("error: {msg}");
        return 1;
    }
    exit_code_for_status(&final_status)
}

fn format_event(ev: &ChatEvent) -> Vec<Span> {
    match ev {
        ChatEvent::Turn { number, max } => vec![
            Span::styled(format!("--- turn {}/{} ---", number, max), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        ],
        ChatEvent::Assistant { text } => vec![
            Span::styled("assistant: ", Style::default().fg(Color::Green)),
            Span::raw(text.replace('\n', " ")),
        ],
        ChatEvent::ToolCall { name, args } => vec![
            Span::styled("tool: ", Style::default().fg(Color::Yellow)),
            Span::styled(name.clone(), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw(format!(" {}", truncate(args, 100))),
        ],
        ChatEvent::ToolResult { name, result } => vec![
            Span::styled("result: ", Style::default().fg(Color::Blue)),
            Span::styled(format!("{} ", name), Style::default().fg(Color::Blue)),
            Span::raw(truncate(result, 200)),
        ],
        ChatEvent::Todo { summary } => vec![
            Span::styled("todo: ", Style::default().fg(Color::Magenta)),
            Span::raw(summary),
        ],
        ChatEvent::Status { status } => vec![
            Span::styled(format!("status: {}", status), Style::default().fg(if status == "completed" { Color::Green } else { Color::Red }).add_modifier(Modifier::BOLD)),
        ],
        ChatEvent::Error { message } => vec![
            Span::styled(format!("error: {}", message), Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        ],
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

fn full_text(ev: &ChatEvent) -> String {
    match ev {
        ChatEvent::Turn { number, max } => format!("Turn {}/{}", number, max),
        ChatEvent::Assistant { text } => text.clone(),
        ChatEvent::ToolCall { name, args } => format!("tool: {}\nargs: {}", name, args),
        ChatEvent::ToolResult { name, result } => format!("result from {}:\n{}", name, result),
        ChatEvent::Todo { summary } => format!("todo: {}", summary),
        ChatEvent::Status { status } => format!("status: {}", status),
        ChatEvent::Error { message } => format!("error: {}", message),
    }
}

fn is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_returns_as_is() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_cuts_with_ellipsis() {
        assert_eq!(truncate("hello world foo", 5), "hello...");
    }

    #[test]
    fn full_text_assistant_returns_content() {
        let ev = ChatEvent::Assistant { text: "hello world".into() };
        assert_eq!(full_text(&ev), "hello world");
    }

    #[test]
    fn full_text_tool_call_includes_name_and_args() {
        let ev = ChatEvent::ToolCall { name: "read".into(), args: r#"{"path":"x"}"#.into() };
        let text = full_text(&ev);
        assert!(text.contains("read"));
        assert!(text.contains("x"));
    }

    #[test]
    fn full_text_tool_result_includes_name() {
        let ev = ChatEvent::ToolResult { name: "grep".into(), result: "3 matches".into() };
        let text = full_text(&ev);
        assert!(text.contains("grep"));
        assert!(text.contains("3 matches"));
    }
}
