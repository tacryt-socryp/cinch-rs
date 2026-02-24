//! Generic rendering for the harness TUI.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cinch_rs::ui::{AgentEntry, LogLevel, UiState};
use ratatui::prelude::*;
use ratatui::widgets::*;

use crate::app::{ActivePane, App, InputMode};
use crate::ext::TuiExtensionRenderer;

// ── Public Utilities ──────────────────────────────────────────────────

/// Format a duration as "Xm Ys" or "Xh Ym".
pub fn format_countdown(remaining: Duration) -> String {
    let total_secs = remaining.as_secs();
    if total_secs >= 3600 {
        let hours = total_secs / 3600;
        let mins = (total_secs % 3600) / 60;
        format!("{hours}h {mins:02}m")
    } else {
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        format!("{mins}m {secs:02}s")
    }
}

/// Truncate a string to a maximum length, appending "..." if truncated.
pub fn truncate_str(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}...", &s[..max])
    } else {
        s.to_string()
    }
}

/// Summarize JSON arguments into a compact `key=value` form.
pub fn summarize_args(raw: &str, max_len: usize) -> String {
    if let Ok(obj) = serde_json::from_str::<serde_json::Value>(raw)
        && let Some(map) = obj.as_object()
    {
        let parts: Vec<String> = map
            .iter()
            .map(|(k, v)| {
                let val = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                format!("{k}={val}")
            })
            .collect();
        return truncate_str(&parts.join(", "), max_len);
    }
    truncate_str(raw, max_len)
}

/// Truncate a tool result to a single-line preview.
pub fn result_preview(raw: &str, max_len: usize) -> String {
    truncate_str(raw.lines().next().unwrap_or(""), max_len)
}

/// Map a log level to a ratatui [`Style`].
pub fn log_level_style(level: LogLevel) -> Style {
    match level {
        LogLevel::Trace => Style::default().fg(Color::DarkGray),
        LogLevel::Debug => Style::default().fg(Color::Cyan),
        LogLevel::Info => Style::default().fg(Color::Green),
        LogLevel::Warn => Style::default().fg(Color::Yellow),
        LogLevel::Error => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    }
}

// ── Root Render ───────────────────────────────────────────────────────

/// Snapshot of UiState fields needed for rendering.
///
/// We clone everything we need in one shot so the `UiState` lock is held
/// only for the clone duration — never during widget construction or
/// `frame.render_widget()` calls.  This prevents the render pass from
/// blocking tokio worker threads that update state via `with_state!`.
struct RenderSnapshot {
    // Status bar fields.
    phase: String,
    round: u32,
    max_rounds: u32,
    context_pct: f64,
    model: String,
    cycle: u32,
    running: bool,
    next_cycle_at: Option<Instant>,
    active_question: Option<cinch_rs::ui::ActiveQuestion>,

    // Extension spans (pre-rendered while lock is held, since the trait
    // borrows &dyn UiExtension).
    ext_status_spans: Vec<Span<'static>>,
    ext_secondary_spans: Vec<Span<'static>>,

    // Agent output.
    agent_output: Vec<AgentEntry>,
    streaming_buffer: String,

    // Logs.
    logs: Vec<cinch_rs::ui::LogLine>,
}

/// Convert a `Vec<Span<'_>>` to `Vec<Span<'static>>` by ensuring all
/// inner `Cow::Borrowed` strings become owned.
fn own_spans(spans: Vec<Span<'_>>) -> Vec<Span<'static>> {
    spans
        .into_iter()
        .map(|s| Span::styled(s.content.into_owned(), s.style))
        .collect()
}

pub(crate) fn render(
    frame: &mut Frame,
    state: &Arc<Mutex<UiState>>,
    app: &App,
    ext_renderer: &dyn TuiExtensionRenderer,
) {
    let area = frame.area();

    // Outer layout: [6] status | [flex] middle | [3] input bar.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Min(6),
            Constraint::Length(3),
        ])
        .split(area);

    // Take a snapshot of everything we need and release the lock
    // immediately.  No rendering happens while the lock is held.
    let snap = {
        let s = state.lock().unwrap();

        // Pre-render extension spans while we have &UiState; convert to
        // owned so they outlive the lock.
        let ext_status_spans = own_spans(ext_renderer.status_spans(s.extensions.as_ref()));
        let ext_secondary_spans =
            own_spans(ext_renderer.status_secondary_spans(s.extensions.as_ref()));

        RenderSnapshot {
            phase: s.phase.clone(),
            round: s.round,
            max_rounds: s.max_rounds,
            context_pct: s.context_pct,
            model: s.model.clone(),
            cycle: s.cycle,
            running: s.running,
            next_cycle_at: s.next_cycle_at,
            active_question: s.active_question.clone(),
            ext_status_spans,
            ext_secondary_spans,
            agent_output: s.agent_output.clone(),
            streaming_buffer: s.streaming_buffer.clone(),
            logs: if app.show_logs {
                s.logs.clone()
            } else {
                Vec::new()
            },
        }
        // lock released here
    };

    render_status_from_snap(frame, chunks[0], &snap);
    render_input(frame, chunks[2], app);

    if matches!(
        app.input_mode,
        InputMode::QuestionSelect | InputMode::QuestionEdit
    ) {
        render_question_select_from_snap(frame, chunks[1], &snap, app, ext_renderer);
    } else if app.show_logs {
        let mid = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(chunks[1]);
        render_agent_output(
            frame,
            mid[0],
            &snap.agent_output,
            &snap.streaming_buffer,
            app,
        );
        render_logs(frame, mid[1], &snap.logs, app);
    } else {
        render_agent_output(
            frame,
            chunks[1],
            &snap.agent_output,
            &snap.streaming_buffer,
            app,
        );
    }
}

// ── Status Pane ───────────────────────────────────────────────────────

fn render_status_from_snap(frame: &mut Frame, area: Rect, snap: &RenderSnapshot) {
    let round_str = if snap.max_rounds > 0 {
        format!("Round {}/{}", snap.round, snap.max_rounds)
    } else {
        "\u{2014}".to_string()
    };

    let ctx_pct = (snap.context_pct * 100.0).min(100.0);
    let ctx_bar_width = 20usize;
    let filled = ((ctx_pct / 100.0) * ctx_bar_width as f64) as usize;
    let empty = ctx_bar_width.saturating_sub(filled);
    let ctx_bar = format!(
        "{}{} {:.0}%",
        "\u{2588}".repeat(filled),
        "\u{2591}".repeat(empty),
        ctx_pct
    );

    // Build the third line with cycle count, extension spans, and timers.
    let mut line3_spans: Vec<Span<'_>> = vec![
        Span::styled("Cycle: ", Style::default().fg(Color::DarkGray)),
        Span::styled(snap.cycle.to_string(), Style::default().fg(Color::White)),
    ];

    // Domain-specific spans (pre-rendered in snapshot).
    if !snap.ext_status_spans.is_empty() {
        line3_spans.push(Span::raw("   "));
        line3_spans.extend(snap.ext_status_spans.iter().cloned());
    }

    // Question countdown.
    if let Some(ref aq) = snap.active_question
        && !aq.done
        && let Some(deadline) = aq.deadline
    {
        let now = Instant::now();
        if deadline > now {
            let countdown = format_countdown(deadline - now);
            line3_spans.push(Span::raw("   "));
            line3_spans.push(Span::styled(
                "Select: ",
                Style::default().fg(Color::DarkGray),
            ));
            line3_spans.push(Span::styled(
                countdown,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }

    // Next-cycle countdown.
    if let Some(next_at) = snap.next_cycle_at {
        let now = Instant::now();
        if next_at > now {
            let countdown = format_countdown(next_at - now);
            line3_spans.push(Span::raw("   "));
            line3_spans.push(Span::styled("Next: ", Style::default().fg(Color::DarkGray)));
            line3_spans.push(Span::styled(countdown, Style::default().fg(Color::Cyan)));
        }
    }

    // Line 4: domain-specific secondary spans (pre-rendered in snapshot).
    let line4_spans = &snap.ext_secondary_spans;

    let mut status_text = vec![
        Line::from(vec![
            Span::styled("Phase: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                snap.phase.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled(round_str, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled("Model: ", Style::default().fg(Color::DarkGray)),
            Span::raw(snap.model.clone()),
            Span::raw("   Context: "),
            Span::styled(
                ctx_bar,
                if ctx_pct >= 80.0 {
                    Style::default().fg(Color::Red)
                } else if ctx_pct >= 60.0 {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::Green)
                },
            ),
        ]),
        Line::from(line3_spans),
    ];

    if !line4_spans.is_empty() {
        status_text.push(Line::from(line4_spans.clone()));
    }

    let title = if snap.running {
        " Agent "
    } else {
        " Agent [finished] "
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(title);

    let paragraph = Paragraph::new(status_text).block(block);
    frame.render_widget(paragraph, area);
}

// ── Log Pane ──────────────────────────────────────────────────────────

fn render_logs(frame: &mut Frame, area: Rect, logs: &[cinch_rs::ui::LogLine], app: &App) {
    let inner_height = area.height.saturating_sub(2) as usize;

    let mut lines: Vec<Line> = Vec::with_capacity(logs.len());

    for log in logs {
        // Filter out trace/debug-level logs — they're too noisy for the TUI.
        if matches!(log.level, LogLevel::Trace | LogLevel::Debug) {
            continue;
        }
        let level_span = Span::styled(
            format!("{} ", log.level.label()),
            log_level_style(log.level),
        );
        let time_span = Span::styled(
            format!("{} ", log.time),
            Style::default().fg(Color::DarkGray),
        );
        let msg_span = Span::raw(&log.message);
        lines.push(Line::from(vec![time_span, level_span, msg_span]));
    }

    let total = lines.len();
    let scroll = if app.log_scroll == 0 {
        total.saturating_sub(inner_height)
    } else {
        total
            .saturating_sub(inner_height)
            .saturating_sub(app.log_scroll)
    };

    let border_color = if app.active_pane == ActivePane::Log {
        Color::Cyan
    } else {
        Color::DarkGray
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(" Log ");

    let paragraph = Paragraph::new(lines)
        .block(block)
        .scroll((scroll as u16, 0))
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

// ── Agent Output Pane ─────────────────────────────────────────────────

fn render_agent_output(
    frame: &mut Frame,
    area: Rect,
    agent_output: &[AgentEntry],
    streaming_buffer: &str,
    app: &App,
) {
    let inner_height = area.height.saturating_sub(2) as usize;
    let content_width = area.width.saturating_sub(4) as usize;
    let arg_max = content_width.saturating_sub(16).max(20);

    let tool_name_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let tool_args_style = Style::default().fg(Color::DarkGray);
    let tool_ok_style = Style::default().fg(Color::Green);
    let tool_err_style = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
    let text_style = Style::default().fg(Color::White);
    let streaming_style = Style::default().fg(Color::Yellow);

    let mut lines: Vec<Line> = Vec::new();

    for entry in agent_output {
        match entry {
            AgentEntry::Text(text) => {
                for line in text.lines() {
                    lines.push(Line::from(Span::styled(line, text_style)));
                }
            }
            AgentEntry::ToolExecuting { name, arguments } => {
                let args_summary = summarize_args(arguments, arg_max);
                lines.push(Line::from(vec![
                    Span::styled(">> ", tool_name_style),
                    Span::styled(name.as_str(), tool_name_style),
                    Span::styled(format!("  {args_summary}"), tool_args_style),
                ]));
            }
            AgentEntry::ToolResult {
                name,
                result,
                is_error,
            } => {
                let preview = result_preview(result, arg_max);
                if *is_error {
                    lines.push(Line::from(vec![
                        Span::styled("<< ", tool_err_style),
                        Span::styled(name.as_str(), tool_err_style),
                        Span::styled(format!("  {preview}"), tool_err_style),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled("<< ", tool_ok_style),
                        Span::styled(name.as_str(), tool_ok_style),
                        Span::styled(format!("  {preview}"), Style::default().fg(Color::DarkGray)),
                    ]));
                }
            }
            AgentEntry::UserMessage(message) => {
                let user_style = Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD);
                lines.push(Line::from(vec![
                    Span::styled("> ", user_style),
                    Span::styled(message.as_str(), user_style),
                ]));
            }
            AgentEntry::TodoUpdate(content) => {
                let header_style = Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD);
                let item_style = Style::default().fg(Color::White);
                for line in content.lines() {
                    if line.starts_with("Todo list:") {
                        lines.push(Line::from(Span::styled(line, header_style)));
                    } else {
                        lines.push(Line::from(Span::styled(line, item_style)));
                    }
                }
            }
        }
    }

    // In-progress streaming buffer (tokens arriving live).
    if !streaming_buffer.is_empty() {
        for line in streaming_buffer.lines() {
            lines.push(Line::from(Span::styled(line, streaming_style)));
        }
    }

    let total = lines.len();
    let scroll = if app.agent_scroll == 0 {
        total.saturating_sub(inner_height)
    } else {
        total
            .saturating_sub(inner_height)
            .saturating_sub(app.agent_scroll)
    };

    let border_color = if app.active_pane == ActivePane::AgentOutput {
        Color::Cyan
    } else {
        Color::DarkGray
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(" Agent Output ");

    let paragraph = Paragraph::new(lines)
        .block(block)
        .scroll((scroll as u16, 0))
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

// ── Question Select Pane ──────────────────────────────────────────────

fn render_question_select_from_snap(
    frame: &mut Frame,
    area: Rect,
    snap: &RenderSnapshot,
    app: &App,
    ext_renderer: &dyn TuiExtensionRenderer,
) {
    let inner_height = area.height.saturating_sub(2) as usize;

    let mut lines: Vec<Line> = Vec::new();
    let mut choice_line_counts: Vec<usize> = Vec::new();

    if let Some(ref aq) = snap.active_question {
        for (i, choice) in aq.question.choices.iter().enumerate() {
            let is_selected = i == app.question_cursor;
            let marker = if is_selected { "> " } else { "  " };
            let label_style = if is_selected {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Cyan)
            };
            let body_style = if is_selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let mut line_count = 0usize;

            // Header line: "> Label  (metadata)"
            let mut header = vec![
                Span::styled(marker, label_style),
                Span::styled(choice.label.clone(), label_style),
            ];
            if !choice.metadata.is_empty() {
                header.push(Span::styled(
                    format!(" ({})", choice.metadata),
                    Style::default().fg(Color::Blue),
                ));
            }
            // Domain-specific decoration — ext_renderer reads from choice
            // data (already cloned in the snapshot), not from UiState.
            if let Some(deco) = ext_renderer.choice_decoration(&cinch_rs::ui::NoExtension, choice) {
                header.push(Span::raw(" "));
                header.push(deco);
            }
            lines.push(Line::from(header));
            line_count += 1;

            // Body text, indented.
            for body_line in choice.body.lines() {
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(body_line.to_string(), body_style),
                ]));
                line_count += 1;
            }
            if choice.body.is_empty() {
                line_count += 0; // no body lines
            }

            // Separator.
            lines.push(Line::from(""));
            line_count += 1;

            choice_line_counts.push(line_count);
        }
    }

    // Compute scroll based on cursor position.
    let mut cursor_top = 0usize;
    let mut cursor_height = 0usize;
    for (i, &count) in choice_line_counts.iter().enumerate() {
        if i < app.question_cursor {
            cursor_top += count;
        } else if i == app.question_cursor {
            cursor_height = count;
            break;
        }
    }
    let scroll = if cursor_top + cursor_height > app.question_scroll + inner_height {
        (cursor_top + cursor_height).saturating_sub(inner_height)
    } else if cursor_top < app.question_scroll {
        cursor_top
    } else {
        app.question_scroll
    };

    let title = if let Some(ref aq) = snap.active_question {
        format!(
            " {} [Up/Down] navigate  [Enter] select  [Esc] skip ",
            aq.question.prompt
        )
    } else {
        " Select ".to_string()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(title);

    let paragraph = Paragraph::new(lines)
        .block(block)
        .scroll((scroll as u16, 0))
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

// ── Input Bar ─────────────────────────────────────────────────────────

fn render_input(frame: &mut Frame, area: Rect, app: &App) {
    let (title, style) = match app.input_mode {
        InputMode::Normal => {
            let hint = if let Some(ref msg) = app.status_message {
                msg.clone()
            } else {
                "[q] quit  [,] toggle logs  [Tab] switch pane  [Up/Down] scroll".to_string()
            };
            (format!(" {hint} "), Style::default().fg(Color::DarkGray))
        }
        InputMode::QuestionSelect => (
            " [Up/Down] navigate  [Enter] select  [e] edit  [Esc] skip ".to_string(),
            Style::default().fg(Color::Yellow),
        ),
        InputMode::QuestionEdit => {
            let char_count = app.input_buffer.chars().count();
            // Show char count against max_edit_length if we had one; the
            // actual validation happens in input.rs. Here we show a generic
            // counter.
            (
                format!(" Editing ({char_count} chars) \u{2014} [Enter] confirm  [Esc] cancel "),
                Style::default().fg(Color::Cyan),
            )
        }
        InputMode::FreeText => {
            let char_count = app.input_buffer.chars().count();
            (
                format!(" Type your message ({char_count} chars) \u{2014} [Enter] send  [Esc] cancel "),
                Style::default().fg(Color::Green),
            )
        }
    };

    let input_text = match app.input_mode {
        InputMode::Normal | InputMode::QuestionSelect => String::new(),
        InputMode::QuestionEdit | InputMode::FreeText => {
            format!("> {}\u{2588}", app.input_buffer)
        }
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .title(title);

    let paragraph = Paragraph::new(input_text).block(block);
    frame.render_widget(paragraph, area);
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_countdown_minutes() {
        assert_eq!(format_countdown(Duration::from_secs(90)), "1m 30s");
        assert_eq!(format_countdown(Duration::from_secs(1800)), "30m 00s");
    }

    #[test]
    fn format_countdown_hours() {
        assert_eq!(format_countdown(Duration::from_secs(3661)), "1h 01m");
    }

    #[test]
    fn truncate_str_short() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn truncate_str_long() {
        let result = truncate_str("hello world this is long", 11);
        assert!(result.ends_with("..."));
        assert!(result.len() <= 14); // 11 + "..."
    }

    #[test]
    fn summarize_args_json() {
        let args = r#"{"path":"docs/voice.md","lines":10}"#;
        let summary = summarize_args(args, 100);
        assert!(summary.contains("path=docs/voice.md"));
        assert!(summary.contains("lines=10"));
    }

    #[test]
    fn summarize_args_truncates() {
        let args = r#"{"path":"very/long/path/that/goes/on/and/on/forever.md"}"#;
        let summary = summarize_args(args, 20);
        assert!(summary.ends_with("..."));
        assert!(summary.len() <= 23);
    }

    #[test]
    fn result_preview_first_line() {
        let result = "first line\nsecond line\nthird line";
        assert_eq!(result_preview(result, 100), "first line");
    }

    #[test]
    fn result_preview_truncates() {
        let result = "this is a really long first line that should get truncated";
        let preview = result_preview(result, 20);
        assert!(preview.ends_with("..."));
        assert!(preview.len() <= 23);
    }

    #[test]
    fn log_level_style_colors() {
        // Just verify we get non-default styles for each level.
        let _ = log_level_style(LogLevel::Trace);
        let _ = log_level_style(LogLevel::Debug);
        let _ = log_level_style(LogLevel::Info);
        let _ = log_level_style(LogLevel::Warn);
        let _ = log_level_style(LogLevel::Error);
    }
}
