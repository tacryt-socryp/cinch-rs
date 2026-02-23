//! Serializable projection of [`UiState`] for WebSocket and REST transport.
//!
//! [`UiState`] contains non-serializable types (`Instant`, `Box<dyn UiExtension>`)
//! and potentially large collections. [`UiStateSnapshot`] converts these into
//! wire-friendly representations: `Instant` → seconds remaining, extension → JSON,
//! logs capped to the most recent entries.

use std::time::Instant;

use cinch_rs::ui::{AgentEntry, LogLine, UiState, UserQuestion};
use serde::Serialize;

/// Maximum number of log lines included in a snapshot.
const SNAPSHOT_MAX_LOGS: usize = 200;

/// Serializable view of [`UiState`] sent over WebSocket or REST.
///
/// Converts `Instant` fields to "seconds remaining" and domain extension
/// state to JSON via [`UiExtension::to_json()`].
#[derive(Debug, Serialize)]
pub struct UiStateSnapshot {
    // ── Agent progress ──
    pub phase: String,
    pub round: u32,
    pub max_rounds: u32,
    pub context_pct: f64,
    pub model: String,
    pub cycle: u32,

    // ── Agent output ──
    pub agent_output: Vec<AgentEntry>,
    pub streaming_buffer: String,

    // ── Logs (capped) ──
    pub logs: Vec<LogLine>,

    // ── Lifecycle ──
    pub running: bool,

    // ── Scheduling ──
    /// Seconds until the next cycle starts, or `null` if not scheduled.
    pub next_cycle_secs: Option<f64>,

    // ── Active question ──
    pub active_question: Option<ActiveQuestionSnapshot>,

    // ── Domain extension ──
    pub extension: Option<serde_json::Value>,
}

/// Serializable view of an in-flight question.
#[derive(Debug, Serialize)]
pub struct ActiveQuestionSnapshot {
    pub question: UserQuestion,
    /// Seconds remaining before timeout, or `null` if no deadline.
    pub remaining_secs: Option<f64>,
    pub done: bool,
}

impl UiStateSnapshot {
    /// Build a snapshot from the current `UiState`.
    ///
    /// This reads all fields and converts non-serializable types. Should be
    /// called while holding the `UiState` lock.
    pub fn from_ui_state(state: &UiState) -> Self {
        let now = Instant::now();

        let next_cycle_secs = state.next_cycle_at.map(|t| {
            if t > now {
                t.duration_since(now).as_secs_f64()
            } else {
                0.0
            }
        });

        let active_question = state.active_question.as_ref().map(|aq| {
            let remaining_secs = aq.deadline.map(|d| {
                if d > now {
                    d.duration_since(now).as_secs_f64()
                } else {
                    0.0
                }
            });

            ActiveQuestionSnapshot {
                question: aq.question.clone(),
                remaining_secs,
                done: aq.done,
            }
        });

        // Take only the most recent logs to limit payload size.
        let log_start = state.logs.len().saturating_sub(SNAPSHOT_MAX_LOGS);
        let logs: Vec<LogLine> = state.logs[log_start..].to_vec();

        Self {
            phase: state.phase.clone(),
            round: state.round,
            max_rounds: state.max_rounds,
            context_pct: state.context_pct,
            model: state.model.clone(),
            cycle: state.cycle,
            agent_output: state.agent_output.clone(),
            streaming_buffer: state.streaming_buffer.clone(),
            logs,
            running: state.running,
            next_cycle_secs,
            active_question,
            extension: state.extensions.to_json(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_from_default_state() {
        let state = UiState::default();
        let snap = UiStateSnapshot::from_ui_state(&state);

        assert_eq!(snap.phase, "Initializing");
        assert_eq!(snap.round, 0);
        assert_eq!(snap.max_rounds, 0);
        assert!(snap.running);
        assert!(snap.agent_output.is_empty());
        assert!(snap.logs.is_empty());
        assert!(snap.active_question.is_none());
        assert!(snap.next_cycle_secs.is_none());
        assert!(snap.extension.is_none());
    }

    #[test]
    fn snapshot_serializes_to_json() {
        let state = UiState::default();
        let snap = UiStateSnapshot::from_ui_state(&state);

        let json = serde_json::to_value(&snap).unwrap();
        assert_eq!(json["phase"], "Initializing");
        assert_eq!(json["running"], true);
        assert!(json["next_cycle_secs"].is_null());
    }

    #[test]
    fn snapshot_caps_logs() {
        let mut state = UiState::default();
        for i in 0..300 {
            state.logs.push(LogLine {
                time: format!("{i:03}"),
                level: cinch_rs::ui::LogLevel::Info,
                message: format!("msg {i}"),
            });
        }

        let snap = UiStateSnapshot::from_ui_state(&state);
        assert_eq!(snap.logs.len(), 200);
        // Should contain the *last* 200 entries.
        assert_eq!(snap.logs[0].time, "100");
        assert_eq!(snap.logs[199].time, "299");
    }
}
