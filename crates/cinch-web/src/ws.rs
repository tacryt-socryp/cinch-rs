//! WebSocket upgrade handler and message dispatch.
//!
//! Each connected client receives:
//! 1. A full [`UiStateSnapshot`] on connect.
//! 2. Incremental [`WsMessage`] updates as harness events fire.
//!
//! Clients can send JSON messages back (question answers, quit requests).

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use cinch_rs::ui::{QuestionResponse, UiState, push_user_message};
use futures::{SinkExt, StreamExt, stream::SplitSink};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::broadcast::WsMessage;
use crate::snapshot::UiStateSnapshot;

/// Shared state for WebSocket handlers.
#[derive(Clone)]
pub struct WsState {
    pub ui_state: Arc<Mutex<UiState>>,
    pub broadcast_tx: broadcast::Sender<WsMessage>,
    pub chat_tx: tokio::sync::mpsc::Sender<String>,
}

/// GET /ws — WebSocket upgrade handler.
pub async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(ws_state): State<WsState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, ws_state))
}

/// Handle a single WebSocket connection.
async fn handle_socket(socket: WebSocket, ws_state: WsState) {
    let (mut sink, mut stream) = socket.split();

    // Send initial snapshot.
    let snapshot = {
        let state = ws_state.ui_state.lock().unwrap();
        UiStateSnapshot::from_ui_state(&state)
    };
    let snapshot_msg = WsMessage::Snapshot {
        data: serde_json::to_value(snapshot).unwrap_or_default(),
    };
    if ws_send(&mut sink, &snapshot_msg).await.is_err() {
        return;
    }

    // Also send active question if one exists.
    let pending_question = {
        let state = ws_state.ui_state.lock().unwrap();
        state
            .active_question
            .as_ref()
            .filter(|aq| !aq.done)
            .map(|aq| aq.question.clone())
    };
    if let Some(question) = pending_question {
        let _ = ws_send(&mut sink, &WsMessage::Question { question }).await;
    }

    debug!("WebSocket client connected");

    // Subscribe to broadcast channel for server→client messages.
    let mut broadcast_rx = ws_state.broadcast_tx.subscribe();

    // Spawn a task that forwards broadcast messages to this client.
    let ui_state_for_resync = ws_state.ui_state.clone();
    let forward_task = tokio::spawn(async move {
        loop {
            match broadcast_rx.recv().await {
                Ok(msg) => {
                    if ws_send(&mut sink, &msg).await.is_err() {
                        break; // Client disconnected.
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // Client fell behind — send a fresh snapshot to resynchronize.
                    warn!("WebSocket client lagged by {n} messages, resending snapshot");
                    let snapshot = {
                        let state = ui_state_for_resync.lock().unwrap();
                        UiStateSnapshot::from_ui_state(&state)
                    };
                    let msg = WsMessage::Snapshot {
                        data: serde_json::to_value(snapshot).unwrap_or_default(),
                    };
                    if ws_send(&mut sink, &msg).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Handle incoming messages from this client.
    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Text(text) => {
                handle_client_message(
                    &text,
                    &ws_state.ui_state,
                    &ws_state.chat_tx,
                    &ws_state.broadcast_tx,
                );
            }
            Message::Close(_) => break,
            _ => {} // Ignore binary, ping, pong.
        }
    }

    debug!("WebSocket client disconnected");
    forward_task.abort();
}

/// Process a JSON message received from a client.
fn handle_client_message(
    text: &str,
    ui_state: &Arc<Mutex<UiState>>,
    chat_tx: &tokio::sync::mpsc::Sender<String>,
    broadcast_tx: &broadcast::Sender<WsMessage>,
) {
    #[derive(serde::Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum ClientMessage {
        Answer { response: QuestionResponse },
        Chat { message: String },
        Quit,
    }

    let Ok(msg) = serde_json::from_str::<ClientMessage>(text) else {
        debug!("Ignoring malformed WebSocket message");
        return;
    };

    match msg {
        ClientMessage::Answer { response } => {
            let mut state = ui_state.lock().unwrap();
            if let Some(ref mut aq) = state.active_question
                && !aq.done
            {
                aq.response = Some(response);
                aq.done = true;
            }
        }
        ClientMessage::Chat { message } => {
            // Push user message to UI state so it appears in the chat stream
            // and persists across reconnects (via snapshot).
            push_user_message(ui_state, &message);
            // Broadcast to all connected clients.
            let _ = broadcast_tx.send(WsMessage::UserMessage {
                message: message.clone(),
            });
            // Forward to the agent loop.
            let _ = chat_tx.try_send(message);
        }
        ClientMessage::Quit => {
            let mut state = ui_state.lock().unwrap();
            state.quit_requested = true;
        }
    }
}

/// Serialize a `WsMessage` and send it over the WebSocket sink.
async fn ws_send(sink: &mut SplitSink<WebSocket, Message>, msg: &WsMessage) -> Result<(), ()> {
    let json = serde_json::to_string(msg).unwrap_or_default();
    sink.send(Message::Text(json.into())).await.map_err(|_| ())
}
