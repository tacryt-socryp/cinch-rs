//! Tool abstraction for LLM function-calling agents.
//!
//! The [`Tool`] trait defines the interface that every tool must implement:
//! a static API definition (name, description, JSON schema) and an async
//! `execute` method. Tools are collected into a [`ToolSet`] which handles
//! dispatch, definition export, and result truncation.

use crate::ToolDef;
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use tracing::{debug, info, trace};

/// Maximum size (in bytes) for tool output before truncation.
pub const DEFAULT_MAX_RESULT_BYTES: usize = 30_000;

/// Boxed future returned by [`Tool::execute`].
///
/// Type alias to keep trait signatures and implementations readable.
pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = String> + Send + 'a>>;

// ── CommonToolsConfig ────────────────────────────────────────────────

/// Per-tool configuration for [`ToolSet::with_common_tools_configured`].
///
/// Override defaults for the built-in common tools (grep match limits,
/// find result limits, shell command blocklists) without manually
/// registering each tool individually.
///
/// # Example
///
/// ```ignore
/// let config = CommonToolsConfig::default()
///     .grep_max_matches(500)
///     .find_max_results(200)
///     .shell_block_command("dangerous-cmd");
/// ```
#[derive(Debug, Clone)]
pub struct CommonToolsConfig {
    /// Maximum grep matches per file before truncation.
    /// Default: [`DEFAULT_MAX_GREP_MATCHES`](crate::tools::common::DEFAULT_MAX_GREP_MATCHES) (200).
    pub grep_max_matches: u32,
    /// Maximum find results before truncation.
    /// Default: [`DEFAULT_MAX_FIND_RESULTS`](crate::tools::common::DEFAULT_MAX_FIND_RESULTS) (100).
    pub find_max_results: u32,
    /// Blocked shell command patterns (lowercased substring match).
    /// Default: `["rm -rf /", "mkfs", "> /dev/"]`.
    pub shell_blocked_commands: Vec<String>,
}

impl Default for CommonToolsConfig {
    fn default() -> Self {
        use crate::tools::common::{
            DEFAULT_BLOCKED_COMMANDS, DEFAULT_MAX_FIND_RESULTS, DEFAULT_MAX_GREP_MATCHES,
        };
        Self {
            grep_max_matches: DEFAULT_MAX_GREP_MATCHES,
            find_max_results: DEFAULT_MAX_FIND_RESULTS,
            shell_blocked_commands: DEFAULT_BLOCKED_COMMANDS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        }
    }
}

impl CommonToolsConfig {
    /// Set the maximum grep matches per file.
    pub fn grep_max_matches(mut self, max: u32) -> Self {
        self.grep_max_matches = max;
        self
    }

    /// Set the maximum find results.
    pub fn find_max_results(mut self, max: u32) -> Self {
        self.find_max_results = max;
        self
    }

    /// Replace the shell blocked commands list.
    pub fn shell_blocked_commands(mut self, commands: Vec<String>) -> Self {
        self.shell_blocked_commands = commands;
        self
    }

    /// Add a single blocked shell command pattern.
    pub fn shell_block_command(mut self, command: impl Into<String>) -> Self {
        self.shell_blocked_commands.push(command.into());
        self
    }
}

// ── Tool trait ─────────────────────────────────────────────────────

/// A tool that an LLM agent can invoke via function-calling.
///
/// Implementors provide:
/// - A static definition ([`Tool::definition`]) describing the tool's name,
///   description, and JSON Schema parameters for the LLM.
/// - An async [`Tool::execute`] method that receives the raw JSON arguments
///   string and returns a result string.
///
/// # Example
///
/// ```ignore
/// struct ReadFile { workdir: String }
///
/// impl Tool for ReadFile {
///     fn definition(&self) -> ToolDef { /* ... */ }
///
///     fn execute(&self, arguments: &str) -> ToolFuture<'_> {
///         let workdir = self.workdir.clone();
///         let arguments = arguments.to_string();
///         Box::pin(async move {
///             // parse args, read file, return content
///             todo!()
///         })
///     }
/// }
/// ```
pub trait Tool: Send + Sync {
    /// The tool definition sent to the LLM API.
    fn definition(&self) -> ToolDef;

    /// Execute the tool with the given raw JSON arguments string.
    ///
    /// Returns the tool result as a string. Errors should be returned as
    /// `"Error: ..."` strings rather than panicking — the harness will pass
    /// the string back to the LLM as a tool result regardless.
    ///
    /// Uses a boxed future so that the trait is dyn-compatible (object-safe).
    fn execute(&self, arguments: &str) -> ToolFuture<'_>;

    /// The tool's name (convenience — delegates to definition).
    fn name(&self) -> String {
        self.definition().function.name.clone()
    }

    /// Whether this tool's results can be cached (read-only, deterministic
    /// for the same arguments within a session). Defaults to `false`.
    fn cacheable(&self) -> bool {
        false
    }

    /// Whether this tool mutates external state and should invalidate cached
    /// results from other tools. Defaults to `false`.
    fn is_mutation(&self) -> bool {
        false
    }
}

// ── ToolSet ────────────────────────────────────────────────────────

/// A collection of tools that can be dispatched by name.
///
/// Manages tool registration, definition export (for the LLM API), and
/// dispatch with timing, validation, and truncation. This is the tool-side
/// counterpart to the [`Harness`](crate::agent::harness::Harness).
///
/// # Example
///
/// ```ignore
/// // Start with the built-in file and shell tools.
/// let tools = ToolSet::new()
///     .with_max_result_bytes(15_000)
///     .with_common_tools("/path/to/workdir")
///     .with_arg_validation(true)
///     .with_default_timeout(Some(Duration::from_secs(30)));
///
/// // Add custom tools.
/// let tools = tools
///     .with(MyCustomTool::new())
///     .with_if(auto_post_enabled, PostTweetTool::new())
///     .with_if(!auto_post_enabled, DisabledTool::new(
///         post_tweet_def(),
///         "Posting disabled. Run with --auto-post.",
///     ));
///
/// // Export definitions for the LLM API.
/// let defs = tools.definitions();
/// ```
/// Default timeout for tool execution (60 seconds).
pub const DEFAULT_TOOL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

pub struct ToolSet {
    tools: HashMap<String, Box<dyn Tool>>,
    max_result_bytes: usize,
    /// Whether to validate tool arguments against JSON Schema before execution.
    validate_args: bool,
    /// Default timeout for tool execution. `None` disables timeouts.
    default_timeout: Option<std::time::Duration>,
    /// Tool names whose results are cacheable (populated from `Tool::cacheable()`).
    cacheable_tools: HashSet<String>,
    /// Tool names that mutate state (populated from `Tool::is_mutation()`).
    mutation_tools: HashSet<String>,
}

impl fmt::Debug for ToolSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolSet")
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .field("max_result_bytes", &self.max_result_bytes)
            .finish()
    }
}

impl ToolSet {
    /// Create an empty tool set.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            max_result_bytes: DEFAULT_MAX_RESULT_BYTES,
            validate_args: false,
            default_timeout: None,
            cacheable_tools: HashSet::new(),
            mutation_tools: HashSet::new(),
        }
    }

    /// Set the maximum result size in bytes before truncation.
    pub fn with_max_result_bytes(mut self, max: usize) -> Self {
        self.max_result_bytes = max;
        self
    }

    /// Enable JSON Schema argument validation before tool execution.
    pub fn with_arg_validation(mut self, enabled: bool) -> Self {
        self.validate_args = enabled;
        self
    }

    /// Set a default timeout for tool execution. Applies to all tools unless
    /// overridden. Pass `None` to disable timeouts.
    pub fn with_default_timeout(mut self, timeout: Option<std::time::Duration>) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Register a tool. Replaces any existing tool with the same name.
    pub fn register(&mut self, tool: impl Tool + 'static) {
        let name = tool.name();
        if tool.cacheable() {
            self.cacheable_tools.insert(name.clone());
        }
        if tool.is_mutation() {
            self.mutation_tools.insert(name.clone());
        }
        self.tools.insert(name, Box::new(tool));
    }

    /// Register a tool (builder pattern).
    pub fn with(mut self, tool: impl Tool + 'static) -> Self {
        self.register(tool);
        self
    }

    /// Conditionally register a tool (builder pattern).
    ///
    /// Adds the tool only when `condition` is `true`. This keeps the
    /// builder chain intact for conditional tool registration instead of
    /// requiring mutable reassignment:
    ///
    /// ```ignore
    /// let tools = ToolSet::new()
    ///     .with(ReadFile::new(workdir))
    ///     .with_if(verbose, DebugTool::new());
    /// ```
    pub fn with_if(self, condition: bool, tool: impl Tool + 'static) -> Self {
        if condition { self.with(tool) } else { self }
    }

    /// Return all tool definitions for the LLM API.
    pub fn definitions(&self) -> Vec<ToolDef> {
        self.tools.values().map(|t| t.definition()).collect()
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Register all common tools ([`ReadFile`], [`ListFiles`], [`Grep`],
    /// [`FindFiles`], [`Shell`], [`WebSearch`]) plus the [`ThinkTool`] and
    /// [`TodoTool`] pseudo-tools. Common tools inherit the `ToolSet`'s
    /// `max_result_bytes`.
    ///
    /// This is a convenience method for the typical agent setup pattern.
    /// Use individual `.with()` calls if you need per-tool configuration.
    ///
    /// [`ReadFile`]: crate::tools::common::ReadFile
    /// [`ListFiles`]: crate::tools::common::ListFiles
    /// [`Grep`]: crate::tools::common::Grep
    /// [`FindFiles`]: crate::tools::common::FindFiles
    /// [`Shell`]: crate::tools::common::Shell
    pub fn with_common_tools(self, workdir: impl Into<String>) -> Self {
        self.with_common_tools_configured(workdir, CommonToolsConfig::default())
    }

    /// Register common tools with per-tool configuration overrides.
    ///
    /// Like [`with_common_tools`](Self::with_common_tools), but applies
    /// configuration from a [`CommonToolsConfig`]. Use this when you need
    /// per-tool limits (grep max matches, find max results, shell blocklist)
    /// without manually registering each tool.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let tools = ToolSet::new()
    ///     .with_max_result_bytes(50_000)
    ///     .with_common_tools_configured(".", CommonToolsConfig::default()
    ///         .grep_max_matches(500)
    ///         .find_max_results(200)
    ///         .shell_blocked_commands(vec!["rm -rf /".into(), "mkfs".into()]));
    /// ```
    pub fn with_common_tools_configured(
        self,
        workdir: impl Into<String>,
        config: CommonToolsConfig,
    ) -> Self {
        use crate::tools::common::{FindFiles, Grep, ListFiles, ReadFile, Shell, WebSearch};
        let workdir = workdir.into();
        let max = self.max_result_bytes;
        self.with(ReadFile::new(workdir.clone()).max_result_bytes(max))
            .with(ListFiles::new(workdir.clone()))
            .with(
                Grep::new(workdir.clone())
                    .max_matches(config.grep_max_matches)
                    .max_result_bytes(max),
            )
            .with(
                FindFiles::new(workdir.clone())
                    .max_results(config.find_max_results)
                    .max_result_bytes(max),
            )
            .with(
                Shell::new(workdir)
                    .blocked_commands(config.shell_blocked_commands)
                    .max_result_bytes(max),
            )
            .with_if(
                std::env::var("BRAVE_SEARCH_KEY").is_ok(),
                WebSearch::new().max_result_bytes(max),
            )
            .with(ThinkTool)
            .with(TodoTool::new())
    }

    /// Whether a tool's results are cacheable (read-only, deterministic).
    pub fn is_cacheable(&self, tool_name: &str) -> bool {
        self.cacheable_tools.contains(tool_name)
    }

    /// Whether a tool mutates state and should invalidate cached results.
    pub fn is_mutation_tool(&self, tool_name: &str) -> bool {
        self.mutation_tools.contains(tool_name)
    }

    /// Execute a tool call by name, with optional validation, timing, and truncation.
    ///
    /// If argument validation is enabled, validates arguments against the tool's
    /// declared JSON Schema before execution. Returns a structured error on
    /// validation failure so the LLM can self-correct.
    ///
    /// Returns the (possibly truncated) result string.
    /// Returns an error string if the tool name is unknown.
    pub async fn execute(&self, name: &str, arguments: &str) -> String {
        let tool = match self.tools.get(name) {
            Some(t) => t,
            None => return format!("Error: unknown tool '{name}'"),
        };

        // Validate arguments against JSON Schema if enabled.
        if self.validate_args
            && let Some(error) = validate_tool_arguments(tool.as_ref(), arguments)
        {
            return error;
        }

        log_tool_call(name, arguments);
        let start = std::time::Instant::now();

        // Execute with optional timeout.
        let result = if let Some(timeout_duration) = self.default_timeout {
            match tokio::time::timeout(timeout_duration, tool.execute(arguments)).await {
                Ok(r) => r,
                Err(_) => {
                    let elapsed = start.elapsed();
                    info!(
                        "Tool {name} timed out after {:.1}s (limit: {:.0}s)",
                        elapsed.as_secs_f64(),
                        timeout_duration.as_secs_f64(),
                    );
                    format!(
                        "Error: tool '{name}' timed out after {:.0} seconds. \
                         Consider breaking the task into smaller steps or using \
                         different arguments.",
                        timeout_duration.as_secs_f64(),
                    )
                }
            }
        } else {
            tool.execute(arguments).await
        };

        let elapsed = start.elapsed();
        debug!(
            "Tool {name} completed in {:.0}ms ({} bytes)",
            elapsed.as_secs_f64() * 1000.0,
            result.len()
        );
        trace!(
            "Tool {name} result preview: {}",
            &result[..result.len().min(300)]
        );

        truncate_result(result, self.max_result_bytes)
    }
}

impl Default for ToolSet {
    fn default() -> Self {
        Self::new()
    }
}

// ── DisabledTool ───────────────────────────────────────────────────

/// A tool that always returns an error message when executed.
///
/// Use this to register a "disabled" variant of a tool that the LLM can
/// still see in its tool list (preserving the description and schema) but
/// that returns a static error message explaining why it's unavailable.
///
/// This eliminates the need to create separate disabled-stub structs for
/// every feature-gated tool.
///
/// # Example
///
/// ```ignore
/// let tools = ToolSet::new()
///     .with_if(auto_post, PostTweet { platform })
///     .with_if(!auto_post, DisabledTool::new(
///         post_tweet_def(),
///         "Tweet posting is disabled. Run with --auto-post to enable.",
///     ));
/// ```
pub struct DisabledTool {
    def: ToolDef,
    reason: String,
}

impl DisabledTool {
    /// Create a disabled tool with the given definition and error reason.
    ///
    /// When executed, returns `"Error: {reason}"`.
    pub fn new(def: ToolDef, reason: impl Into<String>) -> Self {
        Self {
            def,
            reason: reason.into(),
        }
    }

    /// Create a disabled variant of an existing tool.
    ///
    /// Extracts the [`ToolDef`] from `tool` so the LLM sees the same name,
    /// description, and schema, but execution always returns an error with
    /// the given reason.
    ///
    /// This avoids the need to factor out a shared `ToolDef`-returning
    /// function when a tool can be conditionally enabled/disabled:
    ///
    /// ```ignore
    /// let tool = post_tweet_tool(platform);
    /// if auto_post {
    ///     set.register(tool);
    /// } else {
    ///     set.register(DisabledTool::from_tool(&tool, "Posting disabled."));
    /// }
    /// ```
    pub fn from_tool(tool: &dyn Tool, reason: impl Into<String>) -> Self {
        Self {
            def: tool.definition(),
            reason: reason.into(),
        }
    }
}

impl Tool for DisabledTool {
    fn definition(&self) -> ToolDef {
        self.def.clone()
    }

    fn execute(&self, _arguments: &str) -> ToolFuture<'_> {
        let msg = format!("Error: {}", self.reason);
        Box::pin(async move { msg })
    }
}

// ── FnTool ────────────────────────────────────────────────────────

/// A closure-based tool that auto-parses arguments and delegates to a handler.
///
/// Eliminates the boilerplate of defining a struct + `impl Tool` for simple
/// tools whose execute logic is a pure async function. The generic constructor
/// performs type erasure so `FnTool` is a concrete, dyn-compatible type.
///
/// Use [`FnTool`] for stateless tools. For tools that need shared state
/// (database connections, API clients, configuration), define a struct and
/// implement the [`Tool`] trait directly.
///
/// # Example
///
/// ```ignore
/// use cinch_rs::prelude::*;
/// use serde::Deserialize;
/// use schemars::JsonSchema;
///
/// #[derive(Deserialize, JsonSchema)]
/// struct SearchArgs {
///     /// The search query.
///     query: String,
///     /// Maximum number of results to return.
///     #[serde(default = "default_limit")]
///     limit: u32,
/// }
/// fn default_limit() -> u32 { 10 }
///
/// let tool = FnTool::new(
///     ToolDef::new("search", "Search the knowledge base", json_schema_for::<SearchArgs>()),
///     |args: SearchArgs| async move {
///         format!("Found {} results for: {}", args.limit, args.query)
///     },
/// );
///
/// let tools = ToolSet::new().with(tool);
/// ```
/// Type-erased async handler for [`FnTool`].
type ErasedToolHandler =
    Box<dyn Fn(String) -> Pin<Box<dyn Future<Output = String> + Send>> + Send + Sync>;

pub struct FnTool {
    def: ToolDef,
    handler: ErasedToolHandler,
    mutation: bool,
}

impl FnTool {
    /// Create a new closure-based tool.
    ///
    /// The handler receives parsed arguments of type `A` (auto-deserialized
    /// from the raw JSON string) and returns a future that produces the result
    /// string. Parse errors are automatically formatted for the LLM.
    pub fn new<A, F, Fut>(def: ToolDef, handler: F) -> Self
    where
        A: serde::de::DeserializeOwned + Send + 'static,
        F: Fn(A) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = String> + Send + 'static,
    {
        let erased = move |raw: String| -> Pin<Box<dyn Future<Output = String> + Send>> {
            let args: A = match serde_json::from_str(&raw) {
                Ok(a) => a,
                Err(e) => {
                    return Box::pin(async move {
                        format!(
                            "Error: invalid tool arguments: {e}. \
                                 Please provide valid JSON matching the tool's parameter schema."
                        )
                    });
                }
            };
            Box::pin(handler(args))
        };

        Self {
            def,
            handler: Box::new(erased),
            mutation: false,
        }
    }

    /// Mark this tool as a mutation (builder pattern).
    pub fn mutation(mut self, is_mutation: bool) -> Self {
        self.mutation = is_mutation;
        self
    }
}

impl Tool for FnTool {
    fn definition(&self) -> ToolDef {
        self.def.clone()
    }

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let fut = (self.handler)(arguments.to_string());
        // Safety: the future is 'static (from the erased handler), which
        // satisfies the 'a lifetime in ToolFuture<'a>.
        Box::pin(fut)
    }

    fn is_mutation(&self) -> bool {
        self.mutation
    }
}

impl fmt::Debug for FnTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FnTool")
            .field("name", &self.def.function.name)
            .field("mutation", &self.mutation)
            .finish()
    }
}

// ── Helpers ────────────────────────────────────────────────────────

/// Validate tool arguments against the tool's declared JSON Schema.
///
/// Returns `None` if valid, or `Some(error_string)` if validation fails.
/// The error string is formatted for the LLM to understand and self-correct.
pub fn validate_tool_arguments(tool: &dyn Tool, arguments: &str) -> Option<String> {
    let args_value: serde_json::Value = match serde_json::from_str(arguments) {
        Ok(v) => v,
        Err(e) => {
            return Some(format!(
                "Error: invalid JSON arguments for tool '{}': {e}. \
                 Please provide valid JSON matching the tool's parameter schema.",
                tool.name()
            ));
        }
    };

    let schema = tool.definition().function.parameters;

    // Use jsonschema for validation.
    let validator = match jsonschema::validator_for(&schema) {
        Ok(v) => v,
        Err(_) => return None, // If schema itself is invalid, skip validation.
    };

    let errors: Vec<String> = validator
        .iter_errors(&args_value)
        .map(|e| format!("  - {}: {e}", e.instance_path()))
        .collect();

    if errors.is_empty() {
        None
    } else {
        Some(format!(
            "Error: argument validation failed for tool '{}':\n{}\n\
             Please fix the arguments and try again.",
            tool.name(),
            errors.join("\n")
        ))
    }
}

/// Log a tool call at INFO level with a truncated preview of arguments.
pub fn log_tool_call(name: &str, arguments: &str) {
    let args_preview: String = arguments.chars().take(120).collect();
    info!(
        "[tool] {}({args_preview}{})",
        name,
        if arguments.len() > 120 { "..." } else { "" }
    );
    debug!("[tool] {name} full args ({} bytes)", arguments.len());
    trace!("[tool] {name} arguments: {arguments}");
}

/// Truncate a string to at most `max` bytes, appending a notice if trimmed.
pub fn truncate_result(s: String, max: usize) -> String {
    if s.len() > max {
        format!("{}...\n[truncated: {} bytes total]", &s[..max], s.len())
    } else {
        s
    }
}

/// Parse raw JSON arguments into a typed struct.
///
/// Returns a formatted error string suitable for returning directly from
/// [`Tool::execute`] — the LLM will see the error and self-correct.
///
/// # Example
///
/// ```ignore
/// fn execute(&self, arguments: &str) -> ToolFuture<'_> {
///     Box::pin(async move {
///         let args: MyArgs = match parse_tool_args(arguments) {
///             Ok(a) => a,
///             Err(e) => return e,
///         };
///         // ... use args
///     })
/// }
/// ```
pub fn parse_tool_args<T: serde::de::DeserializeOwned>(arguments: &str) -> Result<T, String> {
    serde_json::from_str(arguments).map_err(|e| {
        format!(
            "Error: invalid tool arguments: {e}. \
             Please provide valid JSON matching the tool's parameter schema."
        )
    })
}

/// Extract a string value from tool-call arguments JSON.
pub fn parse_string_arg(args: &serde_json::Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Extract an integer value from tool-call arguments JSON.
pub fn parse_int_arg(args: &serde_json::Value, key: &str) -> Option<i64> {
    args.get(key).and_then(|v| v.as_i64())
}

/// Extract a boolean value from tool-call arguments JSON.
pub fn parse_bool_arg(args: &serde_json::Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

// ── Pseudo-tools ───────────────────────────────────────────────────

/// A no-op scratchpad tool that gives the LLM a structured way to reason
/// between rounds. The input is returned unchanged. Think calls are captured
/// in events for logging.
pub struct ThinkTool;

/// Typed arguments for the `think` pseudo-tool.
#[derive(Deserialize, JsonSchema)]
pub struct ThinkArgs {
    /// Your step-by-step reasoning or analysis.
    pub reasoning: String,
}

impl Tool for ThinkTool {
    fn definition(&self) -> ToolDef {
        ToolDef::new(
            "think",
            "Use this tool to think through a problem step-by-step before \
             acting. Write your reasoning as the 'reasoning' argument. This is a \
             scratchpad — it does not perform any action. Use it when you need to \
             plan, analyze, or deliberate between tool calls.",
            crate::json_schema_for::<ThinkArgs>(),
        )
    }

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let arguments = arguments.to_string();
        Box::pin(async move {
            match serde_json::from_str::<ThinkArgs>(&arguments) {
                Ok(args) => args.reasoning,
                Err(_) => "[no reasoning provided]".into(),
            }
        })
    }
}

/// Status of a todo item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl fmt::Display for TodoStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TodoStatus::Pending => write!(f, "[ ]"),
            TodoStatus::InProgress => write!(f, "[~]"),
            TodoStatus::Completed => write!(f, "[x]"),
        }
    }
}

/// A single todo item.
#[derive(Debug, Clone)]
pub struct TodoItem {
    pub task: String,
    pub status: TodoStatus,
}

/// A persistent, mutable task checklist tool. The LLM can add, complete,
/// list, and remove tasks across rounds. Unlike the `think` tool (ephemeral
/// reasoning), the todo tool maintains state that accumulates over the run.
pub struct TodoTool {
    items: Mutex<Vec<TodoItem>>,
}

impl TodoTool {
    pub fn new() -> Self {
        Self {
            items: Mutex::new(Vec::new()),
        }
    }

    fn format_list(items: &[TodoItem]) -> String {
        if items.is_empty() {
            return "Todo list is empty.".into();
        }
        let mut out = String::from("Todo list:\n");
        for (i, item) in items.iter().enumerate() {
            out.push_str(&format!("  {}. {} {}\n", i + 1, item.status, item.task));
        }
        out
    }
}

impl Default for TodoTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Action for the todo tool.
#[derive(Deserialize, JsonSchema, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoAction {
    Add,
    Complete,
    InProgress,
    Remove,
    List,
}

/// Typed arguments for the `todo` pseudo-tool.
#[derive(Deserialize, JsonSchema)]
pub struct TodoArgs {
    /// The action to perform on the todo list.
    pub action: TodoAction,
    /// The task description (required for 'add').
    #[serde(default)]
    pub task: Option<String>,
    /// The task number (1-indexed, for 'complete'/'in_progress'/'remove').
    #[serde(default)]
    pub number: Option<i64>,
}

impl Tool for TodoTool {
    fn definition(&self) -> ToolDef {
        ToolDef::new(
            "todo",
            "Manage a persistent task checklist across rounds. Use this to \
             track what's done, what's in progress, and what remains. Actions: \
             'add' (add a task), 'complete' (mark task done by number), \
             'in_progress' (mark task as in-progress by number), \
             'remove' (remove task by number), 'list' (show all tasks).",
            crate::json_schema_for::<TodoArgs>(),
        )
    }

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let arguments = arguments.to_string();

        Box::pin(async move {
            let parsed: TodoArgs = match serde_json::from_str(&arguments) {
                Ok(a) => a,
                Err(e) => {
                    return format!(
                        "Error: invalid arguments: {e}. Use: add, complete, in_progress, remove, list."
                    );
                }
            };

            let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner());

            match parsed.action {
                TodoAction::Add => {
                    let task = match parsed.task {
                        Some(t) if !t.is_empty() => t,
                        _ => return "Error: 'task' is required for 'add' action.".into(),
                    };
                    items.push(TodoItem {
                        task,
                        status: TodoStatus::Pending,
                    });
                    Self::format_list(&items)
                }
                TodoAction::Complete | TodoAction::InProgress | TodoAction::Remove => {
                    let idx = match parsed.number {
                        Some(n) if n >= 1 && (n as usize) <= items.len() => (n - 1) as usize,
                        _ => {
                            return format!(
                                "Error: invalid task number. {}",
                                Self::format_list(&items)
                            );
                        }
                    };
                    match parsed.action {
                        TodoAction::Complete => items[idx].status = TodoStatus::Completed,
                        TodoAction::InProgress => items[idx].status = TodoStatus::InProgress,
                        TodoAction::Remove => {
                            items.remove(idx);
                        }
                        _ => unreachable!(),
                    }
                    Self::format_list(&items)
                }
                TodoAction::List => Self::format_list(&items),
            }
        })
    }
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoTool;

    impl Tool for EchoTool {
        fn definition(&self) -> ToolDef {
            ToolDef::new(
                "echo",
                "Echo the input",
                serde_json::json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"]
                }),
            )
        }

        fn execute(&self, arguments: &str) -> ToolFuture<'_> {
            let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
            let result = parse_string_arg(&args, "text").unwrap_or_else(|| "Error: no text".into());
            Box::pin(async move { result })
        }
    }

    struct FailTool;

    impl Tool for FailTool {
        fn definition(&self) -> ToolDef {
            ToolDef::new(
                "fail",
                "Always fails",
                serde_json::json!({"type": "object", "properties": {}}),
            )
        }

        fn execute(&self, _arguments: &str) -> ToolFuture<'_> {
            Box::pin(async { "Error: intentional failure".into() })
        }
    }

    #[test]
    fn tool_name_from_definition() {
        let tool = EchoTool;
        assert_eq!(tool.name(), "echo");
    }

    #[test]
    fn toolset_register_and_definitions() {
        let set = ToolSet::new().with(EchoTool).with(FailTool);
        assert_eq!(set.len(), 2);

        let defs = set.definitions();
        let names: Vec<String> = defs.iter().map(|d| d.function.name.clone()).collect();
        assert!(names.contains(&"echo".to_string()));
        assert!(names.contains(&"fail".to_string()));
    }

    #[tokio::test]
    async fn toolset_execute_known_tool() {
        let set = ToolSet::new().with(EchoTool);
        let result = set.execute("echo", r#"{"text": "hello"}"#).await;
        assert_eq!(result, "hello");
    }

    #[tokio::test]
    async fn toolset_execute_unknown_tool() {
        let set = ToolSet::new().with(EchoTool);
        let result = set.execute("nonexistent", "{}").await;
        assert!(result.contains("unknown tool"));
    }

    #[tokio::test]
    async fn toolset_truncates_long_results() {
        struct BigTool;
        impl Tool for BigTool {
            fn definition(&self) -> ToolDef {
                ToolDef::new(
                    "big",
                    "Returns a big result",
                    serde_json::json!({"type": "object", "properties": {}}),
                )
            }
            fn execute(&self, _arguments: &str) -> ToolFuture<'_> {
                Box::pin(async { "a".repeat(200) })
            }
        }

        let set = ToolSet::new().with_max_result_bytes(50).with(BigTool);
        let result = set.execute("big", "{}").await;
        assert!(result.contains("[truncated: 200 bytes total]"));
    }

    #[test]
    fn truncate_short_unchanged() {
        assert_eq!(truncate_result("hello".into(), 100), "hello");
    }

    #[test]
    fn truncate_long_is_cut() {
        let s = "a".repeat(200);
        let result = truncate_result(s, 50);
        assert!(result.starts_with(&"a".repeat(50)));
        assert!(result.contains("[truncated: 200 bytes total]"));
    }

    #[test]
    fn parse_helpers() {
        let args = serde_json::json!({"name": "test", "count": 42, "verbose": true});
        assert_eq!(parse_string_arg(&args, "name"), Some("test".into()));
        assert_eq!(parse_int_arg(&args, "count"), Some(42));
        assert_eq!(parse_bool_arg(&args, "verbose"), Some(true));
        assert_eq!(parse_string_arg(&args, "missing"), None);
    }

    #[test]
    fn with_common_tools_registers_all() {
        let set = ToolSet::new().with_common_tools("/tmp");
        // 5 common tools + think + todo = 7
        assert_eq!(set.len(), 7);

        let defs = set.definitions();
        let names: Vec<String> = defs.iter().map(|d| d.function.name.clone()).collect();
        assert!(names.contains(&"read_file".to_string()));
        assert!(names.contains(&"list_files".to_string()));
        assert!(names.contains(&"grep".to_string()));
        assert!(names.contains(&"find_files".to_string()));
        assert!(names.contains(&"shell".to_string()));
        assert!(names.contains(&"think".to_string()));
        assert!(names.contains(&"todo".to_string()));
    }

    #[test]
    fn with_common_tools_inherits_max_result_bytes() {
        let set = ToolSet::new()
            .with_max_result_bytes(5000)
            .with_common_tools("/tmp");
        assert_eq!(set.len(), 7);
        // The ToolSet's own max_result_bytes is set.
        assert_eq!(set.max_result_bytes, 5000);
    }

    #[test]
    fn with_common_tools_composable_with_custom_tools() {
        let set = ToolSet::new().with_common_tools("/tmp").with(EchoTool);
        assert_eq!(set.len(), 8);
    }

    #[test]
    fn with_if_true_registers_tool() {
        let set = ToolSet::new().with_if(true, EchoTool);
        assert_eq!(set.len(), 1);
        assert!(set.definitions().iter().any(|d| d.function.name == "echo"));
    }

    #[test]
    fn with_if_false_skips_tool() {
        let set = ToolSet::new().with_if(false, EchoTool);
        assert_eq!(set.len(), 0);
    }

    #[tokio::test]
    async fn disabled_tool_returns_error() {
        let def = ToolDef::new(
            "my_tool",
            "A tool that is disabled",
            serde_json::json!({"type": "object", "properties": {}}),
        );
        let tool = DisabledTool::new(def, "Feature not enabled. Pass --enable to turn on.");

        assert_eq!(tool.definition().function.name, "my_tool");
        assert!(!tool.is_mutation());

        let result = tool.execute("{}").await;
        assert_eq!(
            result,
            "Error: Feature not enabled. Pass --enable to turn on."
        );
    }

    #[test]
    fn disabled_tool_in_toolset() {
        let def = ToolDef::new("gated", "Gated tool", serde_json::json!({}));
        let set = ToolSet::new().with(DisabledTool::new(def, "Not available"));
        assert_eq!(set.len(), 1);
        assert!(set.definitions().iter().any(|d| d.function.name == "gated"));
    }

    #[tokio::test]
    async fn disabled_tool_from_tool_preserves_definition() {
        let original = EchoTool;
        let disabled = DisabledTool::from_tool(&original, "Feature gated off");

        // Definition should match the original.
        assert_eq!(
            disabled.definition().function.name,
            original.definition().function.name
        );
        assert_eq!(
            disabled.definition().function.description,
            original.definition().function.description
        );

        // Execution should return the disabled error.
        let result = disabled.execute(r#"{"text": "hello"}"#).await;
        assert_eq!(result, "Error: Feature gated off");
    }

    // ── CommonToolsConfig ──────────────────────────────────────────

    #[test]
    fn common_tools_config_defaults() {
        let config = CommonToolsConfig::default();
        assert_eq!(config.grep_max_matches, 200);
        assert_eq!(config.find_max_results, 100);
        assert_eq!(config.shell_blocked_commands.len(), 3);
        assert!(config.shell_blocked_commands.contains(&"rm -rf /".into()));
    }

    #[test]
    fn common_tools_config_builder() {
        let config = CommonToolsConfig::default()
            .grep_max_matches(500)
            .find_max_results(50)
            .shell_block_command("dangerous-cmd");

        assert_eq!(config.grep_max_matches, 500);
        assert_eq!(config.find_max_results, 50);
        // Original 3 + 1 added.
        assert_eq!(config.shell_blocked_commands.len(), 4);
        assert!(
            config
                .shell_blocked_commands
                .contains(&"dangerous-cmd".into())
        );
    }

    #[test]
    fn common_tools_config_replace_blocked_commands() {
        let config = CommonToolsConfig::default().shell_blocked_commands(vec!["only-this".into()]);
        assert_eq!(config.shell_blocked_commands.len(), 1);
        assert_eq!(config.shell_blocked_commands[0], "only-this");
    }

    #[test]
    fn with_common_tools_configured_registers_all() {
        let config = CommonToolsConfig::default().grep_max_matches(500);
        let set = ToolSet::new().with_common_tools_configured("/tmp", config);
        // Same 7 tools as with_common_tools.
        assert_eq!(set.len(), 7);

        let defs = set.definitions();
        let names: Vec<String> = defs.iter().map(|d| d.function.name.clone()).collect();
        assert!(names.contains(&"read_file".to_string()));
        assert!(names.contains(&"grep".to_string()));
        assert!(names.contains(&"shell".to_string()));
        assert!(names.contains(&"think".to_string()));
        assert!(names.contains(&"todo".to_string()));
    }
}
