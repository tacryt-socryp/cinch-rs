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
    pub total_prompt_tokens: u64,
    /// Total completion tokens consumed.
    pub total_completion_tokens: u64,
    /// Estimated cost so far.
    pub estimated_cost_usd: f64,
    /// Timestamp of the checkpoint.
    pub timestamp: String,
}
