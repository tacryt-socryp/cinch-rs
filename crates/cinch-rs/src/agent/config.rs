//! Configuration types for the [`Harness`](super::harness::Harness).
//!
//! All advanced modules (eviction, summarization, checkpointing, caching,
//! plan-execute) are **enabled by default** with sensible defaults. Override
//! specific modules through [`HarnessConfig`] struct fields, or use the
//! builder methods for common settings.
//!
//! # Examples
//!
//! Minimal configuration — everything uses defaults:
//!
//! ```ignore
//! let config = HarnessConfig::new("anthropic/claude-sonnet-4", "You are helpful.");
//! ```
//!
//! Customized configuration with builder methods:
//!
//! ```ignore
//! let config = HarnessConfig::new("anthropic/claude-sonnet-4", "You are helpful.")
//!     .with_max_rounds(30)
//!     .with_max_tokens(4096)
//!     .with_temperature(0.3)
//!     .with_streaming(true)
//!     .with_retries(3);
//! ```
//!
//! Disabling specific modules via struct fields:
//!
//! ```ignore
//! let config = HarnessConfig {
//!     plan_execute: HarnessPlanExecuteConfig::disabled(),
//!     session: HarnessSessionConfig::disabled(),
//!     ..HarnessConfig::new("anthropic/claude-sonnet-4", "You are helpful.")
//! };
//! ```

use crate::ReasoningConfig;
use crate::agent::plan_execute::PlanExecuteConfig;
use crate::agent::project_instructions::ProjectInstructions;
use crate::api::retry::RetryConfig;
use crate::api::router::RoutingStrategy;
use crate::context::eviction::EvictionConfig;
use crate::context::summarizer::SummarizerConfig;
use std::path::{Path, PathBuf};

// ── Generic toggle ────────────────────────────────────────────────

/// Generic enabled/disabled wrapper for module configurations.
///
/// Captures the common pattern of `{ enabled: bool, config: T }` used by
/// several harness modules. When `enabled` is `false`, the module is skipped
/// regardless of the inner config values.
#[derive(Debug, Clone)]
pub struct Toggle<T: Default> {
    /// Whether this module is active.
    pub enabled: bool,
    /// Module-specific configuration.
    pub config: T,
}

impl<T: Default> Toggle<T> {
    /// Create a disabled instance with default inner config.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            config: T::default(),
        }
    }
}

impl<T: Default> Default for Toggle<T> {
    fn default() -> Self {
        Self {
            enabled: true,
            config: T::default(),
        }
    }
}

// ── Module-specific type aliases ──────────────────────────────────

/// Eviction module configuration.
pub type HarnessEvictionConfig = Toggle<EvictionConfig>;
/// Summarizer module configuration.
pub type HarnessSummarizerConfig = Toggle<SummarizerConfig>;
/// Plan-execute workflow configuration.
pub type HarnessPlanExecuteConfig = Toggle<PlanExecuteConfig>;

// ── Session config ─────────────────────────────────────────────────

/// Configuration for per-session directories with manifests.
///
/// Each session gets its own directory under `sessions_dir`, containing
/// a `manifest.json` and per-round checkpoint files.
#[derive(Debug, Clone)]
pub struct HarnessSessionConfig {
    /// Whether session management is enabled.
    pub enabled: bool,
    /// Root directory for session directories. Default: `.agents/sessions`.
    pub sessions_dir: PathBuf,
    /// Whether to delete round checkpoint files on successful completion,
    /// keeping only the manifest. Default: `true`.
    pub cleanup_on_success: bool,
}

impl Default for HarnessSessionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sessions_dir: PathBuf::from(".agents/sessions"),
            cleanup_on_success: true,
        }
    }
}

impl HarnessSessionConfig {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            sessions_dir: PathBuf::from(".agents/sessions"),
            cleanup_on_success: true,
        }
    }
}

// ── Cache config ──────────────────────────────────────────────────

/// Configuration for tool result caching within the harness.
#[derive(Debug, Clone)]
pub struct HarnessCacheConfig {
    /// Whether tool result caching is enabled.
    pub enabled: bool,
    /// Maximum number of entries in the cache.
    pub max_entries: usize,
    /// Maximum age (in rounds) before a cache entry is evicted.
    pub max_age_rounds: u32,
}

impl Default for HarnessCacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_entries: 100,
            max_age_rounds: 10,
        }
    }
}

impl HarnessCacheConfig {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            max_entries: 100,
            max_age_rounds: 10,
        }
    }
}

// ── Memory config ─────────────────────────────────────────────────

/// Configuration for the file-based memory system.
#[derive(Debug, Clone)]
pub struct MemoryConfig {
    /// Path to the MEMORY.md file. When set, the harness reads this file
    /// at startup and injects its content into the system prompt.
    pub memory_file: Option<PathBuf>,
    /// Maximum number of lines to include from MEMORY.md before truncation.
    pub max_memory_lines: usize,
    /// Model to use for post-session memory consolidation.
    /// Falls back through: consolidation_model → summarizer model → main model.
    pub consolidation_model: Option<String>,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            memory_file: None,
            max_memory_lines: 200,
            consolidation_model: None,
        }
    }
}

// ── Main harness config ───────────────────────────────────────────

/// Configuration for a [`Harness`](super::harness::Harness) run.
///
/// Controls model selection, round limits, token budgets, and all advanced
/// modules. **All modules are enabled by default** — callers only need to set
/// `model` and optionally `system_prompt` for standard use.
///
/// Two construction patterns are supported:
///
/// - **Builder pattern** — use [`HarnessConfig::new()`] and chain
///   `.with_*()` methods for common settings.
/// - **Struct update syntax** — set advanced module fields (eviction,
///   summarizer, checkpoint, cache, plan-execute) directly using
///   `..Default::default()` or `..HarnessConfig::new(...)`.
///
/// Disabling a module is an explicit override (e.g.
/// [`HarnessSessionConfig::disabled()`]), not the absence of a builder call.
#[derive(Debug, Clone)]
pub struct HarnessConfig {
    /// Model identifier (e.g. `"anthropic/claude-sonnet-4"`).
    pub model: String,
    /// Maximum tool-use round-trips before stopping.
    pub max_rounds: u32,
    /// Maximum tokens per LLM response.
    pub max_tokens: u32,
    /// Sampling temperature.
    pub temperature: f32,
    /// Optional OpenRouter plugins (web-search, response-healing, etc.).
    pub plugins: Option<Vec<crate::Plugin>>,
    /// Optional reasoning / extended thinking configuration.
    pub reasoning: Option<ReasoningConfig>,
    /// Retry configuration for transient API failures.
    pub retry: RetryConfig,

    // ── Advanced module configs (all default to enabled) ──
    /// Model routing strategy. Defaults to single-model (uses `model` field).
    pub routing: RoutingStrategy,
    /// Eviction configuration. Enabled by default.
    pub eviction: HarnessEvictionConfig,
    /// Summarizer configuration. Enabled by default.
    pub summarizer: HarnessSummarizerConfig,
    /// Session management (per-session directories with manifests). Enabled by default.
    pub session: HarnessSessionConfig,
    /// Tool result cache configuration. Enabled by default.
    pub cache: HarnessCacheConfig,
    /// Plan-then-execute workflow. Enabled by default. The agent first plans
    /// with read-only tools, then executes with the full tool set.
    pub plan_execute: HarnessPlanExecuteConfig,
    /// Whether to use streaming for LLM API calls (emits delta events).
    pub streaming: bool,
    /// Tool names that require human approval before execution.
    /// When a tool in this list is about to execute, the harness emits
    /// an `ApprovalRequired` event and waits for the handler's response.
    pub approval_required_tools: Vec<String>,
    /// Force sequential tool execution. When `true`, tool calls within a
    /// round execute one at a time in order, never in parallel. Use this
    /// for tool sets that include destructive operations where same-round
    /// dependencies are a concern. Default: `false` (parallel with
    /// optional dependency-graph ordering via `depends_on` annotations).
    pub sequential_tools: bool,
    /// Context window size in tokens (for context layout thresholds).
    pub context_window_tokens: usize,
    /// Number of recent messages to keep in the raw recency window.
    pub keep_recent_messages: usize,
    /// System prompt (used for context layout prefix).
    pub system_prompt: Option<String>,
    /// File-based memory instructions injected into the system prompt.
    /// When `Some`, the harness appends these instructions to the system
    /// message so the agent knows how to use `memory/` for persistent
    /// learnings and scratchpad notes. Defaults to [`memory_prompt::default_memory_prompt()`].
    /// Set to `None` to disable, or provide a custom string to override.
    pub memory_prompt: Option<String>,
    /// Optional JSON Schema for structured output. When set, the final LLM
    /// response is expected to conform to this schema, and the harness will
    /// attempt to parse the output as JSON and store it in
    /// `HarnessResult::structured_output`.
    pub output_schema: Option<serde_json::Value>,
    /// Memory system configuration (MEMORY.md index loading).
    pub memory_config: MemoryConfig,
    /// Project-level instructions loaded from AGENTS.md hierarchy.
    /// When set, the prompt is injected into the system message and
    /// compaction instructions are forwarded to the summarizer.
    pub project_instructions: Option<ProjectInstructions>,
}

impl HarnessConfig {
    /// Create a config with a model and system prompt.
    ///
    /// Sets the routing strategy to [`RoutingStrategy::Single`] and leaves
    /// all advanced modules at their defaults (enabled). Chain builder methods
    /// or use struct update syntax for further customization.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = HarnessConfig::new("anthropic/claude-sonnet-4", "You are a coding assistant.")
    ///     .with_max_rounds(20)
    ///     .with_max_tokens(4096);
    /// ```
    pub fn new(model: impl Into<String>, system_prompt: impl Into<String>) -> Self {
        let model = model.into();
        Self {
            routing: RoutingStrategy::Single(model.clone()),
            model,
            system_prompt: Some(system_prompt.into()),
            ..Default::default()
        }
    }

    // ── Builder methods ───────────────────────────────────────────
    //
    // Only the settings that callers routinely customise get builder
    // methods.  Internal module knobs (eviction, summarizer, checkpoint,
    // cache, plan-execute, routing, context-window sizing) are public
    // struct fields — power users can set them directly — but they are
    // intentionally excluded from the builder API to keep it small and
    // hard to misuse.

    /// Set the maximum number of tool-use round-trips.
    pub fn with_max_rounds(mut self, max_rounds: u32) -> Self {
        self.max_rounds = max_rounds;
        self
    }

    /// Set the maximum tokens per LLM response.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Set the sampling temperature.
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = temperature;
        self
    }

    /// Set the OpenRouter plugins.
    pub fn with_plugins(mut self, plugins: Vec<crate::Plugin>) -> Self {
        self.plugins = Some(plugins);
        self
    }

    /// Set the reasoning / extended thinking configuration.
    pub fn with_reasoning(mut self, reasoning: ReasoningConfig) -> Self {
        self.reasoning = Some(reasoning);
        self
    }

    /// Enable automatic retries for transient API failures (429, 5xx,
    /// network errors). Uses exponential backoff with jitter.
    ///
    /// Pass `0` to disable retries (the default).
    pub fn with_retries(mut self, max_retries: u32) -> Self {
        self.retry = RetryConfig::with_retries(max_retries);
        self
    }

    /// Enable or disable streaming for LLM API calls.
    pub fn with_streaming(mut self, streaming: bool) -> Self {
        self.streaming = streaming;
        self
    }

    /// Set tool names that require human approval before execution.
    pub fn with_approval_required_tools(mut self, tools: Vec<String>) -> Self {
        self.approval_required_tools = tools;
        self
    }

    /// Set the memory prompt (file-based persistent instructions).
    /// Pass `None` to disable the memory system.
    pub fn with_memory_prompt(mut self, prompt: Option<String>) -> Self {
        self.memory_prompt = prompt;
        self
    }

    /// Set a JSON Schema for structured output.
    pub fn with_output_schema(mut self, schema: serde_json::Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    /// Set a custom planning-phase prompt for the plan-execute workflow.
    ///
    /// This prompt is injected as a user message at the start of the planning
    /// phase. It tells the agent what exploration tools are available and how
    /// to submit a plan.
    ///
    /// Only effective when plan-execute is enabled (the default).
    pub fn with_planning_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.plan_execute.config.planning_prompt = prompt.into();
        self
    }

    /// Set a custom execution-phase prompt for the plan-execute workflow.
    ///
    /// This prompt is injected as a user message when the agent transitions
    /// from planning to execution. It tells the agent that all tools are now
    /// available and it should follow its plan.
    ///
    /// Only effective when plan-execute is enabled (the default).
    pub fn with_execution_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.plan_execute.config.execution_prompt = prompt.into();
        self
    }

    /// Set the path to the MEMORY.md file for memory index loading.
    pub fn with_memory_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.memory_config.memory_file = Some(path.into());
        self
    }

    /// Set the model used for post-session memory consolidation.
    pub fn with_consolidation_model(mut self, model: impl Into<String>) -> Self {
        self.memory_config.consolidation_model = Some(model.into());
        self
    }

    /// Load project instructions from the given project root.
    ///
    /// Searches the standard AGENTS.md file hierarchy and sets
    /// `project_instructions`. If the instructions contain a
    /// `## Compaction Instructions` section, it is forwarded to the
    /// summarizer configuration.
    pub fn with_project_root(mut self, root: impl AsRef<Path>) -> Self {
        let instructions = ProjectInstructions::load(Some(root.as_ref()));
        if let Some(ref ci) = instructions.compaction_instructions {
            self.summarizer.config.compaction_instructions = Some(ci.clone());
        }
        self.project_instructions = Some(instructions);
        self
    }

    /// Set project instructions directly.
    ///
    /// If the instructions contain compaction instructions, they are
    /// forwarded to the summarizer configuration.
    pub fn with_project_instructions(mut self, instructions: ProjectInstructions) -> Self {
        if let Some(ref ci) = instructions.compaction_instructions {
            self.summarizer.config.compaction_instructions = Some(ci.clone());
        }
        self.project_instructions = Some(instructions);
        self
    }
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            model: crate::DEFAULT_MODEL.to_string(),
            max_rounds: 10,
            max_tokens: 1024,
            temperature: 0.7,
            plugins: None,
            reasoning: None,
            retry: RetryConfig::default(),
            routing: RoutingStrategy::default(),
            eviction: HarnessEvictionConfig::default(),
            summarizer: HarnessSummarizerConfig::default(),
            session: HarnessSessionConfig::default(),
            cache: HarnessCacheConfig::default(),
            plan_execute: HarnessPlanExecuteConfig::default(),
            streaming: false,
            approval_required_tools: Vec::new(),
            sequential_tools: false,
            context_window_tokens: 200_000,
            keep_recent_messages: 10,
            system_prompt: None,
            memory_prompt: Some(crate::agent::memory::default_memory_prompt()),
            output_schema: None,
            memory_config: MemoryConfig::default(),
            project_instructions: None,
        }
    }
}
