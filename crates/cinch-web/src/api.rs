//! REST API endpoint handlers.
//!
//! These complement the WebSocket channel for cases where request/response
//! semantics are more appropriate (initial state load, question answers).

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use cinch_rs::ui::{QuestionResponse, UiState, push_user_message};
use serde::Deserialize;
use tokio::sync::{broadcast, mpsc};

use crate::broadcast::WsMessage;
use crate::snapshot::UiStateSnapshot;

/// Shared application state passed to all handlers via axum's `State` extractor.
#[derive(Clone)]
pub struct AppState {
    pub ui_state: Arc<Mutex<UiState>>,
    pub chat_tx: mpsc::Sender<String>,
    pub broadcast_tx: broadcast::Sender<WsMessage>,
}

/// GET /api/state — Full state snapshot.
///
/// Returns the current `UiState` as a JSON object. Used for initial page load
/// (before WebSocket connects) and as a fallback.
pub async fn get_state(State(app): State<AppState>) -> Json<serde_json::Value> {
    let snapshot = {
        let state = app.ui_state.lock().unwrap();
        UiStateSnapshot::from_ui_state(&state)
    };
    Json(serde_json::to_value(snapshot).unwrap_or_default())
}

/// Request body for POST /api/answer.
#[derive(Deserialize)]
pub struct AnswerRequest {
    pub response: QuestionResponse,
}

/// POST /api/answer — Submit a question response.
///
/// Sets the active question's response and marks it as done.
/// Returns 204 on success, 404 if no active question exists.
pub async fn post_answer(
    State(app): State<AppState>,
    Json(body): Json<AnswerRequest>,
) -> StatusCode {
    let mut state = app.ui_state.lock().unwrap();
    if let Some(ref mut aq) = state.active_question
        && !aq.done
    {
        aq.response = Some(body.response);
        aq.done = true;
        return StatusCode::NO_CONTENT;
    }
    StatusCode::NOT_FOUND
}

/// Request body for POST /api/control.
#[derive(Deserialize)]
pub struct ControlRequest {
    pub action: ControlAction,
}

/// Available control actions.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlAction {
    /// Request the agent to quit.
    Quit,
}

/// POST /api/control — Agent control commands.
///
/// Currently supports `quit` to request agent shutdown.
pub async fn post_control(
    State(app): State<AppState>,
    Json(body): Json<ControlRequest>,
) -> StatusCode {
    match body.action {
        ControlAction::Quit => {
            let mut state = app.ui_state.lock().unwrap();
            state.quit_requested = true;
            StatusCode::NO_CONTENT
        }
    }
}

/// Request body for POST /api/chat.
#[derive(Deserialize)]
pub struct ChatRequest {
    pub message: String,
}

/// POST /api/chat — Send a user chat message.
///
/// Pushes the message to the UI state, broadcasts it to all WebSocket clients,
/// and forwards it to the agent loop via an mpsc channel.
/// Returns 204 on success, 503 if the agent loop is not consuming messages.
pub async fn post_chat(State(app): State<AppState>, Json(body): Json<ChatRequest>) -> StatusCode {
    // Push user message to UI state for snapshot persistence.
    push_user_message(&app.ui_state, &body.message);
    // Broadcast to all connected WebSocket clients.
    let _ = app.broadcast_tx.send(WsMessage::UserMessage {
        message: body.message.clone(),
    });
    // Forward to the agent loop.
    match app.chat_tx.try_send(body.message) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answer_request_deserializes() {
        let json = r#"{"response":{"Selected":1}}"#;
        let req: AnswerRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.response, QuestionResponse::Selected(1));
    }

    #[test]
    fn control_request_deserializes() {
        let json = r#"{"action":"quit"}"#;
        let req: ControlRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(req.action, ControlAction::Quit));
    }
}
