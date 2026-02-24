//! Generic UI state and types for harness-powered agents.
//!
//! This module provides the shared data model that any UI frontend (TUI, web,
//! headless) uses to display agent progress. It contains no rendering
//! dependencies — just plain data types, traits, and convenience updaters.
//!
//! # Architecture
//!
//! ```text
//! Agent runtime ──writes──▶ Arc<Mutex<UiState>> ◀──reads── UI frontend
//! ```
//!
//! The agent (via [`event_handler::UiEventHandler`] or direct calls) writes
//! status updates into [`UiState`] . A UI frontend reads from the same state
//! to render.
//! Domain-specific data lives in the [`UiExtension`] slot.

pub mod ask_user_tool;
pub mod event_handler;
mod question;
pub mod tracing;
mod traits;

pub use question::{
    ActiveQuestion, QuestionChoice, QuestionResponse, UserQuestion, ask_question, poll_question,
};
pub use traits::{NoExtension, UiExtension};

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Maximum log lines kept in memory.
pub const MAX_LOG_LINES: usize = 2000;
/// Trim to this many when the cap is exceeded.
pub const LOG_TRIM_TO: usize = 1200;

/// Maximum agent output entries kept in memory.
///
/// The render function iterates over **all** entries every frame (including
/// word-wrapping) while holding the shared `UiState` mutex.  Unbounded
/// growth makes each render progressively slower, starving the TUI event
/// loop and eventually freezing the interface.
pub const MAX_AGENT_OUTPUT: usize = 500;
/// Trim to this many when the cap is exceeded.
pub const AGENT_OUTPUT_TRIM_TO: usize = 300;

// ── Agent Output Entries ──────────────────────────────────────────────

/// A single entry in the agent output stream.
///
/// Entries are rendered in order, interleaving LLM text with tool call /
/// result lines, and user messages from the chat UI.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AgentEntry {
    /// Free-form text emitted by the LLM.
    Text(String),
    /// A tool is about to execute.
    ToolExecuting { name: String, arguments: String },
    /// A tool finished executing.
    ToolResult {
        name: String,
        result: String,
        is_error: bool,
    },
    /// A message sent by the user via the chat UI.
    UserMessage(String),
    /// A consolidated, in-place-updated todo checklist.
    ///
    /// When the `todo` tool is called multiple times, only the most recent
    /// state is kept in the output stream so the user sees a single,
    /// always-current checklist rather than one entry per mutation.
    TodoUpdate(String),
}

// ── Log Types ─────────────────────────────────────────────────────────

/// A single log line captured from tracing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogLine {
    pub time: String,
    pub level: LogLevel,
    pub message: String,
}

/// Log severity level (mirrors tracing levels).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    /// Short fixed-width label for display.
    pub fn label(self) -> &'static str {
        match self {
            Self::Trace => "TRACE",
            Self::Debug => "DEBUG",
            Self::Info => "INFO ",
            Self::Warn => "WARN ",
            Self::Error => "ERROR",
        }
    }
}

// ── UiState ───────────────────────────────────────────────────────────

/// Core UI state shared between the agent runtime and a frontend.
///
/// Protected by a `Mutex`. The agent writes status updates; the frontend
/// reads them for rendering.
pub struct UiState {
    // ── Agent progress ──
    pub phase: String,
    pub round: u32,
    pub max_rounds: u32,
    pub context_pct: f64,
    pub model: String,
    pub cycle: u32,

    // ── Agent output stream ──
    pub agent_output: Vec<AgentEntry>,
    /// Buffer for accumulating streaming text deltas. Rendered live by the
    /// frontend; cleared when the complete `Text` event arrives.
    pub streaming_buffer: String,

    // ── Tracing log capture ──
    pub logs: Vec<LogLine>,

    // ── Lifecycle ──
    /// Set to `false` when the agent finishes (e.g. `--once` mode).
    pub running: bool,
    /// The frontend sets this to `true` when the user requests quit.
    pub quit_requested: bool,

    // ── Active question (human-in-the-loop) ──
    pub active_question: Option<ActiveQuestion>,

    // ── Scheduling ──
    /// When the next agent cycle is scheduled to start.
    pub next_cycle_at: Option<Instant>,

    // ── Domain-specific extension slot ──
    pub extensions: Box<dyn UiExtension>,
}

impl UiState {
    /// Create a `UiState` with a domain-specific extension.
    pub fn with_extension(ext: impl UiExtension + 'static) -> Self {
        Self {
            extensions: Box::new(ext),
            ..Default::default()
        }
    }
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            phase: "Initializing".into(),
            round: 0,
            max_rounds: 0,
            context_pct: 0.0,
            model: String::new(),
            cycle: 0,
            agent_output: Vec::new(),
            streaming_buffer: String::new(),
            logs: Vec::new(),
            running: true,
            quit_requested: false,
            active_question: None,
            next_cycle_at: None,
            extensions: Box::new(NoExtension),
        }
    }
}

// ── Convenience Updaters ──────────────────────────────────────────────

/// Lock the shared state mutex and run a closure on the guard.
/// Silently ignores poisoned locks (no log spam inside UI frontends).
macro_rules! with_state {
    ($state:expr, |$s:ident| $body:block) => {
        if let Ok(mut $s) = $state.lock() {
            $body
        }
    };
}

/// Update the current phase string.
pub fn update_phase(state: &Arc<Mutex<UiState>>, phase: &str) {
    with_state!(state, |s| { s.phase = phase.to_string() });
}

/// Update the current round and context percentage.
pub fn update_round(state: &Arc<Mutex<UiState>>, round: u32, max: u32, ctx_pct: f64) {
    with_state!(state, |s| {
        s.round = round;
        s.max_rounds = max;
        s.context_pct = ctx_pct;
    });
}

/// Trim `agent_output` when it exceeds `MAX_AGENT_OUTPUT`, keeping the most
/// recent entries.
fn trim_agent_output(s: &mut UiState) {
    if s.agent_output.len() > MAX_AGENT_OUTPUT {
        let drain = s.agent_output.len() - AGENT_OUTPUT_TRIM_TO;
        s.agent_output.drain(..drain);
    }
}

/// Push a complete agent text block.
///
/// If there is an in-progress streaming buffer it is discarded because the
/// complete `Text` event carries the same content.
pub fn push_agent_text(state: &Arc<Mutex<UiState>>, text: &str) {
    with_state!(state, |s| {
        s.streaming_buffer.clear();
        s.agent_output.push(AgentEntry::Text(text.to_string()));
        trim_agent_output(&mut s);
    });
}

/// Append a streaming text delta to the in-progress buffer.
///
/// The frontend renders this buffer live so the user sees tokens as they
/// arrive. When the full `Text` event fires, [`push_agent_text`] clears
/// the buffer.
pub fn push_agent_text_delta(state: &Arc<Mutex<UiState>>, delta: &str) {
    with_state!(state, |s| { s.streaming_buffer.push_str(delta) });
}

/// Record that a tool is about to execute.
pub fn push_tool_executing(state: &Arc<Mutex<UiState>>, name: &str, arguments: &str) {
    with_state!(state, |s| {
        s.agent_output.push(AgentEntry::ToolExecuting {
            name: name.to_string(),
            arguments: arguments.to_string(),
        });
        trim_agent_output(&mut s);
    });
}

/// Record a tool result. Auto-detects errors by checking if the result
/// starts with "Error" or "error:".
pub fn push_tool_result(state: &Arc<Mutex<UiState>>, name: &str, result: &str) {
    let is_error = result.starts_with("Error") || result.starts_with("error:");
    with_state!(state, |s| {
        s.agent_output.push(AgentEntry::ToolResult {
            name: name.to_string(),
            result: result.to_string(),
            is_error,
        });
        trim_agent_output(&mut s);
    });
}

/// Record or update the consolidated todo list.
///
/// If a [`AgentEntry::TodoUpdate`] entry already exists in `agent_output` it
/// is replaced in-place so the user sees only the current checklist state.
/// Otherwise a new entry is appended.
pub fn push_todo_update(state: &Arc<Mutex<UiState>>, content: &str) {
    with_state!(state, |s| {
        if let Some(existing) = s
            .agent_output
            .iter_mut()
            .rev()
            .find(|e| matches!(e, AgentEntry::TodoUpdate(_)))
        {
            *existing = AgentEntry::TodoUpdate(content.to_string());
        } else {
            s.agent_output
                .push(AgentEntry::TodoUpdate(content.to_string()));
            trim_agent_output(&mut s);
        }
    });
}

/// Record a user message sent from the chat UI.
pub fn push_user_message(state: &Arc<Mutex<UiState>>, message: &str) {
    with_state!(state, |s| {
        s.agent_output
            .push(AgentEntry::UserMessage(message.to_string()));
        trim_agent_output(&mut s);
    });
}

/// Set the next-cycle countdown.
pub fn set_next_cycle(state: &Arc<Mutex<UiState>>, duration: Duration) {
    with_state!(state, |s| {
        s.next_cycle_at = Some(Instant::now() + duration);
    });
}

/// Clear the next-cycle countdown (cycle is starting).
pub fn clear_next_cycle(state: &Arc<Mutex<UiState>>) {
    with_state!(state, |s| { s.next_cycle_at = None });
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_level_labels() {
        assert_eq!(LogLevel::Info.label(), "INFO ");
        assert_eq!(LogLevel::Error.label(), "ERROR");
        assert_eq!(LogLevel::Debug.label(), "DEBUG");
        assert_eq!(LogLevel::Trace.label(), "TRACE");
        assert_eq!(LogLevel::Warn.label(), "WARN ");
    }

    #[test]
    fn ui_state_defaults() {
        let state = UiState::default();
        assert!(state.running);
        assert!(!state.quit_requested);
        assert_eq!(state.phase, "Initializing");
        assert!(state.logs.is_empty());
        assert!(state.agent_output.is_empty());
        assert!(state.streaming_buffer.is_empty());
        assert!(state.active_question.is_none());
        assert!(state.next_cycle_at.is_none());
        assert_eq!(state.round, 0);
        assert_eq!(state.max_rounds, 0);
        assert_eq!(state.cycle, 0);
    }

    #[test]
    fn ui_state_with_extension() {
        struct TestExt {
            count: u32,
        }
        impl UiExtension for TestExt {
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }

        let state = UiState::with_extension(TestExt { count: 42 });
        let ext = state.extensions.as_any().downcast_ref::<TestExt>().unwrap();
        assert_eq!(ext.count, 42);
    }

    #[test]
    fn update_helpers_work() {
        let state = Arc::new(Mutex::new(UiState::default()));

        update_phase(&state, "Pre-computing context");
        assert_eq!(state.lock().unwrap().phase, "Pre-computing context");

        update_round(&state, 3, 20, 0.42);
        {
            let s = state.lock().unwrap();
            assert_eq!(s.round, 3);
            assert_eq!(s.max_rounds, 20);
            assert!((s.context_pct - 0.42).abs() < 0.001);
        }

        push_agent_text(&state, "Hello from agent");
        assert_eq!(state.lock().unwrap().agent_output.len(), 1);
    }

    #[test]
    fn streaming_deltas_accumulate_in_buffer() {
        let state = Arc::new(Mutex::new(UiState::default()));

        push_agent_text_delta(&state, "Hello");
        push_agent_text_delta(&state, " world");
        {
            let s = state.lock().unwrap();
            assert_eq!(s.streaming_buffer, "Hello world");
            assert!(
                s.agent_output.is_empty(),
                "deltas should not touch agent_output"
            );
        }

        // Complete text event clears the buffer and pushes to agent_output.
        push_agent_text(&state, "Hello world");
        {
            let s = state.lock().unwrap();
            assert!(s.streaming_buffer.is_empty(), "buffer should be cleared");
            assert_eq!(s.agent_output.len(), 1);
            assert_eq!(
                s.agent_output[0],
                AgentEntry::Text("Hello world".to_string())
            );
        }
    }

    #[test]
    fn tool_executing_and_result_pushed() {
        let state = Arc::new(Mutex::new(UiState::default()));

        push_tool_executing(&state, "read_file", r#"{"path":"docs/voice.md"}"#);
        push_tool_result(&state, "read_file", "file contents here");
        push_tool_executing(&state, "shell", r#"{"command":"echo hi"}"#);
        push_tool_result(&state, "shell", "Error: command not allowed");

        let s = state.lock().unwrap();
        assert_eq!(s.agent_output.len(), 4);
        assert_eq!(
            s.agent_output[0],
            AgentEntry::ToolExecuting {
                name: "read_file".into(),
                arguments: r#"{"path":"docs/voice.md"}"#.into(),
            }
        );
        assert_eq!(
            s.agent_output[1],
            AgentEntry::ToolResult {
                name: "read_file".into(),
                result: "file contents here".into(),
                is_error: false,
            }
        );
        if let AgentEntry::ToolResult { is_error, .. } = &s.agent_output[3] {
            assert!(is_error);
        } else {
            panic!("expected ToolResult");
        }
    }

    #[test]
    fn next_cycle_set_and_clear() {
        let state = Arc::new(Mutex::new(UiState::default()));

        set_next_cycle(&state, Duration::from_secs(3600));
        assert!(state.lock().unwrap().next_cycle_at.is_some());

        clear_next_cycle(&state);
        assert!(state.lock().unwrap().next_cycle_at.is_none());
    }

    #[test]
    fn ask_question_and_poll() {
        let state = Arc::new(Mutex::new(UiState::default()));

        let question = UserQuestion {
            prompt: "Pick one:".into(),
            choices: vec![
                QuestionChoice {
                    label: "Option A".into(),
                    body: "First option".into(),
                    metadata: String::new(),
                },
                QuestionChoice {
                    label: "Option B".into(),
                    body: "Second option".into(),
                    metadata: String::new(),
                },
            ],
            editable: false,
            max_edit_length: None,
        };

        ask_question(&state, question, 60);

        // Not done yet — poll returns None.
        assert!(poll_question(&state).is_none());

        // Simulate user selecting option B.
        {
            let mut s = state.lock().unwrap();
            let aq = s.active_question.as_mut().unwrap();
            aq.response = Some(QuestionResponse::Selected(1));
            aq.done = true;
        }

        // Now poll returns the response.
        let response = poll_question(&state).unwrap();
        assert_eq!(response, QuestionResponse::Selected(1));

        // Question is cleared after poll.
        assert!(state.lock().unwrap().active_question.is_none());
    }

    #[test]
    fn poll_question_returns_none_when_no_question() {
        let state = Arc::new(Mutex::new(UiState::default()));
        assert!(poll_question(&state).is_none());
    }

    #[test]
    fn poll_question_defaults_to_timed_out() {
        let state = Arc::new(Mutex::new(UiState::default()));

        let question = UserQuestion {
            prompt: "Pick:".into(),
            choices: vec![QuestionChoice {
                label: "A".into(),
                body: "a".into(),
                metadata: String::new(),
            }],
            editable: false,
            max_edit_length: None,
        };
        ask_question(&state, question, 0);

        // Mark done without setting a response.
        {
            let mut s = state.lock().unwrap();
            s.active_question.as_mut().unwrap().done = true;
        }

        let response = poll_question(&state).unwrap();
        assert_eq!(response, QuestionResponse::TimedOut);
    }
}
