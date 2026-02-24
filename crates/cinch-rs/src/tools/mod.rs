//! Tool abstractions for LLM function-calling agents.
//!
//! Every agent capability — reading files, searching code, posting data — is a
//! [`Tool`] trait implementor. Tools are collected into a [`ToolSet`] which
//! handles dispatch, validation, truncation, and timeouts.
//!
//! # Defining tools
//!
//! There are three ways to define a tool, from simplest to most flexible:
//!
//! - **[`FnTool`]** — closure-based, auto-parses arguments. Best for simple tools.
//! - **`impl Tool`** — full struct with manual [`Tool::definition()`] and
//!   [`Tool::execute()`]. Best for tools with complex state or ownership.
//! - **[`DisabledTool`]** — wraps a tool definition but always returns an error.
//!   Use for feature-gated tools the LLM can see but not invoke.
//!
//! # Submodules
//!
//! - [`core`] — [`Tool`] trait, [`ToolSet`], [`FnTool`], [`DisabledTool`],
//!   pseudo-tools ([`ThinkTool`], [`TodoTool`]).
//! - [`common`] — built-in tools: `ReadFile`, `EditFile`, `WriteFile`,
//!   `ListFiles`, `Grep`, `FindFiles`, `Shell`. Register all at once with
//!   [`ToolSet::with_common_tools()`].
//! - [`read_tracker`] — [`ReadTracker`] for read-before-write enforcement
//!   shared between `ReadFile`, `EditFile`, and `WriteFile`.
//! - [`spec`] — [`ToolSpec`](spec::ToolSpec) builder for structured tool
//!   descriptions with `when_to_use` / `when_not_to_use` guidance.
//! - [`filter`] — [`ToolFilter`] for dynamic tool selection by category,
//!   keywords, and usage frequency.
//! - [`cache`] — tool result caching with FNV-1a hashing and age-based eviction.
//! - [`dag`] — dependency-aware parallel execution with topological ordering.
//! - [`reflection`] — structured error formatting for LLM self-correction.

pub mod budget;
pub mod cache;
pub mod common;
pub mod core;
pub mod dag;
pub mod filter;
pub mod read_tracker;
pub mod reflection;
pub mod spec;

// Re-export commonly used items at the module level.
pub use budget::ToolBudget;
pub use core::{
    CommonToolsConfig, DisabledTool, FnTool, ThinkTool, TodoTool, Tool, ToolFuture, ToolSet,
};
pub use core::{
    DEFAULT_MAX_RESULT_BYTES, parse_tool_args, truncate_result, validate_tool_arguments,
};
pub use filter::{ToolCategory, ToolFilter};
pub use read_tracker::ReadTracker;
