//! Context window management: budgets, layout, eviction, and summarization.
//!
//! The context window is the scarcest resource in any LLM agent. This module
//! provides layered strategies for keeping context usage under control:
//!
//! 1. **[`budget`]** — [`ContextBudget`] tracks estimated token usage and injects
//!    advisory notices when approaching model limits (60% warning, 80% critical).
//!
//! 2. **[`eviction`]** — replaces old tool results with compact one-line
//!    placeholders. Highest ROI context recovery: no LLM call needed, typically
//!    frees 10-100x more tokens than model reasoning occupies.
//!
//! 3. **[`summarizer`]** — LLM-based incremental summarization of middle-zone
//!    messages. Used when eviction alone isn't enough.
//!
//! 4. **[`layout`]** — three-zone message architecture:
//!    - **Pinned prefix** — system prompt + original task. Never modified.
//!      Serves as attention sink and prompt cache anchor.
//!    - **Compressed history** — running summary of completed work.
//!    - **Raw recency window** — last N messages, unmodified. Full fidelity.
//!
//! All four systems are integrated into the [`Harness`](crate::agent::harness::Harness)
//! loop and run automatically when enabled (the default).

pub mod budget;
pub mod eviction;
pub mod file_tracker;
pub mod layout;
pub mod summarizer;

// Re-export commonly used items at the module level.
pub use budget::{ContextBudget, ContextUsage, DEFAULT_CHARS_PER_TOKEN};
pub use layout::ContextBreakdown;
