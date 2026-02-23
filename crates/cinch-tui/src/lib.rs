//! Generic terminal UI for cinch-rs powered agents.
//!
//! Provides a ready-to-use TUI dashboard (ratatui + crossterm) that renders
//! [`UiState`] from `cinch-rs`. Domain-specific rendering is injected via
//! the [`TuiExtensionRenderer`] trait.
//!
//! # Quick start
//!
//! ```ignore
//! use cinch_tui::{TuiConfig, spawn_tui};
//! use cinch_rs::ui::UiState;
//! use std::sync::{Arc, Mutex};
//!
//! let ui_state = Arc::new(Mutex::new(UiState::default()));
//! let config = TuiConfig::default();
//! let handle = spawn_tui(ui_state.clone(), config);
//! // ... run your agent, update ui_state ...
//! handle.join().unwrap();
//! ```

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cinch_rs::ui::UiState;
use cinch_rs::ui::tracing::LogBuffer;
use crossterm::event::{self, Event};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{cursor, execute};
use ratatui::prelude::*;

mod app;
pub mod ext;
mod input;
mod render;

pub use ext::{NoTuiExtension, TuiExtensionRenderer};
pub use render::{format_countdown, log_level_style, result_preview, summarize_args, truncate_str};

use app::{App, InputMode};
use input::handle_key_event;
use render::render;

/// Configuration for the TUI.
pub struct TuiConfig {
    /// Working directory displayed in the status bar.
    pub workdir: PathBuf,
    /// Optional domain-specific renderer.
    pub extension_renderer: Box<dyn TuiExtensionRenderer>,
    /// Optional log buffer from the tracing layer.
    ///
    /// When set, the TUI drains pending log lines from this buffer once
    /// per frame and merges them into `UiState::logs`.  This keeps the
    /// tracing layer's `on_event` completely decoupled from the UiState
    /// lock, preventing log calls from blocking the render thread.
    pub log_buffer: Option<LogBuffer>,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            workdir: PathBuf::from("."),
            extension_renderer: Box::new(NoTuiExtension),
            log_buffer: None,
        }
    }
}

/// Spawn the TUI on a dedicated OS thread.
///
/// The TUI runs until `quit_requested` is set or `running` becomes false
/// and the user presses `q`.
pub fn spawn_tui(state: Arc<Mutex<UiState>>, config: TuiConfig) -> JoinHandle<()> {
    std::thread::spawn(move || {
        if let Err(e) = run_tui(state, &config) {
            eprintln!("TUI error: {e}");
        }
    })
}

/// Run the TUI event loop (blocking). Call this from a dedicated OS thread.
///
/// Returns when the user presses `q` or the agent finishes in `--once` mode.
pub fn run_tui(state: Arc<Mutex<UiState>>, config: &TuiConfig) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, cursor::Hide)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = App::new();

    loop {
        // Check if we should exit.
        let (running, quit) = {
            let s = state.lock().unwrap();
            (s.running, s.quit_requested)
        };
        if app.should_quit || quit {
            state.lock().unwrap().quit_requested = true;
            break;
        }

        // Auto-enter question-select mode when a question becomes available.
        if matches!(app.input_mode, InputMode::Normal) {
            let should_enter = {
                let s = state.lock().unwrap();
                s.active_question
                    .as_ref()
                    .is_some_and(|aq| !aq.done && !aq.question.choices.is_empty())
            };
            if should_enter {
                app.input_mode = InputMode::QuestionSelect;
                app.question_cursor = 0;
                app.question_scroll = 0;
                app.status_message = None;
            }
        }

        // Check for question timeout while in question-select mode.
        if matches!(
            app.input_mode,
            InputMode::QuestionSelect | InputMode::QuestionEdit
        ) {
            let timed_out = {
                let s = state.lock().unwrap();
                s.active_question.as_ref().is_some_and(|aq| {
                    aq.deadline
                        .is_some_and(|deadline| Instant::now() >= deadline)
                })
            };
            if timed_out {
                {
                    let mut s = state.lock().unwrap();
                    if let Some(ref mut aq) = s.active_question {
                        aq.response = None; // will default to TimedOut in poll_question
                        aq.done = true;
                    }
                }
                app.input_mode = InputMode::Normal;
                app.input_buffer.clear();
                app.status_message = Some("Selection timed out.".into());
            }
        }

        // Flush pending log lines from the tracing layer into UiState
        // before rendering.  This acquires the UiState lock briefly and
        // only when there are new lines, keeping the render path fast.
        if let Some(ref log_buf) = config.log_buffer {
            log_buf.flush_into(&state);
        }

        // Render.
        terminal.draw(|frame| {
            render(frame, &state, &app, config.extension_renderer.as_ref());
        })?;

        // Poll for input events (100ms timeout for responsive rendering).
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            handle_key_event(key, &mut app, &state);
        }

        // In --once mode, auto-show exit message after agent finishes.
        if !running && matches!(app.input_mode, InputMode::Normal) && app.status_message.is_none() {
            let has_pending_question = {
                let s = state.lock().unwrap();
                s.active_question.as_ref().is_some_and(|aq| !aq.done)
            };
            if !has_pending_question {
                app.status_message = Some("Agent finished. Press [q] to quit.".into());
            }
        }
    }

    // Restore terminal.
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, cursor::Show)?;
    terminal.show_cursor()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tui_config_default() {
        let config = TuiConfig::default();
        assert_eq!(config.workdir, PathBuf::from("."));
    }

    #[test]
    fn app_defaults() {
        let app = App::new();
        assert!(!app.should_quit);
        assert!(app.status_message.is_none());
        assert_eq!(app.log_scroll, 0);
        assert_eq!(app.agent_scroll, 0);
        assert_eq!(app.question_cursor, 0);
        assert_eq!(app.question_scroll, 0);
    }
}
