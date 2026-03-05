//! Generic rendering for the harness TUI.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cinch_rs::ui::{AgentEntry, ContextSnapshot, LogLevel, UiState};
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

/// Truncate a string to a maximum byte length, appending "..." if truncated.
///
/// Uses [`str::floor_char_boundary`] so the cut never falls inside a
/// multi-byte UTF-8 character.
#[allow(clippy::string_slice)] // end from floor_char_boundary
pub fn truncate_str(s: &str, max: usize) -> String {
    if s.len() > max {
        let end = s.floor_char_boundary(max);
        format!("{}...", &s[..end])
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
///
/// Palette is optimised for light-mode terminals and colour-blind
/// accessibility (no red/green-only distinctions, no yellow/cyan that
/// wash out on light backgrounds).
pub fn log_level_style(level: LogLevel) -> Style {
    match level {
        LogLevel::Trace => Style::default().fg(Color::DarkGray),
        LogLevel::Debug => Style::default().fg(Color::DarkGray),
        LogLevel::Info => Style::default().fg(Color::Blue),
        LogLevel::Warn => Style::default().fg(Color::Magenta),
        LogLevel::Error => Style::default()
            .fg(Color::Red)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
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

    // Context window snapshot.
    context_snapshot: Option<ContextSnapshot>,
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
            context_snapshot: if matches!(app.input_mode, InputMode::ContextView) {
                s.context_snapshot.clone()
            } else {
                None
            },
        }
        // lock released here
    };

    render_status_from_snap(frame, chunks[0], &snap);
    render_input(frame, chunks[2], app);

    if matches!(app.input_mode, InputMode::ContextView) {
        render_context_view(frame, chunks[1], &snap, app);
    } else if matches!(
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
        Span::styled(snap.cycle.to_string(), Style::default().fg(Color::Black)),
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
                    .fg(Color::Magenta)
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
            line3_spans.push(Span::styled(countdown, Style::default().fg(Color::Blue)));
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
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled(round_str, Style::default().fg(Color::Blue)),
        ]),
        Line::from(vec![
            Span::styled("Model: ", Style::default().fg(Color::DarkGray)),
            Span::raw(snap.model.clone()),
            Span::raw("   Context: "),
            Span::styled(
                ctx_bar,
                if ctx_pct >= 80.0 {
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
                } else if ctx_pct >= 60.0 {
                    Style::default().fg(Color::Magenta)
                } else {
                    Style::default().fg(Color::Blue)
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
        .borders(Borders::TOP | Borders::BOTTOM)
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
        Color::Blue
    } else {
        Color::DarkGray
    };

    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(border_color))
        .title(" Log ");

    let paragraph = Paragraph::new(lines)
        .block(block)
        .scroll((scroll as u16, 0))
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

// ── Agent Output Pane ─────────────────────────────────────────────────

/// Maximum lines shown per entry in the collapsed (non-expanded) preview.
const PREVIEW_LINES: usize = 4;

/// Extract the full displayable content from an agent entry.
fn entry_full_content(entry: &AgentEntry) -> &str {
    match entry {
        AgentEntry::Text(t) => t.as_str(),
        AgentEntry::ToolExecuting { arguments, .. } => arguments.as_str(),
        AgentEntry::ToolResult { result, .. } => result.as_str(),
        AgentEntry::UserMessage(m) => m.as_str(),
        AgentEntry::TodoUpdate(c) => c.as_str(),
    }
}

/// Count the number of display lines an entry takes in collapsed preview mode.
fn entry_preview_line_count(entry: &AgentEntry) -> usize {
    let total = match entry {
        AgentEntry::Text(t) => t.lines().count().max(1),
        AgentEntry::ToolExecuting { .. } => 1, // header only
        AgentEntry::ToolResult { result, .. } => 1 + result.lines().count().max(1), // header + content
        AgentEntry::UserMessage(m) => m.lines().count().max(1),
        AgentEntry::TodoUpdate(c) => c.lines().count().max(1),
    };
    // Cap at PREVIEW_LINES, plus 1 for "..." if truncated.
    if total > PREVIEW_LINES {
        PREVIEW_LINES + 1
    } else {
        total
    }
}

/// Count the number of display lines an entry takes when fully expanded.
fn entry_expanded_line_count(entry: &AgentEntry, expand_width: usize) -> usize {
    let full_content = entry_full_content(entry);
    let content_lines: usize = full_content
        .lines()
        .map(|l| {
            if l.len() > expand_width {
                l.len().div_ceil(expand_width)
            } else {
                1
            }
        })
        .sum();
    content_lines + 2 // blank lines around expansion
}

/// Push indented preview lines (after the header) for multi-line content,
/// up to `remaining` additional lines. Returns whether content was truncated.
fn push_preview_lines<'a>(
    lines: &mut Vec<Line<'a>>,
    content_lines: impl Iterator<Item = &'a str>,
    remaining: usize,
    total_lines: usize,
    style: Style,
) {
    let indent = "     ";
    for line in content_lines.take(remaining) {
        lines.push(Line::from(vec![
            Span::raw(indent),
            Span::styled(line, style),
        ]));
    }
    if total_lines > remaining {
        lines.push(Line::from(Span::styled(
            format!("{indent}..."),
            Style::default().fg(Color::DarkGray),
        )));
    }
}

/// Push the full expanded content with word-wrapping and indentation.
fn push_expanded_content<'a>(
    lines: &mut Vec<Line<'a>>,
    full_content: &'a str,
    expand_width: usize,
) {
    lines.push(Line::from(""));
    for content_line in full_content.lines() {
        if content_line.len() > expand_width && expand_width > 0 {
            let mut remaining = content_line;
            while !remaining.is_empty() {
                let end = remaining.floor_char_boundary(expand_width.min(remaining.len()));
                let end = if end == 0 {
                    remaining.len().min(1)
                } else {
                    end
                };
                #[allow(clippy::string_slice)] // end from floor_char_boundary
                let chunk = &remaining[..end];
                lines.push(Line::from(vec![
                    Span::raw("      "),
                    Span::styled(chunk, Style::default().fg(Color::Black)),
                ]));
                #[allow(clippy::string_slice)] // end from floor_char_boundary
                {
                    remaining = &remaining[end..];
                }
            }
        } else {
            lines.push(Line::from(vec![
                Span::raw("      "),
                Span::styled(content_line, Style::default().fg(Color::Black)),
            ]));
        }
    }
    lines.push(Line::from(""));
}

fn render_agent_output(
    frame: &mut Frame,
    area: Rect,
    agent_output: &[AgentEntry],
    streaming_buffer: &str,
    app: &App,
) {
    let inner_height = area.height.saturating_sub(2) as usize;
    let content_width = area.width.saturating_sub(2) as usize;
    let arg_max = content_width.saturating_sub(16).max(20);

    let entry_count = agent_output.len();
    let cursor = app.agent_cursor.min(entry_count.saturating_sub(1));

    let tool_name_style = Style::default()
        .fg(Color::Blue)
        .add_modifier(Modifier::BOLD);
    let tool_args_style = Style::default().fg(Color::Black);
    let tool_ok_style = Style::default().fg(Color::Blue);
    let tool_err_style = Style::default()
        .fg(Color::Red)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let text_style = Style::default().fg(Color::Black);
    let dim_style = Style::default()
        .fg(Color::Black)
        .add_modifier(Modifier::DIM);
    let streaming_style = Style::default().fg(Color::Magenta);

    let mut lines: Vec<Line> = Vec::new();

    for (i, entry) in agent_output.iter().enumerate() {
        let is_cursor = i == cursor;
        let marker = if is_cursor { "> " } else { "  " };
        let row_style = if is_cursor {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        match entry {
            AgentEntry::Text(text) => {
                let total = text.lines().count().max(1);
                let mut text_lines = text.lines();
                // First line with marker.
                let first = text_lines.next().unwrap_or("");
                lines.push(Line::from(vec![
                    Span::styled(marker, row_style),
                    Span::styled(first, text_style),
                ]));
                push_preview_lines(
                    &mut lines,
                    text_lines,
                    PREVIEW_LINES - 1,
                    total - 1,
                    text_style,
                );
            }
            AgentEntry::ToolExecuting { name, arguments } => {
                let args_summary = summarize_args(arguments, arg_max);
                lines.push(Line::from(vec![
                    Span::styled(marker, row_style),
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
                let style = if *is_error {
                    tool_err_style
                } else {
                    tool_ok_style
                };
                // Header line with tool name.
                lines.push(Line::from(vec![
                    Span::styled(marker, row_style),
                    Span::styled("<< ", style),
                    Span::styled(name.as_str(), style),
                ]));
                // Preview lines of the result content.
                let total = result.lines().count().max(1);
                let content_style = if *is_error { tool_err_style } else { dim_style };
                push_preview_lines(
                    &mut lines,
                    result.lines(),
                    PREVIEW_LINES - 1,
                    total,
                    content_style,
                );
            }
            AgentEntry::UserMessage(message) => {
                let user_style = Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD);
                let total = message.lines().count().max(1);
                let mut msg_lines = message.lines();
                let first = msg_lines.next().unwrap_or("");
                lines.push(Line::from(vec![
                    Span::styled(marker, row_style),
                    Span::styled("> ", user_style),
                    Span::styled(first, user_style),
                ]));
                push_preview_lines(
                    &mut lines,
                    msg_lines,
                    PREVIEW_LINES - 1,
                    total - 1,
                    user_style,
                );
            }
            AgentEntry::TodoUpdate(content) => {
                let header_style = Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD);
                let item_style = Style::default().fg(Color::Black);
                let total = content.lines().count().max(1);
                let mut content_lines = content.lines();
                // First line with marker.
                if let Some(first) = content_lines.next() {
                    let style = if first.starts_with("Todo list:") {
                        header_style
                    } else {
                        item_style
                    };
                    lines.push(Line::from(vec![
                        Span::styled(marker, row_style),
                        Span::styled(first, style),
                    ]));
                }
                push_preview_lines(
                    &mut lines,
                    content_lines,
                    PREVIEW_LINES - 1,
                    total - 1,
                    item_style,
                );
            }
        }

        // If this entry is expanded, show full content with word-wrapping.
        if app.agent_expanded == Some(i) {
            let full_content = entry_full_content(entry);
            let expand_width = content_width.saturating_sub(6);
            push_expanded_content(&mut lines, full_content, expand_width);
        }
    }

    // In-progress streaming buffer (tokens arriving live).
    if !streaming_buffer.is_empty() {
        for line in streaming_buffer.lines() {
            lines.push(Line::from(Span::styled(line, streaming_style)));
        }
    }

    // ── Scroll computation ──
    // Find the line index where the cursor row starts, then auto-scroll
    // to keep the cursor visible.
    let mut cursor_line_start = 0usize;
    let mut cursor_line_count = 1usize;
    {
        let expand_width = content_width.saturating_sub(6).max(1);
        let mut line_idx = 0usize;
        for (i, entry) in agent_output.iter().enumerate() {
            let preview_lines = entry_preview_line_count(entry);
            if i == cursor {
                cursor_line_start = line_idx;
                cursor_line_count = preview_lines;
                if app.agent_expanded == Some(i) {
                    cursor_line_count += entry_expanded_line_count(entry, expand_width);
                }
                break;
            }
            line_idx += preview_lines;
            if app.agent_expanded == Some(i) {
                line_idx += entry_expanded_line_count(entry, expand_width);
            }
        }
    }

    // Scroll: when expanded, scroll within the expanded content;
    // otherwise auto-scroll to keep the cursor row visible.
    let scroll = if app.agent_expanded.is_some() {
        // Offset from the cursor's summary line into the expanded content.
        let max_scroll = (cursor_line_start + cursor_line_count).saturating_sub(inner_height);
        (cursor_line_start + app.agent_expand_scroll).min(max_scroll)
    } else if cursor_line_start + cursor_line_count > app.agent_scroll + inner_height {
        (cursor_line_start + cursor_line_count).saturating_sub(inner_height)
    } else if cursor_line_start < app.agent_scroll {
        cursor_line_start
    } else {
        app.agent_scroll
    };

    let border_color = if app.active_pane == ActivePane::AgentOutput {
        Color::Blue
    } else {
        Color::DarkGray
    };

    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
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
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Blue)
            };
            let body_style = if is_selected {
                Style::default()
                    .fg(Color::Black)
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
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(Color::Magenta))
        .title(title);

    let paragraph = Paragraph::new(lines)
        .block(block)
        .scroll((scroll as u16, 0))
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

// ── Context View ──────────────────────────────────────────────────────

fn render_context_view(frame: &mut Frame, area: Rect, snap: &RenderSnapshot, app: &App) {
    let inner_height = area.height.saturating_sub(2) as usize;
    let content_width = area.width.saturating_sub(4) as usize;

    let snapshot = match snap.context_snapshot.as_ref() {
        Some(s) => s,
        None => {
            let block = Block::default()
                .borders(Borders::TOP | Borders::BOTTOM)
                .border_style(Style::default().fg(Color::Blue))
                .title(" Context Window ");
            let paragraph =
                Paragraph::new("No context snapshot available yet. Waiting for first round...")
                    .block(block);
            frame.render_widget(paragraph, area);
            return;
        }
    };

    let msg_count = snapshot.messages.len();
    // Clamp cursor (the input handler may have overshot).
    let cursor = app.context_cursor.min(msg_count.saturating_sub(1));

    let mut lines: Vec<Line> = Vec::new();

    // ── Zone summary section ──
    if let Some(ref bd) = snapshot.breakdown {
        let total = bd.total_tokens;
        let max = snapshot.max_tokens.max(1);
        let pct = (total as f64 / max as f64 * 100.0).min(100.0);

        lines.push(Line::from(vec![
            Span::styled("Total: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{total} / {max} tokens ({pct:.0}%)"),
                Style::default()
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));

        // Overall bar.
        let bar_width = content_width.min(40);
        let filled = ((pct / 100.0) * bar_width as f64) as usize;
        let empty = bar_width.saturating_sub(filled);
        let bar_style = if pct >= 80.0 {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else if pct >= 60.0 {
            Style::default().fg(Color::Magenta)
        } else {
            Style::default().fg(Color::Blue)
        };
        lines.push(Line::from(Span::styled(
            format!("{}{}", "\u{2588}".repeat(filled), "\u{2591}".repeat(empty)),
            bar_style,
        )));

        lines.push(Line::from(""));

        // Per-zone bars.
        let zones: &[(&str, usize, Color)] = &[
            ("Prefix ", bd.prefix_tokens, Color::Blue),
            ("History", bd.compressed_history_tokens, Color::Magenta),
            ("Middle ", bd.middle_tokens, Color::DarkGray),
            ("Recency", bd.recency_tokens, Color::Cyan),
        ];
        for &(label, tokens, color) in zones {
            let zone_pct = if total > 0 {
                tokens as f64 / total as f64 * 100.0
            } else {
                0.0
            };
            let zone_bar_width = 12usize;
            let zone_filled = ((zone_pct / 100.0) * zone_bar_width as f64) as usize;
            let zone_empty = zone_bar_width.saturating_sub(zone_filled);
            lines.push(Line::from(vec![
                Span::styled(format!("  {label}  "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{:>6} tok  ", tokens),
                    Style::default().fg(Color::Black),
                ),
                Span::styled(
                    format!(
                        "{}{}",
                        "\u{2588}".repeat(zone_filled),
                        "\u{2591}".repeat(zone_empty),
                    ),
                    Style::default().fg(color),
                ),
                Span::styled(
                    format!(" {zone_pct:>4.0}%"),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }

        // ── Prompt cache summary (when available) ──
        if let Some(ref cache) = snapshot.prompt_cache {
            let cached = cache.cached_tokens.unwrap_or(0);
            let written = cache.cache_write_tokens.unwrap_or(0);
            if cached > 0 || written > 0 {
                lines.push(Line::from(""));
                let mut spans = vec![Span::styled(
                    "  Cache   ",
                    Style::default().fg(Color::DarkGray),
                )];
                if cached > 0 {
                    spans.push(Span::styled(
                        format!("{cached} tok cached"),
                        Style::default().fg(Color::Green),
                    ));
                }
                if cached > 0 && written > 0 {
                    spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
                }
                if written > 0 {
                    spans.push(Span::styled(
                        format!("{written} tok written"),
                        Style::default().fg(Color::Yellow),
                    ));
                }
                // Show cache hit percentage relative to total prompt.
                if cached > 0 && total > 0 {
                    let cache_pct = cached as f64 / total as f64 * 100.0;
                    spans.push(Span::styled(
                        format!(" ({cache_pct:.0}% hit)"),
                        Style::default().fg(Color::Green),
                    ));
                }
                lines.push(Line::from(spans));
            }
        }

        lines.push(Line::from(""));
    }

    // ── Separator ──
    let sep = "\u{2500}".repeat(content_width.min(60));
    lines.push(Line::from(Span::styled(
        sep,
        Style::default().fg(Color::DarkGray),
    )));

    // ── Header ──
    lines.push(Line::from(vec![
        Span::styled(
            format!("{:>3}  {:<8} {:<10} {:>6}  ", "#", "Zone", "Role", "Tokens"),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "Preview",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    // ── Compute per-message cache status ──
    // Prompt caching is prefix-based: the first `cached_tokens` tokens of the
    // prompt are served from cache. Walk messages to find the boundary.
    let cached_token_limit = snapshot
        .prompt_cache
        .as_ref()
        .and_then(|c| c.cached_tokens)
        .unwrap_or(0) as usize;
    let mut cache_running_total = 0usize;

    // ── Cache legend (when caching info is available) ──
    let has_breakpoints = snapshot.messages.iter().any(|m| m.has_cache_breakpoint);
    if has_breakpoints || cached_token_limit > 0 {
        let mut legend_spans = vec![Span::styled(
            "  Cache: ",
            Style::default().fg(Color::DarkGray),
        )];
        if cached_token_limit > 0 {
            legend_spans.push(Span::styled("\u{25cf}", Style::default().fg(Color::Green)));
            legend_spans.push(Span::styled(
                "cached ",
                Style::default().fg(Color::DarkGray),
            ));
            legend_spans.push(Span::styled("\u{25d1}", Style::default().fg(Color::Yellow)));
            legend_spans.push(Span::styled(
                "partial ",
                Style::default().fg(Color::DarkGray),
            ));
        }
        if has_breakpoints {
            legend_spans.push(Span::styled("\u{2307}", Style::default().fg(Color::Cyan)));
            legend_spans.push(Span::styled(
                "breakpoint",
                Style::default().fg(Color::DarkGray),
            ));
        }
        lines.push(Line::from(legend_spans));
    }

    // ── Message rows ──
    let preview_width = content_width.saturating_sub(35).max(10);

    for (i, msg) in snapshot.messages.iter().enumerate() {
        let is_cursor = i == cursor;

        let zone_color = match msg.zone {
            cinch_rs::context::ContextZone::Prefix => Color::Blue,
            cinch_rs::context::ContextZone::CompressedHistory => Color::Magenta,
            cinch_rs::context::ContextZone::Middle => Color::DarkGray,
            cinch_rs::context::ContextZone::Recency => Color::Cyan,
        };
        let zone_label = format!("{}", msg.zone);

        let role_display = if let Some(ref tn) = msg.tool_name {
            format!("{}/{tn}", msg.role)
        } else {
            msg.role.clone()
        };

        let marker = if is_cursor { "> " } else { "  " };
        let row_style = if msg.evicted {
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM)
        } else if is_cursor {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        let preview = truncate_str(&msg.preview, preview_width);

        // Determine cache status for this message.
        // ● = fully cached (green), ◑ = partially cached (yellow),
        // ⌇ = cache breakpoint requested (cyan, no hit data yet)
        let cache_indicator = if cached_token_limit > 0 {
            let msg_start = cache_running_total;
            cache_running_total += msg.estimated_tokens;
            if cache_running_total <= cached_token_limit {
                // Fully within cached prefix.
                if msg.has_cache_breakpoint {
                    Span::styled(" \u{25cf}\u{2307}", Style::default().fg(Color::Green))
                } else {
                    Span::styled(" \u{25cf}", Style::default().fg(Color::Green))
                }
            } else if msg_start < cached_token_limit {
                // Partially cached (cache boundary falls within this message).
                if msg.has_cache_breakpoint {
                    Span::styled(" \u{25d1}\u{2307}", Style::default().fg(Color::Yellow))
                } else {
                    Span::styled(" \u{25d1}", Style::default().fg(Color::Yellow))
                }
            } else if msg.has_cache_breakpoint {
                // Beyond cache but has breakpoint.
                Span::styled(" \u{2307}", Style::default().fg(Color::Cyan))
            } else {
                Span::raw("")
            }
        } else {
            cache_running_total += msg.estimated_tokens;
            if msg.has_cache_breakpoint {
                Span::styled(" \u{2307}", Style::default().fg(Color::Cyan))
            } else {
                Span::raw("")
            }
        };

        lines.push(Line::from(vec![
            Span::styled(marker, row_style),
            Span::styled(
                format!("{:>3}  ", i + 1),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("{:<8} ", zone_label),
                Style::default().fg(zone_color),
            ),
            Span::styled(
                format!("{:<10} ", truncate_str(&role_display, 10)),
                row_style,
            ),
            Span::styled(format!("{:>6}  ", msg.estimated_tokens), row_style),
            Span::styled(preview, row_style),
            cache_indicator,
        ]));

        // If this message is expanded, show the full content indented.
        if app.context_expanded == Some(i) {
            lines.push(Line::from(""));
            let expand_width = content_width.saturating_sub(6);
            for content_line in msg.full_content.lines() {
                // Word-wrap long lines.
                if content_line.len() > expand_width && expand_width > 0 {
                    let mut remaining = content_line;
                    while !remaining.is_empty() {
                        let end = remaining.floor_char_boundary(expand_width.min(remaining.len()));
                        let end = if end == 0 {
                            remaining.len().min(1)
                        } else {
                            end
                        };
                        #[allow(clippy::string_slice)] // end from floor_char_boundary
                        let chunk = &remaining[..end];
                        lines.push(Line::from(vec![
                            Span::raw("      "),
                            Span::styled(chunk, Style::default().fg(Color::Black)),
                        ]));
                        #[allow(clippy::string_slice)] // end from floor_char_boundary
                        {
                            remaining = &remaining[end..];
                        }
                    }
                } else {
                    lines.push(Line::from(vec![
                        Span::raw("      "),
                        Span::styled(content_line, Style::default().fg(Color::Black)),
                    ]));
                }
            }
            lines.push(Line::from(""));
        }
    }

    // ── Scroll computation ──
    // Find the line index where the cursor row starts.
    let mut cursor_line_start = 0;
    let mut cursor_line_count = 1usize;
    {
        let has_cache_summary = snapshot.prompt_cache.as_ref().is_some_and(|c| {
            c.cached_tokens.unwrap_or(0) > 0 || c.cache_write_tokens.unwrap_or(0) > 0
        });
        let has_cache_legend = has_breakpoints || cached_token_limit > 0;
        let zone_summary_lines = if snapshot.breakdown.is_some() {
            // total + bar + blank + 4 zones + [blank + cache_line] + blank + separator + header + [legend]
            let base = if has_cache_summary { 12 } else { 10 };
            if has_cache_legend { base + 1 } else { base }
        } else {
            // separator + header + [legend]
            if has_cache_legend { 3 } else { 2 }
        };

        let mut line_idx = zone_summary_lines;
        for (i, _msg) in snapshot.messages.iter().enumerate() {
            if i == cursor {
                cursor_line_start = line_idx;
                cursor_line_count = 1;
                if app.context_expanded == Some(i) {
                    // Count expanded lines.
                    let expand_width = content_width.saturating_sub(6).max(1);
                    let extra: usize = _msg
                        .full_content
                        .lines()
                        .map(|l| {
                            if l.len() > expand_width {
                                l.len().div_ceil(expand_width)
                            } else {
                                1
                            }
                        })
                        .sum();
                    cursor_line_count += extra + 2; // blank lines around expansion
                }
                break;
            }
            line_idx += 1;
            if app.context_expanded == Some(i) {
                let expand_width = content_width.saturating_sub(6).max(1);
                let extra: usize = _msg
                    .full_content
                    .lines()
                    .map(|l| {
                        if l.len() > expand_width {
                            l.len().div_ceil(expand_width)
                        } else {
                            1
                        }
                    })
                    .sum();
                line_idx += extra + 2;
            }
        }
    }

    // Ensure cursor is visible by auto-scrolling.
    let scroll = if cursor_line_start + cursor_line_count > app.context_scroll + inner_height {
        (cursor_line_start + cursor_line_count).saturating_sub(inner_height)
    } else if cursor_line_start < app.context_scroll {
        cursor_line_start
    } else {
        app.context_scroll
    };

    let title = format!(
        " Context Window \u{2014} {} messages ",
        snapshot.messages.len()
    );

    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(Color::Blue))
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
            } else if app.agent_busy {
                "[Esc] interrupt  [q] quit  [c] context  [Enter] expand  [,] logs  [Tab] pane  [Up/Down] scroll"
                    .to_string()
            } else {
                "[q] quit  [c] context  [Enter] expand  [,] logs  [Tab] pane  [Up/Down] scroll"
                    .to_string()
            };
            let style = if app.agent_busy {
                Style::default().fg(Color::Magenta)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            (format!(" {hint} "), style)
        }
        InputMode::QuestionSelect => (
            " [Up/Down] navigate  [Enter] select  [e] edit  [Esc] skip ".to_string(),
            Style::default().fg(Color::Magenta),
        ),
        InputMode::QuestionEdit => {
            let char_count = app.input_buffer.chars().count();
            (
                format!(" Editing ({char_count} chars) \u{2014} [Enter] confirm  [Esc] cancel "),
                Style::default().fg(Color::Blue),
            )
        }
        InputMode::FreeText => {
            let char_count = app.input_buffer.chars().count();
            (
                format!(
                    " Type your message ({char_count} chars) \u{2014} [Enter] send  [Esc] cancel "
                ),
                Style::default().fg(Color::Blue),
            )
        }
        InputMode::ContextView => (
            " [Up/Down] navigate  [Enter] expand/collapse  [c/Esc] close ".to_string(),
            Style::default().fg(Color::Blue),
        ),
    };

    let input_text = match app.input_mode {
        InputMode::Normal | InputMode::QuestionSelect | InputMode::ContextView => String::new(),
        InputMode::QuestionEdit | InputMode::FreeText => {
            format!("> {}\u{2588}", app.input_buffer)
        }
    };

    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
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
    fn truncate_str_multibyte_boundary() {
        // '→' is a 3-byte UTF-8 character (bytes 169..172 in a longer string).
        // Cutting in the middle of it must not panic.
        let s = "content=# Agent Memory\n\nSome text with arrows → Recipe → Done";
        // Pick a max that would land inside '→'.
        let arrow_start = s.find('→').unwrap();
        let result = truncate_str(s, arrow_start + 1); // inside the multi-byte char
        assert!(result.ends_with("..."));
        // The cut should back up to the char boundary before '→'.
        assert!(!result.contains('→'));
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
