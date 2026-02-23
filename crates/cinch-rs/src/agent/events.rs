//! Events, handlers, and run results for the [`Harness`](super::harness::Harness).
//!
//! The harness communicates with callers through [`HarnessEvent`] variants
//! that cover the full lifecycle of an agent run — from round start through
//! tool execution to completion. Callers implement [`EventHandler`] to
//! observe these events for logging, TUI rendering, metrics, approval
//! workflows, or any other side effects.
//!
//! # Choosing an event handler
//!
//! | Handler | Use case |
//! |---------|----------|
//! | [`NoopHandler`] | Tests or fire-and-forget runs |
//! | [`LoggingHandler`] | Structured logging via `tracing` |
//! | [`FnEventHandler`] | Quick closures for simple callbacks |
//! | [`ToolResultHandler`] | Per-tool-name callbacks (e.g. counting saves) |
//! | [`CompositeEventHandler`] | Compose multiple handlers in order |
//! | Custom `impl EventHandler` | Full control (TUI, metrics, approval gates) |

use crate::Message;
use crate::agent::plan_execute::Phase;
use crate::context::{ContextBreakdown, ContextUsage};
use tracing::{debug, info, trace, warn};

// ── Events ─────────────────────────────────────────────────────────

/// Events emitted by the harness during a run.
///
/// Callers implement [`EventHandler`] to observe these events for logging,
/// TUI updates, state management, or any other side effects.
#[derive(Debug)]
pub enum HarnessEvent<'a> {
    /// A new round is starting.
    RoundStart {
        round: u32,
        max_rounds: u32,
        context_usage: &'a ContextUsage,
        /// Per-zone context breakdown (available when ContextLayout is active).
        context_breakdown: Option<&'a ContextBreakdown>,
    },
    /// The LLM returned a text response (may be alongside tool calls).
    Text(&'a str),
    /// The LLM is requesting tool calls this round.
    ToolCallsReceived { round: u32, count: usize },
    /// A single tool is about to be executed.
    ToolExecuting { name: &'a str, arguments: &'a str },
    /// A single tool finished executing.
    ToolResult {
        name: &'a str,
        call_id: &'a str,
        result: &'a str,
    },
    /// Token usage reported by the API for this round.
    TokenUsage {
        prompt_tokens: u32,
        completion_tokens: u32,
    },
    /// The LLM returned reasoning / extended thinking content.
    Reasoning(&'a str),
    /// The agent finished (no more tool calls).
    Finished,
    /// The API returned an empty response (no content, no tool calls, near-zero
    /// tokens). The harness will automatically retry up to `max_retries` times.
    EmptyResponse {
        round: u32,
        attempt: u32,
        max_retries: u32,
    },
    /// The agent hit the round limit without finishing.
    RoundLimitReached { max_rounds: u32 },

    // ── Advanced module events ──
    /// Context eviction occurred: tool results were replaced with placeholders.
    Eviction {
        freed_chars: usize,
        evicted_count: usize,
    },
    /// Context summarization occurred: middle-zone messages were compacted.
    Compaction { compaction_number: usize },
    /// Fired before context compaction begins. Handlers can return
    /// `EventResponse::InjectMessage(msg)` to preserve critical state
    /// through compaction by including it in the summarization input.
    PreCompaction,
    /// Model routing selected a different model for this round.
    ModelRouted { model: &'a str, round: u32 },
    /// Checkpoint saved after a round.
    CheckpointSaved { round: u32, path: &'a str },
    /// Resumed from a checkpoint.
    CheckpointResumed { round: u32 },
    /// A tool result was served from the cache instead of re-executing.
    ToolCacheHit { name: &'a str, arguments: &'a str },
    /// Incremental text content delta (streaming mode only).
    TextDelta(&'a str),
    /// Incremental reasoning delta (streaming mode only).
    ReasoningDelta(&'a str),
    /// A tool execution requires human approval before proceeding.
    /// The event handler should return an `EventResponse` to approve or deny.
    ApprovalRequired { name: &'a str, arguments: &'a str },
    /// The agent transitioned from the planning phase to the execution phase.
    PhaseTransition { from: &'a Phase, to: &'a Phase },
    /// The agent submitted a plan (called `submit_plan` during planning).
    PlanSubmitted { summary: &'a str },
}

impl HarnessEvent<'_> {
    /// Extract total tokens from a `TokenUsage` event as `u64`.
    ///
    /// Returns `Some(total)` for `TokenUsage` events, `None` for all others.
    /// Saves callers from casting `u32` fields manually:
    ///
    /// ```ignore
    /// // Before:
    /// if let HarnessEvent::TokenUsage { prompt_tokens, completion_tokens } = event {
    ///     let total = *prompt_tokens as u64 + *completion_tokens as u64;
    /// }
    ///
    /// // After:
    /// if let Some(total) = event.total_tokens() { ... }
    /// ```
    pub fn total_tokens(&self) -> Option<u64> {
        if let HarnessEvent::TokenUsage {
            prompt_tokens,
            completion_tokens,
        } = self
        {
            Some(*prompt_tokens as u64 + *completion_tokens as u64)
        } else {
            None
        }
    }
}

/// Response from an event handler for events that support feedback.
///
/// Most events return `None` (no feedback needed). `ApprovalRequired` events
/// use the response to approve, deny, or modify the pending action.
#[derive(Debug, Clone)]
pub enum EventResponse {
    /// Approve the pending action.
    Approve,
    /// Deny the pending action with a reason (passed back to the LLM).
    Deny(String),
    /// Inject a user message into the conversation before the next round.
    InjectMessage(String),
}

/// Handler for harness events.
///
/// Implement this trait to react to agent loop events — updating a TUI,
/// tracking metrics, logging, or gating tool execution with human approval.
/// The default implementation returns `None` (auto-approve, no side effects).
///
/// Most events are informational and the return value is ignored. For
/// [`HarnessEvent::ApprovalRequired`], the return value controls whether the
/// tool executes: return `Some(EventResponse::Approve)` to proceed,
/// `Some(EventResponse::Deny(reason))` to block with an error sent to the
/// LLM, or `None` to auto-approve (the default).
///
/// # Example
///
/// ```ignore
/// struct MyHandler;
///
/// impl EventHandler for MyHandler {
///     fn on_event(&self, event: &HarnessEvent<'_>) -> Option<EventResponse> {
///         match event {
///             HarnessEvent::Text(text) => println!("{text}"),
///             HarnessEvent::ToolResult { name, result, .. } => {
///                 println!("[{name}] {} bytes", result.len());
///             }
///             HarnessEvent::ApprovalRequired { name, .. } => {
///                 // Block dangerous tools.
///                 if name == &"shell" {
///                     return Some(EventResponse::Deny("Shell disabled.".into()));
///                 }
///             }
///             _ => {}
///         }
///         None // Auto-approve everything else.
///     }
/// }
/// ```
pub trait EventHandler: Send + Sync {
    /// Called for each event during the harness run.
    ///
    /// Return `None` for most events. For `ApprovalRequired`, return
    /// `Some(EventResponse::Approve)` or `Some(EventResponse::Deny(reason))`.
    /// The default implementation auto-approves everything.
    fn on_event(&self, event: &HarnessEvent<'_>) -> Option<EventResponse> {
        let _ = event;
        None
    }
}

/// A no-op event handler that auto-approves all actions.
pub struct NoopHandler;
impl EventHandler for NoopHandler {}

/// An event handler backed by a closure.
///
/// Wraps a `Fn(&HarnessEvent) -> Option<EventResponse>` closure into an
/// [`EventHandler`] implementation, avoiding the boilerplate of defining a
/// full struct and impl for simple event handling.
///
/// # Example
///
/// ```ignore
/// let handler = FnEventHandler::new(|event| {
///     if let HarnessEvent::Text(text) = event {
///         println!("{text}");
///     }
///     None
/// });
/// ```
pub struct FnEventHandler<F>(F)
where
    F: Fn(&HarnessEvent<'_>) -> Option<EventResponse> + Send + Sync;

impl<F> FnEventHandler<F>
where
    F: Fn(&HarnessEvent<'_>) -> Option<EventResponse> + Send + Sync,
{
    pub fn new(f: F) -> Self {
        Self(f)
    }
}

impl<F> EventHandler for FnEventHandler<F>
where
    F: Fn(&HarnessEvent<'_>) -> Option<EventResponse> + Send + Sync,
{
    fn on_event(&self, event: &HarnessEvent<'_>) -> Option<EventResponse> {
        (self.0)(event)
    }
}

/// An event handler that delegates to multiple inner handlers.
///
/// Events are dispatched to all handlers in order. The first non-`None`
/// response is returned (for events like `ApprovalRequired` that need
/// feedback). This allows composing specialized handlers that each handle
/// a subset of events.
///
/// # Example
///
/// ```ignore
/// let handler = CompositeEventHandler::new()
///     .with(LoggingHandler)
///     .with(my_tui_handler);
/// ```
pub struct CompositeEventHandler {
    handlers: Vec<Box<dyn EventHandler>>,
}

impl CompositeEventHandler {
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }

    /// Add a handler to the chain. Handlers are called in registration order.
    pub fn with(mut self, handler: impl EventHandler + 'static) -> Self {
        self.handlers.push(Box::new(handler));
        self
    }

    /// Conditionally add a handler to the chain.
    ///
    /// When `condition` is `false`, this is a no-op. Keeps the builder chain
    /// intact for conditional handler composition:
    ///
    /// ```ignore
    /// let handler = CompositeEventHandler::new()
    ///     .with(LoggingHandler)
    ///     .with_if(verbose, DebugHandler::new())
    ///     .with(MyHandler);
    /// ```
    pub fn with_if(self, condition: bool, handler: impl EventHandler + 'static) -> Self {
        if condition { self.with(handler) } else { self }
    }

    /// Add a handler from an `Option`. `None` is a no-op.
    ///
    /// Avoids breaking the builder chain with `if let Some(...)`:
    ///
    /// ```ignore
    /// let handler = CompositeEventHandler::new()
    ///     .with(LoggingHandler)
    ///     .with_opt(tui_state.as_ref().map(|ts| UiEventHandler::new(ts.clone())))
    ///     .with(MyHandler);
    /// ```
    pub fn with_opt(self, handler: Option<impl EventHandler + 'static>) -> Self {
        match handler {
            Some(h) => self.with(h),
            None => self,
        }
    }
}

impl Default for CompositeEventHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl EventHandler for CompositeEventHandler {
    fn on_event(&self, event: &HarnessEvent<'_>) -> Option<EventResponse> {
        for handler in &self.handlers {
            if let Some(response) = handler.on_event(event) {
                return Some(response);
            }
        }
        None
    }
}

/// An event handler that dispatches `ToolResult` events to per-tool-name callbacks.
///
/// Register callbacks for specific tool names. When a `ToolResult` event fires,
/// only the callbacks matching that tool's name are called. All other events are
/// ignored (returns `None`). Compose with `CompositeEventHandler` alongside
/// `LoggingHandler` and any other handlers.
///
/// # Example
///
/// ```ignore
/// use std::sync::{Arc, Mutex};
///
/// let counter = Arc::new(Mutex::new(0u32));
/// let c = counter.clone();
///
/// let handler = ToolResultHandler::new()
///     .on("save_draft", move |result| {
///         if !result.starts_with("Error") {
///             *c.lock().unwrap() += 1;
///         }
///     });
///
/// let composite = CompositeEventHandler::new()
///     .with(LoggingHandler)
///     .with(handler);
/// ```
/// Boxed callback for a tool result.
type ToolResultCallback = Box<dyn Fn(&str) + Send + Sync>;

pub struct ToolResultHandler {
    callbacks: Vec<(String, ToolResultCallback)>,
}

impl ToolResultHandler {
    /// Create an empty handler with no callbacks.
    pub fn new() -> Self {
        Self {
            callbacks: Vec::new(),
        }
    }

    /// Register a callback for a specific tool name (builder pattern).
    ///
    /// The callback receives the tool result string. Multiple callbacks
    /// can be registered for the same tool name — all will fire.
    pub fn on(
        mut self,
        tool_name: impl Into<String>,
        callback: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        self.callbacks.push((tool_name.into(), Box::new(callback)));
        self
    }

    /// Create a stateful builder that auto-shares an `Arc<Mutex<S>>`
    /// across all callbacks, eliminating manual Arc cloning.
    ///
    /// Each callback registered via
    /// [`on`](StatefulToolResultBuilder::on) receives `(&S, &str)` —
    /// the shared state (locked) and the tool result.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use std::sync::{Arc, Mutex};
    ///
    /// #[derive(Default)]
    /// struct Counts { saves: u32, posts: u32 }
    ///
    /// let state = Arc::new(Mutex::new(Counts::default()));
    ///
    /// // Before: manual Arc cloning per callback
    /// // let s1 = state.clone();
    /// // let s2 = state.clone();
    /// // ToolResultHandler::new()
    /// //     .on("save_draft", move |r| { s1.lock().unwrap().saves += 1; })
    /// //     .on("post_tweet", move |r| { s2.lock().unwrap().posts += 1; })
    ///
    /// // After: state is shared automatically
    /// let handler = ToolResultHandler::with_state(state.clone())
    ///     .on("save_draft", |s, _result| { s.saves += 1; })
    ///     .on("post_tweet", |s, _result| { s.posts += 1; })
    ///     .build();
    /// ```
    pub fn with_state<S: Send + 'static>(
        state: std::sync::Arc<std::sync::Mutex<S>>,
    ) -> StatefulToolResultBuilder<S> {
        StatefulToolResultBuilder {
            state,
            handler: ToolResultHandler::new(),
        }
    }
}

/// Builder for a [`ToolResultHandler`] with shared state.
///
/// Created by [`ToolResultHandler::with_state()`]. Each callback registered
/// via [`on`](Self::on) automatically receives a mutable reference to the
/// shared state (behind the `Arc<Mutex<S>>`) along with the tool result
/// string. Call [`build`](Self::build) to finalize.
pub struct StatefulToolResultBuilder<S: Send + 'static> {
    state: std::sync::Arc<std::sync::Mutex<S>>,
    handler: ToolResultHandler,
}

impl<S: Send + 'static> StatefulToolResultBuilder<S> {
    /// Register a callback for a specific tool name.
    ///
    /// The callback receives `(&mut S, &str)` — the locked shared state
    /// and the tool result. The `Arc<Mutex<S>>` is cloned automatically.
    pub fn on(
        mut self,
        tool_name: impl Into<String>,
        callback: impl Fn(&mut S, &str) + Send + Sync + 'static,
    ) -> Self {
        let state = self.state.clone();
        self.handler = self.handler.on(tool_name, move |result: &str| {
            if let Ok(mut s) = state.lock() {
                callback(&mut s, result);
            }
        });
        self
    }

    /// Finalize and return the [`ToolResultHandler`].
    pub fn build(self) -> ToolResultHandler {
        self.handler
    }
}

impl Default for ToolResultHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl EventHandler for ToolResultHandler {
    fn on_event(&self, event: &HarnessEvent<'_>) -> Option<EventResponse> {
        if let HarnessEvent::ToolResult { name, result, .. } = event {
            for (tool_name, callback) in &self.callbacks {
                if tool_name == *name {
                    callback(result);
                }
            }
        }
        None
    }
}

/// Wrapper that adapts an observation-only closure into an [`EventHandler`].
///
/// Many event handlers are pure observers — they log, update UI, or track
/// metrics but never return an [`EventResponse`]. `EventObserver` removes
/// the boilerplate of returning `None` from every handler:
///
/// ```ignore
/// // Before — must return Option<EventResponse>:
/// struct MyHandler;
/// impl EventHandler for MyHandler {
///     fn on_event(&self, event: &HarnessEvent<'_>) -> Option<EventResponse> {
///         if let HarnessEvent::Text(t) = event { println!("{t}"); }
///         None  // always None
///     }
/// }
///
/// // After — just observe:
/// let handler = EventObserver::new(|event| {
///     if let HarnessEvent::Text(t) = event { println!("{t}"); }
/// });
/// ```
pub struct EventObserver<F>(F)
where
    F: Fn(&HarnessEvent<'_>) + Send + Sync;

impl<F> EventObserver<F>
where
    F: Fn(&HarnessEvent<'_>) + Send + Sync,
{
    /// Wrap a closure that observes events without producing responses.
    pub fn new(f: F) -> Self {
        Self(f)
    }
}

impl<F> EventHandler for EventObserver<F>
where
    F: Fn(&HarnessEvent<'_>) + Send + Sync,
{
    fn on_event(&self, event: &HarnessEvent<'_>) -> Option<EventResponse> {
        (self.0)(event);
        None
    }
}

/// An event handler that logs events via `tracing`.
pub struct LoggingHandler;

impl EventHandler for LoggingHandler {
    fn on_event(&self, event: &HarnessEvent<'_>) -> Option<EventResponse> {
        match event {
            HarnessEvent::RoundStart {
                round,
                max_rounds,
                context_usage,
                context_breakdown,
            } => {
                info!(
                    "[round {}/{}] {}",
                    round,
                    max_rounds,
                    context_usage.to_log_string()
                );
                if let Some(bd) = context_breakdown {
                    debug!(
                        "  zones: prefix={}t, history={}t, middle={}t, recency={}t",
                        bd.prefix_tokens,
                        bd.compressed_history_tokens,
                        bd.middle_tokens,
                        bd.recency_tokens,
                    );
                }
            }
            HarnessEvent::Text(text) => {
                let preview: String = text.chars().take(200).collect();
                debug!(
                    "LLM text: {preview}{}",
                    if text.len() > 200 { "..." } else { "" }
                );
            }
            HarnessEvent::ToolCallsReceived { round, count } => {
                debug!("{count} tool call(s) in round {round}");
            }
            HarnessEvent::ToolExecuting { name, .. } => {
                debug!("Executing tool: {name}");
            }
            HarnessEvent::ToolResult { name, result, .. } => {
                debug!("Tool {name} result: {} bytes", result.len());
            }
            HarnessEvent::Reasoning(text) => {
                let preview: String = text.chars().take(200).collect();
                debug!(
                    "LLM reasoning: {preview}{}",
                    if text.len() > 200 { "..." } else { "" }
                );
            }
            HarnessEvent::TokenUsage {
                prompt_tokens,
                completion_tokens,
            } => {
                debug!("Tokens: prompt={prompt_tokens}, completion={completion_tokens}");
            }
            HarnessEvent::Finished => {
                info!("Agent finished (no more tool calls)");
            }
            HarnessEvent::EmptyResponse {
                round,
                attempt,
                max_retries,
            } => {
                warn!(
                    "Empty API response at round {round} (no content, no tool calls, ~0 tokens). \
                     Retrying ({attempt}/{max_retries})..."
                );
            }
            HarnessEvent::RoundLimitReached { max_rounds } => {
                info!("Agent hit round limit ({max_rounds})");
            }
            HarnessEvent::Eviction {
                freed_chars,
                evicted_count,
            } => {
                info!("Evicted {evicted_count} tool result(s), freed {freed_chars} chars");
            }
            HarnessEvent::Compaction { compaction_number } => {
                info!("Context compaction #{compaction_number} completed");
            }
            HarnessEvent::PreCompaction => {
                debug!("Pre-compaction event fired");
            }
            HarnessEvent::ModelRouted { model, round } => {
                debug!("Round {round}: routed to model {model}");
            }
            HarnessEvent::CheckpointSaved { round, path } => {
                debug!("Checkpoint saved at round {round}: {path}");
            }
            HarnessEvent::CheckpointResumed { round } => {
                info!("Resumed from checkpoint at round {round}");
            }
            HarnessEvent::ToolCacheHit { name, .. } => {
                debug!("Tool cache hit: {name}");
            }
            HarnessEvent::TextDelta(delta) => {
                let preview: String = delta.chars().take(80).collect();
                trace!("Stream text delta: {preview}");
            }
            HarnessEvent::ReasoningDelta(delta) => {
                let preview: String = delta.chars().take(80).collect();
                trace!("Stream reasoning delta: {preview}");
            }
            HarnessEvent::ApprovalRequired { name, .. } => {
                info!("Approval required for tool: {name}");
                // LoggingHandler auto-approves.
                return Some(EventResponse::Approve);
            }
            HarnessEvent::PhaseTransition { from, to } => {
                info!("Phase transition: {from:?} → {to:?}");
            }
            HarnessEvent::PlanSubmitted { summary } => {
                info!("Plan submitted: {summary}");
            }
        }
        None
    }
}

// ── Run result ─────────────────────────────────────────────────────

/// The result of a complete [`Harness::run()`](super::harness::Harness::run).
///
/// Contains all text output, the full conversation transcript, token usage,
/// cost estimate, and (optionally) parsed structured output. Use
/// [`text()`](HarnessResult::text) for the concatenated LLM text and
/// [`finished`](HarnessResult::finished) to check if the agent completed
/// naturally vs. hitting the round limit.
#[derive(Debug)]
pub struct HarnessResult {
    /// Unique trace ID for this run.
    pub trace_id: String,
    /// All messages exchanged during the run (including the initial ones).
    pub messages: Vec<Message>,
    /// Accumulated text output from the LLM across all rounds.
    pub text_output: Vec<String>,
    /// All URL citation annotations collected across rounds.
    pub annotations: Vec<crate::Annotation>,
    /// Total prompt tokens consumed across all rounds.
    pub total_prompt_tokens: u32,
    /// Total completion tokens consumed across all rounds.
    pub total_completion_tokens: u32,
    /// Number of rounds executed.
    pub rounds_used: u32,
    /// Whether the agent finished naturally (vs hitting the round limit).
    pub finished: bool,
    /// Estimated cost in USD for the run.
    pub estimated_cost_usd: f64,
    /// Parsed structured output (when `HarnessConfig::output_schema` is set
    /// and the final LLM response is valid JSON).
    pub structured_output: Option<serde_json::Value>,
}

impl HarnessResult {
    /// Concatenated text output from all rounds.
    pub fn text(&self) -> String {
        self.text_output.join("\n\n")
    }

    /// Total tokens (prompt + completion).
    pub fn total_tokens(&self) -> u32 {
        self.total_prompt_tokens + self.total_completion_tokens
    }
}
