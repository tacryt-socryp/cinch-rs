//! Generic `EventHandler` → `UiState` bridge.
//!
//! [`UiEventHandler`] automatically maps harness events to UI state updates.
//! Domain crates compose it with their own handlers via
//! [`CompositeEventHandler`](crate::agent::CompositeEventHandler):
//!
//! ```ignore
//! let handler = CompositeEventHandler::new()
//!     .with(LoggingHandler)
//!     .with(domain_handler)
//!     .with(UiEventHandler::new(ui_state.clone()));
//! ```

use std::sync::{Arc, Mutex};

use crate::agent::events::{EventHandler, EventResponse, HarnessEvent};

use super::{
    ContextBreakdownSnapshot, ContextMessageInfo, ContextSnapshot, QuestionChoice,
    QuestionResponse, UiState, UserQuestion, ask_question, poll_question, push_agent_text,
    push_agent_text_delta, push_todo_update, push_tool_executing, push_tool_result,
    update_context_snapshot, update_phase, update_prompt_cache, update_round,
};

/// Event handler that bridges [`HarnessEvent`] variants to [`UiState`] updates.
///
/// Handles all generic UI-relevant events (round progress, text output, tool
/// calls). For `PlanSubmitted` events, presents an approval question to the
/// user and blocks until they approve or provide feedback.
///
/// Domain-specific events (count updates, budget tracking) should be handled
/// by a separate domain handler composed alongside this one.
pub struct UiEventHandler {
    state: Arc<Mutex<UiState>>,
}

impl UiEventHandler {
    /// Create a new handler that writes to the given UI state.
    pub fn new(state: Arc<Mutex<UiState>>) -> Self {
        Self { state }
    }
}

impl EventHandler for UiEventHandler {
    fn on_event(&self, event: &HarnessEvent<'_>) -> Option<EventResponse> {
        match event {
            HarnessEvent::RoundStart {
                round,
                max_rounds,
                context_usage,
                ..
            } => {
                update_round(&self.state, *round, *max_rounds, context_usage.usage_pct);
            }
            HarnessEvent::Text(text) => {
                push_agent_text(&self.state, text);
            }
            HarnessEvent::TextDelta(delta) => {
                push_agent_text_delta(&self.state, delta);
            }
            HarnessEvent::ToolExecuting {
                name, arguments, ..
            } => {
                // The todo tool updates in-place; skip the ToolExecuting entry
                // so the output stream shows only the consolidated checklist.
                if *name != "todo" {
                    update_phase(&self.state, &format!("Tool: {name}"));
                    push_tool_executing(&self.state, name, arguments);
                }
            }
            HarnessEvent::ToolResult { name, result, .. } => {
                if *name == "todo" {
                    push_todo_update(&self.state, result);
                } else {
                    push_tool_result(&self.state, name, result);
                }
            }
            HarnessEvent::Reasoning(text) => {
                push_agent_text(&self.state, text);
            }
            HarnessEvent::PhaseTransition { from, to } => {
                push_agent_text(&self.state, &format!("[phase] {from:?} \u{2192} {to:?}"));
            }
            HarnessEvent::PlanSubmitted { summary } => {
                push_agent_text(&self.state, &format!("[plan] {summary}"));

                // Present approval question to the user.
                // "Approve" accepts; "Suggest changes" lets them edit with feedback.
                let question = UserQuestion {
                    prompt: "Approve this plan? Select 'Suggest changes' and press 'e' to provide feedback.".into(),
                    choices: vec![
                        QuestionChoice {
                            label: "Approve".into(),
                            body: "Accept the plan and start execution.".into(),
                            metadata: String::new(),
                        },
                        QuestionChoice {
                            label: "Suggest changes".into(),
                            body: "Type your feedback here".into(),
                            metadata: String::new(),
                        },
                    ],
                    editable: true,
                    max_edit_length: Some(2000),
                };
                ask_question(&self.state, question, 300);

                // Block until the user responds.
                loop {
                    if let Some(response) = poll_question(&self.state) {
                        return match response {
                            QuestionResponse::Selected(0)
                            | QuestionResponse::TimedOut
                            | QuestionResponse::Skipped => None, // approve
                            QuestionResponse::SelectedEdited { edited_text, .. } => {
                                if edited_text.trim().is_empty() {
                                    None
                                } else {
                                    Some(EventResponse::Deny(edited_text))
                                }
                            }
                            QuestionResponse::FreeText(text) if !text.trim().is_empty() => {
                                Some(EventResponse::Deny(text))
                            }
                            _ => None, // approve by default
                        };
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
            HarnessEvent::Finished => {
                update_phase(&self.state, "Finished");
            }
            HarnessEvent::RoundLimitReached { .. } => {
                update_phase(&self.state, "Round limit reached");
            }
            HarnessEvent::ContextSnapshot {
                messages,
                max_tokens,
                breakdown,
            } => {
                let snapshot = ContextSnapshot {
                    breakdown: Some(ContextBreakdownSnapshot {
                        prefix_tokens: breakdown.prefix_tokens,
                        compressed_history_tokens: breakdown.compressed_history_tokens,
                        middle_tokens: breakdown.middle_tokens,
                        recency_tokens: breakdown.recency_tokens,
                        total_tokens: breakdown.total_tokens,
                    }),
                    messages: messages
                        .iter()
                        .map(|d| ContextMessageInfo {
                            zone: d.zone.clone(),
                            role: d.role.clone(),
                            estimated_tokens: d.estimated_tokens,
                            preview: d.preview.clone(),
                            full_content: d.full_content.clone(),
                            tool_name: d.tool_name.clone(),
                            evicted: d.evicted,
                            message_index: d.message_index,
                            has_cache_breakpoint: d.has_cache_breakpoint,
                        })
                        .collect(),
                    max_tokens: *max_tokens,
                    prompt_cache: None,
                };
                update_context_snapshot(&self.state, snapshot);
            }
            HarnessEvent::PromptCacheStats {
                cached_tokens,
                cache_write_tokens,
            } => {
                update_prompt_cache(
                    &self.state,
                    crate::PromptTokensDetails {
                        cached_tokens: Some(*cached_tokens),
                        cache_write_tokens: Some(*cache_write_tokens),
                    },
                );
            }
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ContextUsage;

    #[test]
    fn ui_event_handler_updates_state() {
        let state = Arc::new(Mutex::new(UiState::default()));
        let handler = UiEventHandler::new(state.clone());

        // RoundStart
        let usage = ContextUsage {
            estimated_tokens: 5000,
            max_tokens: 10000,
            usage_pct: 0.5,
        };
        handler.on_event(&HarnessEvent::RoundStart {
            round: 3,
            max_rounds: 20,
            context_usage: &usage,
            context_breakdown: None,
        });
        {
            let s = state.lock().unwrap();
            assert_eq!(s.round, 3);
            assert_eq!(s.max_rounds, 20);
            assert!((s.context_pct - 0.5).abs() < 0.001);
        }

        // Text
        handler.on_event(&HarnessEvent::Text("Hello from LLM"));
        assert_eq!(state.lock().unwrap().agent_output.len(), 1);

        // TextDelta
        handler.on_event(&HarnessEvent::TextDelta("tok"));
        assert_eq!(state.lock().unwrap().streaming_buffer, "tok");

        // ToolExecuting
        handler.on_event(&HarnessEvent::ToolExecuting {
            name: "read_file",
            arguments: r#"{"path":"a.md"}"#,
        });
        {
            let s = state.lock().unwrap();
            assert_eq!(s.phase, "Tool: read_file");
            assert_eq!(s.agent_output.len(), 2); // Text + ToolExecuting
        }

        // ToolResult
        handler.on_event(&HarnessEvent::ToolResult {
            name: "read_file",
            call_id: "call_1",
            result: "file contents",
        });
        assert_eq!(state.lock().unwrap().agent_output.len(), 3);

        // Finished
        handler.on_event(&HarnessEvent::Finished);
        assert_eq!(state.lock().unwrap().phase, "Finished");

        // RoundLimitReached
        handler.on_event(&HarnessEvent::RoundLimitReached { max_rounds: 30 });
        assert_eq!(state.lock().unwrap().phase, "Round limit reached");
    }

    #[test]
    fn ui_event_handler_always_returns_none() {
        let state = Arc::new(Mutex::new(UiState::default()));
        let handler = UiEventHandler::new(state);

        assert!(handler.on_event(&HarnessEvent::Text("test")).is_none());
        assert!(handler.on_event(&HarnessEvent::Finished).is_none());
        assert!(
            handler
                .on_event(&HarnessEvent::RoundLimitReached { max_rounds: 10 })
                .is_none()
        );
    }
}
