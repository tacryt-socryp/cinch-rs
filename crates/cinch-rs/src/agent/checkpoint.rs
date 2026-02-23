//! Checkpoint and resume for long-running agent loops.
//!
//! Serializes harness state to disk after each round, enabling recovery
//! from crashes or interruptions. On resume, loads the checkpoint and
//! continues from the last completed round.
//!
//! Persistence is handled by [`super::session::SessionManager`]. This module
//! defines the serializable [`Checkpoint`] struct and the unrelated
//! [`AdaptiveRoundLimit`].

use crate::Message;
use serde::{Deserialize, Serialize};

/// Serializable checkpoint of harness state.
#[derive(Serialize, Deserialize, Debug)]
pub struct Checkpoint {
    /// Trace ID for the run.
    pub trace_id: String,
    /// All messages up to the checkpoint.
    pub messages: Vec<Message>,
    /// Text output accumulated so far.
    pub text_output: Vec<String>,
    /// Current round number.
    pub round: u32,
    /// Total prompt tokens consumed.
    pub total_prompt_tokens: u32,
    /// Total completion tokens consumed.
    pub total_completion_tokens: u32,
    /// Estimated cost so far.
    pub estimated_cost_usd: f64,
    /// Timestamp of the checkpoint.
    pub timestamp: String,
}

/// Adaptive round limits: dynamically adjust max_rounds based on progress.
#[derive(Debug, Clone)]
pub struct AdaptiveRoundLimit {
    /// Initial maximum rounds.
    pub initial_max: u32,
    /// Current maximum (may be adjusted).
    pub current_max: u32,
    /// Maximum absolute limit (never exceed this).
    pub absolute_max: u32,
    /// Minimum rounds of progress before considering extension.
    pub min_progress_rounds: u32,
}

impl AdaptiveRoundLimit {
    pub fn new(initial_max: u32, absolute_max: u32) -> Self {
        Self {
            initial_max,
            current_max: initial_max,
            absolute_max,
            min_progress_rounds: 3,
        }
    }

    /// Request more rounds. Returns the new limit if approved.
    /// Requires evidence of progress (tool calls, drafts saved, etc.).
    pub fn request_extension(&mut self, rounds_used: u32, has_progress: bool) -> Option<u32> {
        if !has_progress || rounds_used < self.min_progress_rounds {
            return None;
        }

        // Grant 50% more rounds, up to absolute max.
        let extension = (self.current_max as f64 * 0.5).ceil() as u32;
        let new_max = (self.current_max + extension).min(self.absolute_max);

        if new_max > self.current_max {
            self.current_max = new_max;
            Some(new_max)
        } else {
            None
        }
    }

    /// Check if the current round is within limits.
    pub fn is_within_limit(&self, round: u32) -> bool {
        round < self.current_max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_round_limit() {
        let mut limit = AdaptiveRoundLimit::new(10, 30);
        assert!(limit.is_within_limit(5));
        assert!(limit.is_within_limit(9));
        assert!(!limit.is_within_limit(10));

        // Extend with progress.
        let new = limit.request_extension(5, true);
        assert!(new.is_some());
        assert_eq!(limit.current_max, 15);

        // Can extend again.
        let new2 = limit.request_extension(12, true);
        assert!(new2.is_some());

        // No extension without progress.
        let no_ext = limit.request_extension(20, false);
        assert!(no_ext.is_none());
    }

    #[test]
    fn adaptive_limit_respects_absolute_max() {
        let mut limit = AdaptiveRoundLimit::new(25, 30);
        limit.request_extension(20, true);
        // Should be capped at 30.
        assert!(limit.current_max <= 30);
    }
}
