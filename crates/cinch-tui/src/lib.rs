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
        // Drain pending log lines from the tracing buffer *before*
        // acquiring the UiState lock.  `drain()` only touches the
        // LogBuffer's internal mutex, so it never contends with the
        // agent runtime.
        let pending_logs = config
            .log_buffer
            .as_ref()
            .map(|buf| buf.drain())
            .unwrap_or_default();

        // ── Single lock acquisition ──────────────────────────────
        // Read everything the frame needs and apply side-effects
        // (log flush, timeout) in one shot to minimize contention
        // with the agent's async runtime.
        enum QuestionAction {
            None,
            EnterSelect,
            EnterFreeText,
            TimedOut,
        }

        let (running, quit, question_action) = {
            let mut s = state.lock().unwrap();

            // Merge drained log lines.
            if !pending_logs.is_empty() {
                s.logs.extend(pending_logs);
                // Respect the same trim limits as LogBuffer::flush_into.
                if s.logs.len() > cinch_rs::ui::MAX_LOG_LINES {
                    let trim_to = s.logs.len() - cinch_rs::ui::LOG_TRIM_TO;
                    s.logs.drain(..trim_to);
                }
            }

            // Determine question action.
            let qa = match app.input_mode {
                InputMode::Normal => match s.active_question.as_ref() {
                    Some(aq) if !aq.done && !aq.question.choices.is_empty() => {
                        QuestionAction::EnterSelect
                    }
                    Some(aq) if !aq.done && aq.question.choices.is_empty() => {
                        QuestionAction::EnterFreeText
                    }
                    _ => QuestionAction::None,
                },
                InputMode::QuestionSelect | InputMode::QuestionEdit | InputMode::FreeText => {
                    let timed_out = s.active_question.as_ref().is_some_and(|aq| {
                        aq.deadline
                            .is_some_and(|deadline| Instant::now() >= deadline)
                    });
                    if timed_out {
                        if let Some(ref mut aq) = s.active_question {
                            aq.response = None; // will default to TimedOut in poll_question
                            aq.done = true;
                        }
                        QuestionAction::TimedOut
                    } else {
                        QuestionAction::None
                    }
                }
            };

            (s.running, s.quit_requested, qa)
            // lock released here
        };

        // ── Apply results (no lock held) ─────────────────────────

        if app.should_quit || quit {
            state.lock().unwrap().quit_requested = true;
            break;
        }

        match question_action {
            QuestionAction::EnterSelect => {
                app.input_mode = InputMode::QuestionSelect;
                app.question_cursor = 0;
                app.question_scroll = 0;
                app.status_message = None;
            }
            QuestionAction::EnterFreeText => {
                app.input_mode = InputMode::FreeText;
                app.input_buffer.clear();
                app.status_message = None;
            }
            QuestionAction::TimedOut => {
                app.input_mode = InputMode::Normal;
                app.input_buffer.clear();
                app.status_message = Some("Selection timed out.".into());
            }
            QuestionAction::None => {}
        }

        // Render (takes its own snapshot lock internally).
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
            // We already read `running` and question state above, so we
            // can skip the extra lock when the agent is still running
            // (the common case).
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
