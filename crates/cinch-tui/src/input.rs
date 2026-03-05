//! Generic input handling for the harness TUI.

use std::sync::{Arc, Mutex};

use cinch_rs::ui::{QuestionResponse, UiState};
use crossterm::event::{KeyCode, KeyModifiers};

use crate::app::{ActivePane, App, InputMode};

pub(crate) fn handle_key_event(
    key: crossterm::event::KeyEvent,
    app: &mut App,
    state: &Arc<Mutex<UiState>>,
) {
    // Ctrl+C always quits.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.should_quit = true;
        return;
    }

    match app.input_mode {
        InputMode::Normal => handle_normal_key(key, app, state),
        InputMode::QuestionSelect => handle_question_select_key(key, app, state),
        InputMode::QuestionEdit => handle_question_edit_key(key, app, state),
        InputMode::FreeText => handle_free_text_key(key, app, state),
        InputMode::ContextView => handle_context_view_key(key, app),
    }
}

fn handle_normal_key(key: crossterm::event::KeyEvent, app: &mut App, state: &Arc<Mutex<UiState>>) {
    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Esc => {
            if app.agent_expanded.is_some() {
                // Close expanded agent entry first.
                app.agent_expanded = None;
            } else {
                state.lock().unwrap().interrupt_requested = true;
            }
        }
        KeyCode::Char(',') => {
            app.show_logs = !app.show_logs;
            if app.show_logs {
                app.active_pane = ActivePane::Log;
            } else {
                app.active_pane = ActivePane::AgentOutput;
            }
        }
        KeyCode::Char('c') => {
            app.input_mode = InputMode::ContextView;
            app.context_scroll = 0;
            app.context_cursor = 0;
            app.context_expanded = None;
        }
        KeyCode::Enter => {
            // Toggle expansion of the highlighted agent entry.
            if app.active_pane == ActivePane::AgentOutput {
                if app.agent_expanded == Some(app.agent_cursor) {
                    app.agent_expanded = None;
                    app.agent_expand_scroll = 0;
                } else {
                    app.agent_expanded = Some(app.agent_cursor);
                    app.agent_expand_scroll = 0;
                }
            }
        }
        KeyCode::Tab | KeyCode::BackTab => {
            if app.show_logs {
                app.active_pane = match app.active_pane {
                    ActivePane::Log => ActivePane::AgentOutput,
                    ActivePane::AgentOutput => ActivePane::Log,
                };
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if app.active_pane == ActivePane::AgentOutput {
                if app.agent_expanded.is_some() {
                    // Scroll within expanded content.
                    app.agent_expand_scroll = app.agent_expand_scroll.saturating_sub(1);
                } else {
                    app.agent_cursor = app.agent_cursor.saturating_sub(1);
                }
            } else {
                app.log_scroll = app.log_scroll.saturating_add(3);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.active_pane == ActivePane::AgentOutput {
                if app.agent_expanded.is_some() {
                    // Scroll within expanded content.
                    app.agent_expand_scroll = app.agent_expand_scroll.saturating_add(1);
                    // Clamping happens in the render function.
                } else {
                    app.agent_cursor = app.agent_cursor.saturating_add(1);
                    // Clamping happens in the render function.
                }
            } else {
                app.log_scroll = app.log_scroll.saturating_sub(3);
            }
        }
        KeyCode::PageUp => {
            if app.active_pane == ActivePane::AgentOutput {
                if app.agent_expanded.is_some() {
                    app.agent_expand_scroll = app.agent_expand_scroll.saturating_sub(10);
                } else {
                    app.agent_cursor = app.agent_cursor.saturating_sub(10);
                }
            } else {
                app.log_scroll = app.log_scroll.saturating_add(20);
            }
        }
        KeyCode::PageDown => {
            if app.active_pane == ActivePane::AgentOutput {
                if app.agent_expanded.is_some() {
                    app.agent_expand_scroll = app.agent_expand_scroll.saturating_add(10);
                    // Clamping happens in the render function.
                } else {
                    app.agent_cursor = app.agent_cursor.saturating_add(10);
                }
            } else {
                app.log_scroll = app.log_scroll.saturating_sub(20);
            }
        }
        KeyCode::Home => {
            if app.active_pane == ActivePane::AgentOutput {
                if app.agent_expanded.is_some() {
                    app.agent_expand_scroll = 0;
                } else {
                    app.agent_cursor = 0;
                }
            }
        }
        KeyCode::End => {
            if app.active_pane == ActivePane::AgentOutput {
                if app.agent_expanded.is_some() {
                    app.agent_expand_scroll = usize::MAX; // clamped in render
                } else {
                    app.agent_cursor = usize::MAX; // clamped in render
                }
            } else {
                app.log_scroll = 0; // follow tail
            }
        }
        _ => {}
    }
}

fn handle_question_select_key(
    key: crossterm::event::KeyEvent,
    app: &mut App,
    state: &Arc<Mutex<UiState>>,
) {
    let choice_count = state
        .lock()
        .ok()
        .and_then(|s| {
            s.active_question
                .as_ref()
                .map(|aq| aq.question.choices.len())
        })
        .unwrap_or(0);

    if choice_count == 0 {
        app.input_mode = InputMode::Normal;
        return;
    }

    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            app.question_cursor = app.question_cursor.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.question_cursor + 1 < choice_count {
                app.question_cursor += 1;
            }
        }
        KeyCode::Enter => {
            // Select and confirm this choice.
            {
                let mut s = state.lock().unwrap();
                if let Some(ref mut aq) = s.active_question {
                    aq.response = Some(QuestionResponse::Selected(app.question_cursor));
                    aq.done = true;
                }
            }
            let choice_num = app.question_cursor + 1;
            app.input_mode = InputMode::Normal;
            app.status_message = Some(format!("Choice {choice_num} selected."));
        }
        KeyCode::Char('e') => {
            // Enter edit mode if the question allows editing.
            let editable_and_body = state.lock().ok().and_then(|s| {
                s.active_question.as_ref().and_then(|aq| {
                    if aq.question.editable {
                        aq.question
                            .choices
                            .get(app.question_cursor)
                            .map(|c| c.body.clone())
                    } else {
                        None
                    }
                })
            });
            if let Some(body) = editable_and_body {
                app.input_buffer = body;
                app.input_mode = InputMode::QuestionEdit;
            }
        }
        KeyCode::Esc => {
            // User explicitly skipped.
            {
                let mut s = state.lock().unwrap();
                if let Some(ref mut aq) = s.active_question {
                    aq.response = Some(QuestionResponse::Skipped);
                    aq.done = true;
                }
            }
            app.input_mode = InputMode::Normal;
            app.status_message = Some("Selection skipped.".into());
        }
        _ => {}
    }
}

fn handle_question_edit_key(
    key: crossterm::event::KeyEvent,
    app: &mut App,
    state: &Arc<Mutex<UiState>>,
) {
    match key.code {
        KeyCode::Esc => {
            // Cancel edit — return to question selection.
            app.input_buffer.clear();
            app.input_mode = InputMode::QuestionSelect;
        }
        KeyCode::Enter => {
            // Confirm the edited text.
            let edited = app.input_buffer.trim().to_string();
            if edited.is_empty() {
                app.status_message = Some("Edit is empty \u{2014} cancelled.".into());
                app.input_buffer.clear();
                app.input_mode = InputMode::QuestionSelect;
                return;
            }

            // Validate against max_edit_length if set.
            let max_len = state
                .lock()
                .ok()
                .and_then(|s| {
                    s.active_question
                        .as_ref()
                        .and_then(|aq| aq.question.max_edit_length)
                })
                .unwrap_or(usize::MAX);
            let char_count = edited.chars().count();
            if char_count > max_len {
                app.status_message = Some(format!(
                    "Text is {char_count} chars (max {max_len}) \u{2014} shorten it first."
                ));
                return;
            }

            {
                let mut s = state.lock().unwrap();
                if let Some(ref mut aq) = s.active_question {
                    aq.response = Some(QuestionResponse::SelectedEdited {
                        index: app.question_cursor,
                        edited_text: edited,
                    });
                    aq.done = true;
                }
            }
            let choice_num = app.question_cursor + 1;
            app.input_buffer.clear();
            app.input_mode = InputMode::Normal;
            app.status_message = Some(format!(
                "Choice {choice_num} selected with edits ({char_count} chars)."
            ));
        }
        KeyCode::Backspace => {
            app.input_buffer.pop();
        }
        KeyCode::Char(c) => {
            app.input_buffer.push(c);
        }
        _ => {}
    }
}

fn handle_free_text_key(
    key: crossterm::event::KeyEvent,
    app: &mut App,
    state: &Arc<Mutex<UiState>>,
) {
    match key.code {
        KeyCode::Enter => {
            let text = app.input_buffer.trim().to_string();
            if text.is_empty() {
                return;
            }
            {
                let mut s = state.lock().unwrap();
                if let Some(ref mut aq) = s.active_question {
                    aq.response = Some(QuestionResponse::FreeText(text));
                    aq.done = true;
                }
            }
            app.input_buffer.clear();
            app.input_mode = InputMode::Normal;
            app.status_message = None;
        }
        KeyCode::Esc => {
            {
                let mut s = state.lock().unwrap();
                if let Some(ref mut aq) = s.active_question {
                    aq.response = Some(QuestionResponse::Skipped);
                    aq.done = true;
                }
            }
            app.input_buffer.clear();
            app.input_mode = InputMode::Normal;
            app.status_message = Some("Input cancelled.".into());
        }
        KeyCode::Backspace => {
            app.input_buffer.pop();
        }
        KeyCode::Char(c) => {
            app.input_buffer.push(c);
        }
        // Pass through navigation keys so the user can scroll and
        // switch panes while typing.
        KeyCode::Up => {
            app.log_scroll = app.log_scroll.saturating_add(3);
        }
        KeyCode::Down => {
            app.log_scroll = app.log_scroll.saturating_sub(3);
        }
        KeyCode::PageUp => {
            app.log_scroll = app.log_scroll.saturating_add(20);
        }
        KeyCode::PageDown => {
            app.log_scroll = app.log_scroll.saturating_sub(20);
        }
        KeyCode::End => {
            app.log_scroll = 0;
        }
        KeyCode::Tab | KeyCode::BackTab => {
            if app.show_logs {
                app.active_pane = match app.active_pane {
                    ActivePane::Log => ActivePane::AgentOutput,
                    ActivePane::AgentOutput => ActivePane::Log,
                };
            }
        }
        _ => {}
    }
}

fn handle_context_view_key(key: crossterm::event::KeyEvent, app: &mut App) {
    match key.code {
        KeyCode::Char('c') | KeyCode::Esc => {
            if app.context_expanded.is_some() {
                // Close expanded message first.
                app.context_expanded = None;
            } else {
                app.input_mode = InputMode::Normal;
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.context_cursor = app.context_cursor.saturating_sub(1);
            app.context_expanded = None;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.context_cursor = app.context_cursor.saturating_add(1);
            // Clamping happens in the render function which knows the count.
            app.context_expanded = None;
        }
        KeyCode::PageUp => {
            app.context_cursor = app.context_cursor.saturating_sub(10);
            app.context_expanded = None;
        }
        KeyCode::PageDown => {
            app.context_cursor = app.context_cursor.saturating_add(10);
            app.context_expanded = None;
        }
        KeyCode::Home => {
            app.context_cursor = 0;
            app.context_expanded = None;
        }
        KeyCode::End => {
            app.context_cursor = usize::MAX; // clamped in render
            app.context_expanded = None;
        }
        KeyCode::Enter => {
            // Toggle expansion of the highlighted message.
            if app.context_expanded == Some(app.context_cursor) {
                app.context_expanded = None;
            } else {
                app.context_expanded = Some(app.context_cursor);
            }
        }
        _ => {}
    }
}
