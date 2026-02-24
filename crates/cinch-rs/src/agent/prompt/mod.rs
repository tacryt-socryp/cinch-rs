//! System prompt architecture: builder, registry, sections, and reminders.
//!
//! The prompt system has three layers:
//!
//! 1. **[`SystemPromptBuilder`]** — low-level builder that assembles `## Section`
//!    blocks into a single prompt string. Used internally by the registry.
//!
//! 2. **[`PromptRegistry`]** — higher-level registry of named prompt sections with
//!    conditions and stability tags. Sections tagged [`Stability::Stable`] form
//!    the cache-friendly prefix; [`Stability::Dynamic`] sections vary per turn.
//!    The registry calls [`SystemPromptBuilder`] internally to produce the final
//!    prompt string. The [`Harness`](super::harness::Harness) can use a registry
//!    for system prompt assembly when
//!    [`HarnessConfig::with_prompt_registry(true)`](super::config::HarnessConfig::with_prompt_registry)
//!    is set — see [`build_default_prompt_registry`](super::harness::build_default_prompt_registry).
//!
//! 3. **[`SystemReminder`]** — mid-conversation system messages injected before
//!    each API call. Used for context warnings, memory nudges, tool guidance, etc.

pub mod builder;
pub mod reminders;
pub mod sections;

pub use builder::SystemPromptBuilder;
pub use reminders::{ReminderFrequency, ReminderRegistry, RoundContext, SystemReminder};
pub use sections::{PromptRegistry, PromptSection, Stability, TurnContext};
