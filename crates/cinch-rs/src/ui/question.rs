//! Generic "ask the user a question" data model.
//!
//! Replaces domain-specific selection flows (e.g. tweet selection) with a
//! generic question/choice/response pattern that works across any UI frontend.

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::UiState;

/// A question presented to the user during an agent run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserQuestion {
    /// Short prompt displayed as a header (e.g., "Which tweet should we post?").
    pub prompt: String,
    /// Available choices.
    pub choices: Vec<QuestionChoice>,
    /// Whether the user can edit the selected choice's text before confirming.
    pub editable: bool,
    /// Optional validation for edited text (e.g., max character length).
    pub max_edit_length: Option<usize>,
}

/// A single selectable choice within a [`UserQuestion`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuestionChoice {
    /// Short label (e.g., "Tweet 1 — Technical Explainer").
    pub label: String,
    /// Full body text displayed when this choice is focused.
    pub body: String,
    /// Optional metadata displayed alongside the choice (e.g., "142 chars").
    pub metadata: String,
}

/// The user's response to a [`UserQuestion`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuestionResponse {
    /// User selected a choice (by index).
    Selected(usize),
    /// User selected a choice and edited its body text.
    SelectedEdited { index: usize, edited_text: String },
    /// User explicitly skipped / dismissed the question.
    Skipped,
    /// The question timed out with no user interaction.
    TimedOut,
}

/// Tracks an in-flight question inside [`UiState`].
#[derive(Clone)]
pub struct ActiveQuestion {
    /// The question being asked.
    pub question: UserQuestion,
    /// When the question expires (if any).
    pub deadline: Option<Instant>,
    /// The user's response, set by the UI frontend.
    pub response: Option<QuestionResponse>,
    /// Set to `true` once the question is fully resolved.
    pub done: bool,
}

/// Present a question to the user. Replaces any previous active question.
///
/// The UI frontend reads `UiState.active_question` and renders the choices.
/// When the user responds (or the deadline passes), the frontend sets
/// `active_question.response` and `active_question.done = true`.
pub fn ask_question(state: &Arc<Mutex<UiState>>, question: UserQuestion, timeout_secs: u64) {
    if let Ok(mut s) = state.lock() {
        s.active_question = Some(ActiveQuestion {
            question,
            deadline: Some(Instant::now() + Duration::from_secs(timeout_secs)),
            response: None,
            done: false,
        });
        s.phase = "Waiting for user response".to_string();
    }
}

/// Poll for the user's response. Returns `None` if still waiting.
///
/// When the question is done, returns the response and clears the active
/// question from state.
pub fn poll_question(state: &Arc<Mutex<UiState>>) -> Option<QuestionResponse> {
    if let Ok(mut s) = state.lock()
        && let Some(ref aq) = s.active_question
        && aq.done
    {
        let response = aq.response.clone().unwrap_or(QuestionResponse::TimedOut);
        s.active_question = None;
        return Some(response);
    }
    None
}
