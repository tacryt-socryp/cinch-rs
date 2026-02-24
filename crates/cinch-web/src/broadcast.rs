//! [`EventHandler`] that converts harness events into WebSocket messages.
//!
//! [`WebBroadcastHandler`] intercepts [`HarnessEvent`] variants and serializes
//! them into [`WsMessage`] values, broadcasting to all connected WebSocket
//! clients via a `tokio::sync::broadcast` channel.

use std::sync::{Arc, Mutex};

use cinch_rs::agent::events::{EventHandler, EventResponse, HarnessEvent};
use cinch_rs::ui::UiState;
use serde::Serialize;
use tokio::sync::broadcast;

use crate::ext::WebExtensionRenderer;

/// Maximum tool result size sent over WebSocket (8 KB).
/// Full results remain in `UiState` and can be fetched via `/api/state`.
const MAX_WS_TOOL_RESULT_BYTES: usize = 8 * 1024;

/// A message sent from the server to WebSocket clients.
///
/// Discriminated on the `type` field when serialized to JSON.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsMessage {
    /// Full state snapshot (sent on initial connect and after reconnect).
    Snapshot { data: serde_json::Value },
    /// Complete LLM text block.
    Text { text: String },
    /// Streaming token delta.
    TextDelta { delta: String },
    /// A tool is about to execute.
    ToolExecuting { name: String, arguments: String },
    /// A tool finished executing.
    ToolResult {
        name: String,
        result: String,
        is_error: bool,
    },
    /// LLM reasoning / extended thinking content.
    Reasoning { text: String },
    /// Streaming reasoning delta.
    ReasoningDelta { delta: String },
    /// Round progress update.
    Round {
        round: u32,
        max_rounds: u32,
        context_pct: f64,
    },
    /// Phase change.
    Phase { phase: String },
    /// A question has been presented to the user.
    Question {
        question: cinch_rs::ui::UserQuestion,
    },
    /// The active question has been resolved.
    QuestionDismissed,
    /// The agent finished (no more tool calls).
    Finished,
    /// A log line captured from tracing.
    Log { line: cinch_rs::ui::LogLine },
    /// Domain-specific extension state update.
    Extension { data: serde_json::Value },
    /// A user message sent from the chat UI.
    UserMessage { message: String },
    /// Token usage for the current round.
    TokenUsage {
        prompt_tokens: u32,
        completion_tokens: u32,
    },
    /// The agent received tool calls this round.
    ToolCallsReceived { round: u32, count: usize },
    /// A tool result was served from cache.
    ToolCacheHit { name: String, arguments: String },
    /// Context eviction freed memory.
    Eviction {
        freed_chars: usize,
        evicted_count: usize,
    },
    /// Context compaction completed.
    Compaction { compaction_number: usize },
    /// Model routing selected a different model.
    ModelRouted { model: String, round: u32 },
    /// Checkpoint saved after a round.
    CheckpointSaved { round: u32, path: String },
    /// Resumed from a checkpoint.
    CheckpointResumed { round: u32 },
    /// The API returned an empty response and is retrying.
    EmptyResponse {
        round: u32,
        attempt: u32,
        max_retries: u32,
    },
    /// A tool execution requires human approval.
    ApprovalRequired { name: String, arguments: String },
    /// Consolidated todo list update (replaces previous state in the client).
    TodoUpdate { content: String },
}

/// Event handler that broadcasts harness events to WebSocket clients.
///
/// Compose alongside [`UiEventHandler`](cinch_rs::ui::event_handler::UiEventHandler)
/// in a [`CompositeEventHandler`](cinch_rs::agent::CompositeEventHandler):
///
/// ```ignore
/// let handler = CompositeEventHandler::new()
///     .with(UiEventHandler::new(ui_state.clone()))
///     .with(WebBroadcastHandler::new(ws_sender, ext_renderer, ui_state.clone()));
/// ```
pub struct WebBroadcastHandler {
    sender: broadcast::Sender<WsMessage>,
    extension_renderer: Arc<dyn WebExtensionRenderer>,
    ui_state: Arc<Mutex<UiState>>,
}

impl WebBroadcastHandler {
    /// Create a new broadcast handler.
    pub fn new(
        sender: broadcast::Sender<WsMessage>,
        extension_renderer: Arc<dyn WebExtensionRenderer>,
        ui_state: Arc<Mutex<UiState>>,
    ) -> Self {
        Self {
            sender,
            extension_renderer,
            ui_state,
        }
    }

    /// Broadcast a message to all connected clients.
    ///
    /// Silently ignores send errors (no subscribers is fine).
    fn broadcast(&self, msg: WsMessage) {
        let _ = self.sender.send(msg);
    }

    /// Broadcast the extension state if the renderer provides it.
    fn broadcast_extension(&self) {
        if let Ok(s) = self.ui_state.lock()
            && let Some(data) = self.extension_renderer.to_ws_json(&*s.extensions)
        {
            self.broadcast(WsMessage::Extension { data });
        }
    }
}

impl EventHandler for WebBroadcastHandler {
    fn on_event(&self, event: &HarnessEvent<'_>) -> Option<EventResponse> {
        match event {
            HarnessEvent::RoundStart {
                round,
                max_rounds,
                context_usage,
                ..
            } => {
                self.broadcast(WsMessage::Round {
                    round: *round,
                    max_rounds: *max_rounds,
                    context_pct: context_usage.usage_pct,
                });
            }
            HarnessEvent::Text(text) => {
                self.broadcast(WsMessage::Text {
                    text: text.to_string(),
                });
            }
            HarnessEvent::TextDelta(delta) => {
                self.broadcast(WsMessage::TextDelta {
                    delta: delta.to_string(),
                });
            }
            HarnessEvent::ToolCallsReceived { round, count } => {
                self.broadcast(WsMessage::ToolCallsReceived {
                    round: *round,
                    count: *count,
                });
            }
            HarnessEvent::ToolExecuting {
                name, arguments, ..
            } => {
                // The todo tool updates in-place; skip ToolExecuting so the
                // client shows only the consolidated checklist.
                if *name != "todo" {
                    self.broadcast(WsMessage::Phase {
                        phase: format!("Tool: {name}"),
                    });
                    self.broadcast(WsMessage::ToolExecuting {
                        name: name.to_string(),
                        arguments: arguments.to_string(),
                    });
                }
            }
            HarnessEvent::ToolResult { name, result, .. } => {
                if *name == "todo" {
                    self.broadcast(WsMessage::TodoUpdate {
                        content: result.to_string(),
                    });
                } else {
                    let is_error = result.starts_with("Error") || result.starts_with("error:");
                    // Truncate large results for WebSocket transport.
                    let truncated = if result.len() > MAX_WS_TOOL_RESULT_BYTES {
                        let cut = &result[..MAX_WS_TOOL_RESULT_BYTES];
                        format!(
                            "{cut}\n... (truncated, {total} bytes total)",
                            total = result.len()
                        )
                    } else {
                        result.to_string()
                    };
                    self.broadcast(WsMessage::ToolResult {
                        name: name.to_string(),
                        result: truncated,
                        is_error,
                    });
                }
                // Tool results may change domain state (e.g. tweet drafted count).
                self.broadcast_extension();
            }
            HarnessEvent::TokenUsage {
                prompt_tokens,
                completion_tokens,
            } => {
                self.broadcast(WsMessage::TokenUsage {
                    prompt_tokens: *prompt_tokens,
                    completion_tokens: *completion_tokens,
                });
            }
            HarnessEvent::Reasoning(text) => {
                self.broadcast(WsMessage::Reasoning {
                    text: text.to_string(),
                });
            }
            HarnessEvent::ReasoningDelta(delta) => {
                self.broadcast(WsMessage::ReasoningDelta {
                    delta: delta.to_string(),
                });
            }
            HarnessEvent::Finished => {
                self.broadcast(WsMessage::Finished);
            }
            HarnessEvent::EmptyResponse {
                round,
                attempt,
                max_retries,
            } => {
                self.broadcast(WsMessage::EmptyResponse {
                    round: *round,
                    attempt: *attempt,
                    max_retries: *max_retries,
                });
            }
            HarnessEvent::RoundLimitReached { .. } => {
                self.broadcast(WsMessage::Phase {
                    phase: "Round limit reached".to_string(),
                });
                self.broadcast(WsMessage::Finished);
            }
            HarnessEvent::Eviction {
                freed_chars,
                evicted_count,
            } => {
                self.broadcast(WsMessage::Eviction {
                    freed_chars: *freed_chars,
                    evicted_count: *evicted_count,
                });
            }
            HarnessEvent::Compaction { compaction_number } => {
                self.broadcast(WsMessage::Compaction {
                    compaction_number: *compaction_number,
                });
            }
            HarnessEvent::PreCompaction => {
                // No WebSocket message needed for pre-compaction events.
            }
            HarnessEvent::ModelRouted { model, round } => {
                self.broadcast(WsMessage::ModelRouted {
                    model: model.to_string(),
                    round: *round,
                });
            }
            HarnessEvent::CheckpointSaved { round, path } => {
                self.broadcast(WsMessage::CheckpointSaved {
                    round: *round,
                    path: path.to_string(),
                });
            }
            HarnessEvent::CheckpointResumed { round } => {
                self.broadcast(WsMessage::CheckpointResumed { round: *round });
            }
            HarnessEvent::ToolCacheHit { name, arguments } => {
                self.broadcast(WsMessage::ToolCacheHit {
                    name: name.to_string(),
                    arguments: arguments.to_string(),
                });
            }
            HarnessEvent::ApprovalRequired { name, arguments } => {
                self.broadcast(WsMessage::ApprovalRequired {
                    name: name.to_string(),
                    arguments: arguments.to_string(),
                });
            }
            HarnessEvent::PhaseTransition { from, to } => {
                self.broadcast(WsMessage::Phase {
                    phase: format!("{from:?} → {to:?}"),
                });
            }
            HarnessEvent::PlanSubmitted { summary } => {
                self.broadcast(WsMessage::Text {
                    text: format!("[plan] {summary}"),
                });
            }
            HarnessEvent::MemoryConsolidated {
                lines_before,
                lines_after,
            } => {
                self.broadcast(WsMessage::Phase {
                    phase: format!("Memory consolidated: {lines_before} → {lines_after} lines"),
                });
            }
        }
        None // Never controls flow.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_message_serializes_with_type_tag() {
        let msg = WsMessage::TextDelta {
            delta: "hello".into(),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "text_delta");
        assert_eq!(json["delta"], "hello");
    }

    #[test]
    fn ws_message_round_serializes() {
        let msg = WsMessage::Round {
            round: 3,
            max_rounds: 30,
            context_pct: 0.42,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "round");
        assert_eq!(json["round"], 3);
        assert_eq!(json["max_rounds"], 30);
    }

    #[test]
    fn ws_message_tool_result_serializes() {
        let msg = WsMessage::ToolResult {
            name: "read_file".into(),
            result: "contents".into(),
            is_error: false,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "tool_result");
        assert_eq!(json["is_error"], false);
    }

    #[test]
    fn ws_message_user_message_serializes() {
        let msg = WsMessage::UserMessage {
            message: "hello agent".into(),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "user_message");
        assert_eq!(json["message"], "hello agent");
    }

    #[test]
    fn ws_message_token_usage_serializes() {
        let msg = WsMessage::TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "token_usage");
        assert_eq!(json["prompt_tokens"], 100);
        assert_eq!(json["completion_tokens"], 50);
    }

    #[test]
    fn ws_message_reasoning_delta_serializes() {
        let msg = WsMessage::ReasoningDelta {
            delta: "thinking...".into(),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "reasoning_delta");
        assert_eq!(json["delta"], "thinking...");
    }

    #[test]
    fn broadcast_handler_creation() {
        let (sender, _) = broadcast::channel(16);
        let state = Arc::new(Mutex::new(UiState::default()));
        let ext: Arc<dyn WebExtensionRenderer> = Arc::new(crate::ext::NoWebExtension);
        let handler = WebBroadcastHandler::new(sender, ext, state);

        // Verify it implements EventHandler by calling on_event.
        let result = handler.on_event(&HarnessEvent::Finished);
        assert!(result.is_none());
    }
}
