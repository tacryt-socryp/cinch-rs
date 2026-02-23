//! Agent runtime: the [`Harness`] agentic loop and its supporting modules.
//!
//! This module contains everything needed to run an LLM agent:
//!
//! - [`harness::Harness`] — the core agentic tool-use loop. Start here.
//! - [`config::HarnessConfig`] — configuration for model, rounds, tokens, and
//!   all advanced modules.
//! - [`events`] — [`EventHandler`] trait and [`HarnessEvent`] enum for
//!   observing the loop. Includes [`LoggingHandler`], [`CompositeEventHandler`],
//!   [`FnEventHandler`], and [`ToolResultHandler`].
//! - [`checkpoint`] — serializable checkpoint struct for round state.
//! - [`session`] — per-session directories with manifests and checkpoint management.
//! - [`sub_agent`] — recursive sub-agent delegation with
//!   [`TokenBudgetSemaphore`] for tree-wide budget control.
//! - [`plan_execute`] — two-phase workflow: plan with read-only tools first,
//!   then execute with the full tool set.
//! - [`profile`] — persistent agent identity with per-tool usage stats and
//!   cost tracking across sessions.
//! - [`memory`] — file-based cross-session memory (MEMORY.md index, topic files).
//! - [`project_instructions`] — project-level instructions loaded from AGENTS.md
//!   hierarchy with conditional rules and compaction instructions.
//! - [`prompt`] — [`SystemPromptBuilder`] for multi-section prompt assembly,
//!   [`PromptRegistry`] for conditional section loading, and [`ReminderRegistry`]
//!   for mid-conversation system reminders.

pub mod checkpoint;
pub mod config;
pub mod events;
pub mod execution;
pub mod gather;
pub mod harness;
pub mod memory;
pub mod plan_execute;
pub mod profile;
pub mod project_instructions;
pub mod prompt;
pub mod session;
pub mod sub_agent;

// Re-export commonly used items at the module level.
pub use config::{HarnessConfig, HarnessProfileConfig, MemoryConfig};
pub use events::{
    CompositeEventHandler, EventHandler, EventObserver, EventResponse, FnEventHandler,
    HarnessEvent, HarnessResult, LoggingHandler, NoopHandler, StatefulToolResultBuilder,
    ToolResultHandler,
};
pub use gather::{ContextGatherer, GatherEvent, GatherObserver, UiGatherObserver};
pub use harness::Harness;
pub use profile::AgentProfile;
pub use project_instructions::{ConditionalRule, ProjectInstructions};
pub use prompt::{
    PromptRegistry, PromptSection, ReminderFrequency, ReminderRegistry, RoundContext, Stability,
    SystemPromptBuilder, SystemReminder, TurnContext,
};
pub use sub_agent::{SharedResources, TokenBudgetSemaphore};
