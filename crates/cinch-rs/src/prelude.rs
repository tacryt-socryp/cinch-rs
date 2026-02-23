//! Convenience re-exports for common `cinch-rs` types.
//!
//! Meant to be glob-imported when building agents:
//!
//! ```ignore
//! use cinch_rs::prelude::*;
//! ```
//!
//! This pulls in the types needed for the vast majority of agent programs:
//! the [`OpenRouterClient`], [`Message`] constructors, [`Harness`] + config,
//! [`Tool`] trait + [`ToolSet`], event handlers, and context budget.
//! Specialized types (eviction config, streaming events, DAG execution,
//! checkpoint manager) are intentionally excluded — import those from
//! their modules directly when needed.

// ── Core types ──────────────────────────────────────────────────────
pub use crate::{
    ChatRequest, Message, OpenRouterClient, Plugin, PluginVecExt, ToolDef, json_schema_for,
};

// ── Agent runtime ───────────────────────────────────────────────────
pub use crate::agent::{
    CompositeEventHandler, ContextGatherer, EventHandler, EventObserver, EventResponse,
    FnEventHandler, GatherEvent, GatherObserver, Harness, HarnessConfig, HarnessEvent,
    HarnessProfileConfig, HarnessResult, LoggingHandler, NoopHandler, SharedResources,
    SystemPromptBuilder, TokenBudgetSemaphore, ToolResultHandler, UiGatherObserver,
};

// ── Context management ──────────────────────────────────────────────
pub use crate::context::ContextBudget;

// ── Tools ───────────────────────────────────────────────────────────
pub use crate::tools::spec::ToolSpec;
pub use crate::tools::{
    CommonToolsConfig, DisabledTool, FnTool, Tool, ToolCategory, ToolFilter, ToolFuture, ToolSet,
    parse_tool_args,
};

// ── UI state ────────────────────────────────────────────────────────
pub use crate::ui::ask_user_tool::AskUserTool;
pub use crate::ui::event_handler::UiEventHandler;
pub use crate::ui::tracing::UiTracingLayer;
pub use crate::ui::{
    AgentEntry, LogLevel, LogLine, NoExtension, QuestionChoice, QuestionResponse, UiExtension,
    UiState, UserQuestion, ask_question, clear_next_cycle, poll_question, push_agent_text,
    push_agent_text_delta, push_tool_executing, push_tool_result, set_next_cycle, update_phase,
    update_round,
};
