//! Recursive sub-agent delegation (REPL-native pattern).
//!
//! The root agent delegates tasks to child sub-agents, each with:
//! - Its own conversation messages (isolated context)
//! - Its own round counter and context budget
//! - Access to the shared ToolSet and HTTP client
//! - A token budget semaphore (shared tree-wide)
//!
//! The root agent only has delegation tools (sub_agent, think, todo).
//! Leaf agents have real tools. This prevents the root from wasting context
//! on raw tool results — it only sees compact task results.

use crate::agent::config::HarnessConfig;
use crate::agent::events::NoopHandler;
use crate::agent::harness::Harness;
use crate::tools::core::{Tool, ToolFuture, ToolSet};
use crate::{Message, OpenRouterClient, ToolDef};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Budget semaphore for tree-wide token accounting.
///
/// Ensures that the total tokens consumed across all agents (root + children)
/// never exceeds a global budget. Each agent "checks out" tokens before making
/// an API call and releases any unused tokens afterward.
#[derive(Debug)]
pub struct TokenBudgetSemaphore {
    remaining: AtomicU64,
    total: u64,
}

impl TokenBudgetSemaphore {
    /// Create a new semaphore with the given total budget.
    pub fn new(total_tokens: u64) -> Self {
        Self {
            remaining: AtomicU64::new(total_tokens),
            total: total_tokens,
        }
    }

    /// Try to acquire tokens from the budget. Returns the number of tokens
    /// actually acquired (may be less than requested if budget is low).
    pub fn acquire(&self, requested: u64) -> u64 {
        loop {
            let current = self.remaining.load(Ordering::Relaxed);
            let granted = requested.min(current);
            if granted == 0 {
                return 0;
            }
            if self
                .remaining
                .compare_exchange(
                    current,
                    current - granted,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
            {
                return granted;
            }
        }
    }

    /// Release unused tokens back to the budget.
    pub fn release(&self, tokens: u64) {
        self.remaining.fetch_add(tokens, Ordering::SeqCst);
    }

    /// Current remaining budget.
    pub fn remaining(&self) -> u64 {
        self.remaining.load(Ordering::Relaxed)
    }

    /// Total budget.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Fraction of budget consumed.
    pub fn usage_fraction(&self) -> f64 {
        let used = self.total - self.remaining();
        used as f64 / self.total as f64
    }
}

/// Configuration for a sub-agent invocation.
#[derive(Debug, Clone)]
pub struct SubAgentConfig {
    /// Descriptive name for this sub-agent (for logging/tracing).
    pub name: String,
    /// The task description passed to the sub-agent.
    pub task: String,
    /// Model to use (may differ from parent for cost optimization).
    pub model: Option<String>,
    /// Maximum rounds for the sub-agent.
    pub max_rounds: u32,
    /// Maximum tokens per response.
    pub max_tokens: u32,
    /// Tool names available to this sub-agent.
    pub allowed_tools: Option<Vec<String>>,
    /// Maximum result size returned to the parent.
    pub max_result_chars: usize,
}

impl Default for SubAgentConfig {
    fn default() -> Self {
        Self {
            name: "sub-agent".into(),
            task: String::new(),
            model: None,
            max_rounds: 10,
            max_tokens: 4096,
            allowed_tools: None,
            max_result_chars: 4000,
        }
    }
}

/// Result from a sub-agent execution.
#[derive(Debug)]
pub struct SubAgentResult {
    /// The sub-agent's name.
    pub name: String,
    /// Text output (truncated to max_result_chars).
    pub output: String,
    /// Whether the sub-agent finished naturally.
    pub finished: bool,
    /// Rounds used.
    pub rounds_used: u32,
    /// Tokens consumed (for budget accounting).
    pub tokens_consumed: u64,
}

impl SubAgentResult {
    /// Format as a concise result for the parent agent.
    pub fn to_parent_result(&self) -> String {
        let status = if self.finished {
            "completed"
        } else {
            "hit round limit"
        };
        format!(
            "[Sub-agent '{}' {}] (rounds: {}, tokens: {})\n{}",
            self.name, status, self.rounds_used, self.tokens_consumed, self.output,
        )
    }
}

/// Shared resources passed from parent to child agent.
#[derive(Debug, Clone)]
pub struct SharedResources {
    /// Tree-wide token budget.
    pub budget: Arc<TokenBudgetSemaphore>,
    /// Trace ID from the root agent (for correlated logging).
    pub root_trace_id: String,
    /// Current depth in the agent tree (0 = root).
    pub depth: u32,
    /// Maximum allowed depth.
    pub max_depth: u32,
}

impl SharedResources {
    pub fn new(total_budget: u64, trace_id: String) -> Self {
        Self {
            budget: Arc::new(TokenBudgetSemaphore::new(total_budget)),
            root_trace_id: trace_id,
            depth: 0,
            max_depth: 3,
        }
    }

    /// Create child resources with incremented depth.
    pub fn child(&self) -> Option<Self> {
        if self.depth >= self.max_depth {
            return None;
        }
        Some(Self {
            budget: Arc::clone(&self.budget),
            root_trace_id: self.root_trace_id.clone(),
            depth: self.depth + 1,
            max_depth: self.max_depth,
        })
    }

    pub fn can_spawn_child(&self) -> bool {
        self.depth < self.max_depth
    }
}

// ── DelegateSubAgentTool ────────────────────────────────────────────

/// Specialization type for sub-agents. Each type maps to pre-configured
/// settings (model, max_rounds, plan-execute, system prompt) so the LLM
/// doesn't need to understand internal tradeoffs.
#[derive(Deserialize, JsonSchema, Debug, Clone, Default)]
#[serde(rename_all = "snake_case")]
pub enum SubAgentType {
    /// Fast, read-only exploration. Uses read-only tools, rounds scale with thoroughness.
    Explore,
    /// General-purpose worker. Full tool access.
    #[default]
    Worker,
    /// Planning agent. Read-only tools, returns a structured plan. Does not make changes.
    Planner,
}

/// Controls the depth of exploration or planning for Explore agents.
#[derive(Deserialize, JsonSchema, Debug, Clone, Default)]
#[serde(rename_all = "snake_case")]
pub enum Thoroughness {
    /// Quick scan — 5 rounds.
    Quick,
    /// Moderate exploration — 10 rounds.
    #[default]
    Medium,
    /// Deep investigation — 20 rounds.
    Thorough,
}

/// Typed arguments for the `delegate_sub_agent` tool.
#[derive(Deserialize, JsonSchema, Debug)]
pub struct DelegateSubAgentArgs {
    /// Descriptive name for this sub-agent (for logging and result attribution).
    pub name: String,
    /// The task to delegate to the sub-agent.
    pub task: String,
    /// Agent specialization type. Defaults to "worker".
    #[serde(default)]
    pub agent_type: SubAgentType,
    /// Thoroughness level for explore agents. Ignored for other types.
    #[serde(default)]
    pub thoroughness: Thoroughness,
    /// Optional model override. If not set, inherits the parent's model.
    #[serde(default)]
    pub model: Option<String>,
    /// Optional max rounds override. If not set, determined by agent_type and thoroughness.
    #[serde(default)]
    pub max_rounds: Option<u32>,
    /// Maximum result characters returned to the parent (default: 4000).
    #[serde(default = "default_max_result_chars")]
    pub max_result_chars: usize,
    /// Optional plan-execute override. If not set, determined by agent_type.
    /// For worker type: set to true to enable a planning phase before execution.
    #[serde(default)]
    pub plan: Option<bool>,
    /// Optional context to inject into the sub-agent's system prompt.
    /// Use this to pass relevant findings, file contents, or decisions from
    /// the parent conversation so the child doesn't have to re-discover them.
    #[serde(default)]
    pub context: Option<String>,
}

fn default_max_result_chars() -> usize {
    4000
}

// ── System prompts per type ──────────────────────────────────────────

fn explore_system_prompt(name: &str) -> String {
    format!(
        "You are a focused exploration agent named '{name}'. Search the codebase to \
         answer the question. Use read-only tools (read_file, grep, list_dir, \
         find_files). Be thorough but concise in your findings. Do not ask clarifying \
         questions — do your best with the information given."
    )
}

fn worker_system_prompt(name: &str) -> String {
    format!(
        "You are a focused sub-agent named '{name}'. Complete the following task \
         and provide a concise result. Do not ask clarifying questions — \
         do your best with the information given."
    )
}

fn planner_system_prompt(name: &str) -> String {
    format!(
        "You are a planning agent named '{name}'. Analyze the task, explore the \
         codebase, and produce a structured implementation plan. Do not make \
         changes — only plan. Do not ask clarifying questions — do your best \
         with the information given."
    )
}

/// Build a plan-execute config that stays in planning mode for the entire run.
/// Used by Explore and Planner types to restrict tools to read-only.
fn plan_execute_read_only(max_rounds: u32) -> crate::agent::config::HarnessPlanExecuteConfig {
    use crate::agent::config::Toggle;
    use crate::agent::plan_execute::PlanExecuteConfig;

    Toggle {
        enabled: true,
        config: PlanExecuteConfig {
            // Set max_planning_rounds equal to the full round budget so the
            // agent never transitions to execution — it stays read-only.
            max_planning_rounds: max_rounds,
            ..PlanExecuteConfig::default()
        },
    }
}

// ── Shared config resolution ────────────────────────────────────────

/// Resolved sub-agent configuration derived from `DelegateSubAgentArgs`.
struct ResolvedConfig {
    model: String,
    max_rounds: u32,
    plan_execute: crate::agent::config::HarnessPlanExecuteConfig,
    system_prompt: String,
}

/// Map `DelegateSubAgentArgs` + parent model into concrete config values.
fn resolve_config(args: &DelegateSubAgentArgs, parent_model: &str) -> ResolvedConfig {
    match args.agent_type {
        SubAgentType::Explore => {
            let default_rounds = match args.thoroughness {
                Thoroughness::Quick => 5,
                Thoroughness::Medium => 10,
                Thoroughness::Thorough => 20,
            };
            let rounds = args.max_rounds.unwrap_or(default_rounds);
            ResolvedConfig {
                model: args.model.clone().unwrap_or_else(|| parent_model.into()),
                max_rounds: rounds,
                plan_execute: plan_execute_read_only(rounds),
                system_prompt: explore_system_prompt(&args.name),
            }
        }
        SubAgentType::Worker => {
            let rounds = args.max_rounds.unwrap_or(10);
            let pe = match args.plan {
                Some(true) => crate::agent::config::HarnessPlanExecuteConfig::default(),
                _ => crate::agent::config::HarnessPlanExecuteConfig::disabled(),
            };
            ResolvedConfig {
                model: args.model.clone().unwrap_or_else(|| parent_model.into()),
                max_rounds: rounds,
                plan_execute: pe,
                system_prompt: worker_system_prompt(&args.name),
            }
        }
        SubAgentType::Planner => {
            let rounds = args.max_rounds.unwrap_or(10);
            ResolvedConfig {
                model: args.model.clone().unwrap_or_else(|| parent_model.into()),
                max_rounds: rounds,
                plan_execute: plan_execute_read_only(rounds),
                system_prompt: planner_system_prompt(&args.name),
            }
        }
    }
}

/// Build the `HarnessConfig` for a child agent from resolved settings.
fn build_child_harness_config(resolved: ResolvedConfig) -> (HarnessConfig, String) {
    let system_prompt = resolved.system_prompt;
    let config = HarnessConfig {
        model: resolved.model,
        max_rounds: resolved.max_rounds,
        max_tokens: 4096,
        temperature: 0.7,
        session: crate::agent::config::HarnessSessionConfig::disabled(),
        memory_prompt: None,
        plan_execute: resolved.plan_execute,
        ..Default::default()
    };
    (config, system_prompt)
}

/// Build child messages with system prompt (+ optional parent context) and task.
fn build_child_messages(
    system_prompt: String,
    context: Option<&str>,
    task: String,
) -> Vec<Message> {
    let prompt = match context {
        Some(ctx) => format!("{system_prompt}\n\nContext from parent:\n{ctx}"),
        None => system_prompt,
    };
    vec![Message::system(prompt), Message::user(task)]
}

/// Truncate output to `max_chars`, appending a notice if truncated.
fn truncate_output(output: String, max_chars: usize) -> String {
    if output.len() > max_chars {
        let mut s: String = output.chars().take(max_chars).collect();
        s.push_str("\n[output truncated]");
        s
    } else {
        output
    }
}

/// A tool that spawns a child harness run for recursive sub-agent delegation.
///
/// When the LLM returns multiple `delegate_sub_agent` calls in a single
/// round, the harness's existing parallel tool execution (`join_all`)
/// naturally runs them concurrently — enabling batch parallelism.
///
/// The tool checks depth limits via `SharedResources::can_spawn_child()`
/// and enforces the tree-wide token budget via `TokenBudgetSemaphore`.
pub struct DelegateSubAgentTool {
    /// Shared resources from the parent (token budget, depth tracking).
    shared: SharedResources,
    /// The OpenRouter client for making LLM API calls.
    client: Arc<OpenRouterClient>,
    /// The tool set available to child agents.
    tools: Arc<ToolSet>,
    /// The parent's model (used as default for children).
    parent_model: String,
    /// Concurrency semaphore — limits how many children can run at once.
    concurrency: Arc<tokio::sync::Semaphore>,
}

impl DelegateSubAgentTool {
    /// Create a new delegation tool with access to the client and tools.
    ///
    /// - `shared`: Tree-wide resources (budget, depth).
    /// - `client`: The OpenRouter API client (shared via Arc).
    /// - `tools`: The tool set children can use (shared via Arc).
    /// - `parent_model`: Default model for children that don't override.
    pub fn new(
        shared: SharedResources,
        client: Arc<OpenRouterClient>,
        tools: Arc<ToolSet>,
        parent_model: String,
    ) -> Self {
        Self {
            shared,
            client,
            tools,
            parent_model,
            concurrency: Arc::new(tokio::sync::Semaphore::new(5)),
        }
    }
}

impl Tool for DelegateSubAgentTool {
    fn definition(&self) -> ToolDef {
        ToolDef::new(
            "delegate_sub_agent",
            "Delegate a subtask to a specialized child agent. Types:\n\
             - explore: Read-only codebase exploration (set thoroughness: quick/medium/thorough)\n\
             - worker (default): General-purpose execution with full tool access\n\
             - planner: Read-only planning that returns a structured implementation plan\n\
             All fields except name and task are optional. Use context to pass relevant \
             findings (file contents, decisions, error messages) so the child doesn't have \
             to re-discover them. Multiple delegate_sub_agent calls in one round run concurrently.",
            crate::json_schema_for::<DelegateSubAgentArgs>(),
        )
    }

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let arguments = arguments.to_string();
        let shared = self.shared.clone();
        let client = Arc::clone(&self.client);
        let tools = Arc::clone(&self.tools);
        let parent_model = self.parent_model.clone();
        let concurrency = Arc::clone(&self.concurrency);

        Box::pin(async move {
            let args: DelegateSubAgentArgs = match serde_json::from_str(&arguments) {
                Ok(a) => a,
                Err(e) => {
                    return format!(
                        "Error: invalid arguments for delegate_sub_agent: {e}. \
                         Required: name (string), task (string). \
                         Optional: agent_type, thoroughness, model, max_rounds, plan, \
                         max_result_chars, context."
                    );
                }
            };

            if !shared.can_spawn_child() {
                return format!(
                    "Error: maximum sub-agent depth ({}) reached. Cannot spawn child agent '{}'.",
                    shared.max_depth, args.name
                );
            }

            let _permit = match concurrency.acquire().await {
                Ok(p) => p,
                Err(_) => {
                    return format!(
                        "Error: concurrency semaphore closed. Cannot spawn child agent '{}'.",
                        args.name
                    );
                }
            };

            let child_shared = match shared.child() {
                Some(s) => s,
                None => {
                    return format!(
                        "Error: cannot create child resources for agent '{}'. Depth limit reached.",
                        args.name
                    );
                }
            };

            let resolved = resolve_config(&args, &parent_model);
            info!(
                "Spawning sub-agent '{}' (type={:?}, depth={}, model={}, max_rounds={})",
                args.name, args.agent_type, child_shared.depth, resolved.model, resolved.max_rounds
            );

            let (child_config, system_prompt) = build_child_harness_config(resolved);
            let child_messages =
                build_child_messages(system_prompt, args.context.as_deref(), args.task);

            let child_harness = Harness::new(&client, &tools, child_config)
                .with_event_handler(&NoopHandler)
                .with_shared_resources(child_shared);

            match child_harness.run(child_messages).await {
                Ok(result) => {
                    let tokens_consumed =
                        result.total_prompt_tokens + result.total_completion_tokens;
                    shared.budget.acquire(tokens_consumed);

                    debug!(
                        "Sub-agent '{}' completed: rounds={}, tokens={}, finished={}",
                        args.name, result.rounds_used, tokens_consumed, result.finished
                    );

                    let output = truncate_output(result.text(), args.max_result_chars);
                    SubAgentResult {
                        name: args.name,
                        output,
                        finished: result.finished,
                        rounds_used: result.rounds_used,
                        tokens_consumed,
                    }
                    .to_parent_result()
                }
                Err(e) => {
                    warn!("Sub-agent '{}' failed: {e}", args.name);
                    format!("[Sub-agent '{}' failed] Error: {e}", args.name)
                }
            }
        })
    }
}

// ── Background Agent Registry ───────────────────────────────────────

/// Lifecycle state of a background agent.
enum AgentState {
    /// The agent's Tokio task is running (or queued for a concurrency permit).
    Running(JoinHandle<Result<SubAgentResult, String>>),
    /// The result has already been collected by `check_agent`.
    Collected,
}

/// Metadata + handle for one background agent.
struct RegistryEntry {
    name: String,
    state: AgentState,
    started_at: Instant,
}

/// Thread-safe registry for background sub-agents.
///
/// Stores spawned Tokio task handles keyed by a monotonic ID (`bg-1`, `bg-2`,
/// ...). The [`SpawnBackgroundAgentTool`] inserts entries and the
/// [`CheckAgentTool`] extracts results.
///
/// On drop, all still-running tasks are aborted to prevent resource leaks.
pub struct BackgroundAgentRegistry {
    entries: std::sync::Mutex<HashMap<String, RegistryEntry>>,
    next_id: AtomicU64,
}

impl Default for BackgroundAgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BackgroundAgentRegistry {
    pub fn new() -> Self {
        Self {
            entries: std::sync::Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Register a newly-spawned background agent. Returns the assigned ID.
    fn register(&self, name: String, handle: JoinHandle<Result<SubAgentResult, String>>) -> String {
        let id = format!("bg-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        self.entries.lock().unwrap().insert(
            id.clone(),
            RegistryEntry {
                name,
                state: AgentState::Running(handle),
                started_at: Instant::now(),
            },
        );
        id
    }

    /// Format a status summary of all tracked agents.
    fn status_summary(&self) -> String {
        let entries = self.entries.lock().unwrap();
        if entries.is_empty() {
            return "No background agents.".into();
        }
        let mut lines = Vec::with_capacity(entries.len());
        for (id, entry) in entries.iter() {
            let status = match &entry.state {
                AgentState::Running(h) if h.is_finished() => "finished (uncollected)",
                AgentState::Running(_) => "running",
                AgentState::Collected => "collected",
            };
            let elapsed = entry.started_at.elapsed().as_secs_f64();
            lines.push(format!(
                "  {id}: '{}' — {status} ({elapsed:.1}s)",
                entry.name
            ));
        }
        lines.join("\n")
    }
}

impl Drop for BackgroundAgentRegistry {
    fn drop(&mut self) {
        for (_, entry) in self.entries.get_mut().unwrap().drain() {
            if let AgentState::Running(handle) = entry.state {
                handle.abort();
            }
        }
    }
}

// ── SpawnBackgroundAgentTool ────────────────────────────────────────

/// Tool that spawns a sub-agent as a background Tokio task and returns
/// immediately with an ID. The LLM can continue working and later call
/// `check_agent` to collect the result.
pub struct SpawnBackgroundAgentTool {
    shared: SharedResources,
    client: Arc<OpenRouterClient>,
    tools: Arc<ToolSet>,
    parent_model: String,
    registry: Arc<BackgroundAgentRegistry>,
    concurrency: Arc<tokio::sync::Semaphore>,
}

impl SpawnBackgroundAgentTool {
    pub fn new(
        shared: SharedResources,
        client: Arc<OpenRouterClient>,
        tools: Arc<ToolSet>,
        parent_model: String,
        registry: Arc<BackgroundAgentRegistry>,
    ) -> Self {
        Self {
            shared,
            client,
            tools,
            parent_model,
            registry,
            concurrency: Arc::new(tokio::sync::Semaphore::new(5)),
        }
    }
}

impl Tool for SpawnBackgroundAgentTool {
    fn definition(&self) -> ToolDef {
        ToolDef::new(
            "spawn_background_agent",
            "Spawn a sub-agent as a background task and return immediately with an agent ID. \
             The agent runs concurrently while you continue working. Use check_agent with the \
             returned ID to poll status or collect the result. Accepts the same arguments as \
             delegate_sub_agent (name, task, agent_type, thoroughness, etc.).",
            crate::json_schema_for::<DelegateSubAgentArgs>(),
        )
    }

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        // Clone all Arc/owned data into the future — the future itself is
        // short-lived (returns the ID), but the spawned task lives longer.
        let arguments = arguments.to_string();
        let shared = self.shared.clone();
        let client = Arc::clone(&self.client);
        let tools = Arc::clone(&self.tools);
        let parent_model = self.parent_model.clone();
        let registry = Arc::clone(&self.registry);
        let concurrency = Arc::clone(&self.concurrency);

        Box::pin(async move {
            let args: DelegateSubAgentArgs = match serde_json::from_str(&arguments) {
                Ok(a) => a,
                Err(e) => {
                    return format!(
                        "Error: invalid arguments for spawn_background_agent: {e}. \
                         Required: name (string), task (string)."
                    );
                }
            };

            if !shared.can_spawn_child() {
                return format!(
                    "Error: maximum sub-agent depth ({}) reached. Cannot spawn '{}'.",
                    shared.max_depth, args.name
                );
            }

            let child_shared = match shared.child() {
                Some(s) => s,
                None => {
                    return format!(
                        "Error: cannot create child resources for '{}'. Depth limit reached.",
                        args.name
                    );
                }
            };

            let resolved = resolve_config(&args, &parent_model);
            let agent_name = args.name.clone();
            let max_result_chars = args.max_result_chars;

            info!(
                "Spawning background agent '{}' (type={:?}, model={}, max_rounds={})",
                args.name, args.agent_type, resolved.model, resolved.max_rounds
            );

            let (child_config, system_prompt) = build_child_harness_config(resolved);
            let child_messages =
                build_child_messages(system_prompt, args.context.as_deref(), args.task);

            // Spawn the agent as an independent Tokio task. The concurrency
            // permit is acquired *inside* the task so spawn returns immediately
            // even if all slots are busy — the task queues until a slot opens.
            let budget = Arc::clone(&shared.budget);
            let task_name = agent_name.clone();
            let handle = tokio::spawn(async move {
                let _permit = concurrency
                    .acquire()
                    .await
                    .map_err(|_| format!("concurrency semaphore closed for '{task_name}'"))?;

                let handler = NoopHandler;
                let child_harness = Harness::new(&client, &tools, child_config)
                    .with_event_handler(&handler)
                    .with_shared_resources(child_shared);

                let result = child_harness
                    .run(child_messages)
                    .await
                    .map_err(|e| e.to_string())?;

                let tokens_consumed =
                    result.total_prompt_tokens as u64 + result.total_completion_tokens as u64;
                budget.acquire(tokens_consumed);

                let output = truncate_output(result.text(), max_result_chars);
                Ok(SubAgentResult {
                    name: task_name,
                    output,
                    finished: result.finished,
                    rounds_used: result.rounds_used,
                    tokens_consumed,
                })
            });

            let agent_id = registry.register(agent_name.clone(), handle);
            format!(
                "Background agent '{agent_name}' spawned with ID '{agent_id}'. \
                 Use check_agent to collect the result when ready."
            )
        })
    }
}

// ── CheckAgentTool ──────────────────────────────────────────────────

/// Arguments for the `check_agent` tool.
#[derive(Deserialize, JsonSchema, Debug)]
pub struct CheckAgentArgs {
    /// The agent ID returned by `spawn_background_agent` (e.g. "bg-1").
    pub agent_id: String,
    /// If true (default), block until the agent finishes. If false, return
    /// immediately with the current status.
    #[serde(default = "default_true")]
    pub block: bool,
}

fn default_true() -> bool {
    true
}

/// Tool that checks on or collects the result of a background sub-agent.
pub struct CheckAgentTool {
    registry: Arc<BackgroundAgentRegistry>,
}

impl CheckAgentTool {
    pub fn new(registry: Arc<BackgroundAgentRegistry>) -> Self {
        Self { registry }
    }
}

impl Tool for CheckAgentTool {
    fn definition(&self) -> ToolDef {
        ToolDef::new(
            "check_agent",
            "Check the status of a background agent or collect its result. \
             With block=true (default), waits for the agent to finish and returns \
             the result. With block=false, returns immediately with the current status. \
             Pass agent_id=\"*\" to see a summary of all background agents.",
            crate::json_schema_for::<CheckAgentArgs>(),
        )
    }

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let arguments = arguments.to_string();
        let registry = Arc::clone(&self.registry);

        Box::pin(async move {
            let args: CheckAgentArgs = match serde_json::from_str(&arguments) {
                Ok(a) => a,
                Err(e) => {
                    return format!(
                        "Error: invalid arguments for check_agent: {e}. \
                         Required: agent_id (string). Optional: block (bool, default true)."
                    );
                }
            };

            // Wildcard: list all agents.
            if args.agent_id == "*" {
                return registry.status_summary();
            }

            // Extract the JoinHandle from the registry (lock held briefly).
            let handle = {
                let mut entries = registry.entries.lock().unwrap();
                let entry = match entries.get_mut(&args.agent_id) {
                    Some(e) => e,
                    None => {
                        return format!(
                            "Error: unknown agent ID '{}'. Use agent_id=\"*\" to list all agents.",
                            args.agent_id
                        );
                    }
                };

                match &entry.state {
                    AgentState::Collected => {
                        return format!(
                            "Agent '{}' (id: {}) result was already collected.",
                            entry.name, args.agent_id
                        );
                    }
                    AgentState::Running(h) => {
                        if !h.is_finished() && !args.block {
                            let elapsed = entry.started_at.elapsed().as_secs_f64();
                            return format!(
                                "Agent '{}' (id: {}) is still running ({elapsed:.1}s elapsed).",
                                entry.name, args.agent_id
                            );
                        }
                        // Take the handle, transition to Collected.
                        match std::mem::replace(&mut entry.state, AgentState::Collected) {
                            AgentState::Running(h) => h,
                            _ => unreachable!(),
                        }
                    }
                }
            };
            // Lock released — safe to await.

            match handle.await {
                Ok(Ok(result)) => {
                    debug!(
                        "Background agent '{}' collected: rounds={}, tokens={}",
                        result.name, result.rounds_used, result.tokens_consumed
                    );
                    result.to_parent_result()
                }
                Ok(Err(e)) => format!("[Background agent '{}' failed] Error: {e}", args.agent_id),
                Err(join_err) => {
                    format!(
                        "[Background agent '{}' panicked] Error: {join_err}",
                        args.agent_id
                    )
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_semaphore_basic() {
        let sem = TokenBudgetSemaphore::new(1000);
        assert_eq!(sem.remaining(), 1000);

        let acquired = sem.acquire(300);
        assert_eq!(acquired, 300);
        assert_eq!(sem.remaining(), 700);

        sem.release(100);
        assert_eq!(sem.remaining(), 800);
    }

    #[test]
    fn budget_semaphore_underflow_protection() {
        let sem = TokenBudgetSemaphore::new(100);
        let acquired = sem.acquire(200);
        assert_eq!(acquired, 100); // Only grants what's available.
        assert_eq!(sem.remaining(), 0);
    }

    #[test]
    fn shared_resources_depth_limit() {
        let shared = SharedResources::new(10000, "tr-test".into());
        assert!(shared.can_spawn_child());

        let child = shared.child().unwrap();
        assert_eq!(child.depth, 1);

        let grandchild = child.child().unwrap();
        assert_eq!(grandchild.depth, 2);

        let great = grandchild.child().unwrap();
        assert_eq!(great.depth, 3);
        assert!(!great.can_spawn_child());

        // Can't go deeper.
        assert!(great.child().is_none());
    }

    #[test]
    fn sub_agent_result_format() {
        let result = SubAgentResult {
            name: "analyzer".into(),
            output: "Found 3 issues.".into(),
            finished: true,
            rounds_used: 5,
            tokens_consumed: 2000,
        };
        let formatted = result.to_parent_result();
        assert!(formatted.contains("analyzer"));
        assert!(formatted.contains("completed"));
        assert!(formatted.contains("Found 3 issues."));
    }

    #[test]
    fn usage_fraction() {
        let sem = TokenBudgetSemaphore::new(1000);
        sem.acquire(250);
        assert!((sem.usage_fraction() - 0.25).abs() < 0.01);
    }

    // ── SubAgentType & Thoroughness tests ──────────────────────────

    #[test]
    fn sub_agent_type_default_is_worker() {
        let t: SubAgentType = Default::default();
        assert!(matches!(t, SubAgentType::Worker));
    }

    #[test]
    fn thoroughness_default_is_medium() {
        let t: Thoroughness = Default::default();
        assert!(matches!(t, Thoroughness::Medium));
    }

    #[test]
    fn sub_agent_type_deserialize_variants() {
        let explore: SubAgentType = serde_json::from_str(r#""explore""#).unwrap();
        assert!(matches!(explore, SubAgentType::Explore));

        let worker: SubAgentType = serde_json::from_str(r#""worker""#).unwrap();
        assert!(matches!(worker, SubAgentType::Worker));

        let planner: SubAgentType = serde_json::from_str(r#""planner""#).unwrap();
        assert!(matches!(planner, SubAgentType::Planner));
    }

    #[test]
    fn thoroughness_deserialize_variants() {
        let quick: Thoroughness = serde_json::from_str(r#""quick""#).unwrap();
        assert!(matches!(quick, Thoroughness::Quick));

        let medium: Thoroughness = serde_json::from_str(r#""medium""#).unwrap();
        assert!(matches!(medium, Thoroughness::Medium));

        let thorough: Thoroughness = serde_json::from_str(r#""thorough""#).unwrap();
        assert!(matches!(thorough, Thoroughness::Thorough));
    }

    #[test]
    fn args_minimal_deserialization() {
        // Only required fields: name and task. Everything else defaults.
        let json = r#"{"name": "test", "task": "do something"}"#;
        let args: DelegateSubAgentArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.name, "test");
        assert_eq!(args.task, "do something");
        assert!(matches!(args.agent_type, SubAgentType::Worker));
        assert!(matches!(args.thoroughness, Thoroughness::Medium));
        assert!(args.model.is_none());
        assert!(args.max_rounds.is_none());
        assert!(args.plan.is_none());
        assert!(args.context.is_none());
        assert_eq!(args.max_result_chars, 4000);
    }

    #[test]
    fn args_explore_with_thoroughness() {
        let json = r#"{
            "name": "explorer",
            "task": "find all tests",
            "agent_type": "explore",
            "thoroughness": "thorough"
        }"#;
        let args: DelegateSubAgentArgs = serde_json::from_str(json).unwrap();
        assert!(matches!(args.agent_type, SubAgentType::Explore));
        assert!(matches!(args.thoroughness, Thoroughness::Thorough));
    }

    #[test]
    fn args_with_overrides() {
        let json = r#"{
            "name": "custom",
            "task": "do it",
            "agent_type": "explore",
            "max_rounds": 15,
            "model": "openai/gpt-4o"
        }"#;
        let args: DelegateSubAgentArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.max_rounds, Some(15));
        assert_eq!(args.model.as_deref(), Some("openai/gpt-4o"));
    }

    #[test]
    fn system_prompt_explore_contains_name() {
        let prompt = explore_system_prompt("searcher");
        assert!(prompt.contains("searcher"));
        assert!(prompt.contains("exploration"));
        assert!(prompt.contains("read-only"));
    }

    #[test]
    fn system_prompt_worker_contains_name() {
        let prompt = worker_system_prompt("builder");
        assert!(prompt.contains("builder"));
        assert!(prompt.contains("sub-agent"));
    }

    #[test]
    fn system_prompt_planner_contains_name() {
        let prompt = planner_system_prompt("architect");
        assert!(prompt.contains("architect"));
        assert!(prompt.contains("planning"));
        assert!(prompt.contains("Do not make changes"));
    }

    #[test]
    fn plan_execute_read_only_stays_in_planning() {
        let pe = plan_execute_read_only(15);
        assert!(pe.enabled);
        assert_eq!(pe.config.max_planning_rounds, 15);
    }

    // ── resolve_config tests ────────────────────────────────────────

    #[test]
    fn resolve_config_explore_quick() {
        let args: DelegateSubAgentArgs = serde_json::from_str(
            r#"{"name":"x","task":"t","agent_type":"explore","thoroughness":"quick"}"#,
        )
        .unwrap();
        let rc = resolve_config(&args, "parent-model");
        assert_eq!(rc.model, "parent-model");
        assert_eq!(rc.max_rounds, 5);
        assert!(rc.plan_execute.enabled);
        assert!(rc.system_prompt.contains("exploration"));
    }

    #[test]
    fn resolve_config_explore_thorough() {
        let args: DelegateSubAgentArgs = serde_json::from_str(
            r#"{"name":"x","task":"t","agent_type":"explore","thoroughness":"thorough"}"#,
        )
        .unwrap();
        let rc = resolve_config(&args, "m");
        assert_eq!(rc.max_rounds, 20);
    }

    #[test]
    fn resolve_config_worker_defaults() {
        let args: DelegateSubAgentArgs =
            serde_json::from_str(r#"{"name":"w","task":"t"}"#).unwrap();
        let rc = resolve_config(&args, "pm");
        assert_eq!(rc.max_rounds, 10);
        assert!(!rc.plan_execute.enabled);
    }

    #[test]
    fn resolve_config_worker_with_plan() {
        let args: DelegateSubAgentArgs =
            serde_json::from_str(r#"{"name":"w","task":"t","plan":true}"#).unwrap();
        let rc = resolve_config(&args, "pm");
        assert!(rc.plan_execute.enabled);
    }

    #[test]
    fn resolve_config_planner() {
        let args: DelegateSubAgentArgs =
            serde_json::from_str(r#"{"name":"p","task":"t","agent_type":"planner"}"#).unwrap();
        let rc = resolve_config(&args, "pm");
        assert!(rc.plan_execute.enabled);
        assert!(rc.system_prompt.contains("planning"));
    }

    #[test]
    fn resolve_config_model_override() {
        let args: DelegateSubAgentArgs =
            serde_json::from_str(r#"{"name":"x","task":"t","model":"custom/m"}"#).unwrap();
        let rc = resolve_config(&args, "parent");
        assert_eq!(rc.model, "custom/m");
    }

    #[test]
    fn resolve_config_max_rounds_override() {
        let args: DelegateSubAgentArgs = serde_json::from_str(
            r#"{"name":"x","task":"t","agent_type":"explore","thoroughness":"quick","max_rounds":99}"#,
        )
        .unwrap();
        let rc = resolve_config(&args, "m");
        assert_eq!(rc.max_rounds, 99); // Override beats thoroughness default of 5
    }

    // ── Helper function tests ───────────────────────────────────────

    #[test]
    fn build_child_messages_without_context() {
        let msgs = build_child_messages("sys".into(), None, "do it".into());
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn build_child_messages_with_context() {
        let msgs = build_child_messages("sys".into(), Some("ctx"), "do it".into());
        assert_eq!(msgs.len(), 2);
        let sys_content = msgs[0].content.as_deref().unwrap();
        assert!(sys_content.contains("Context from parent:\nctx"));
    }

    #[test]
    fn truncate_output_short() {
        let s = truncate_output("hello".into(), 100);
        assert_eq!(s, "hello");
    }

    #[test]
    fn truncate_output_long() {
        let s = truncate_output("abcdef".into(), 3);
        assert!(s.starts_with("abc"));
        assert!(s.contains("[output truncated]"));
    }

    // ── Background agent registry tests ─────────────────────────────

    #[tokio::test]
    async fn registry_ids_are_monotonic() {
        let reg = BackgroundAgentRegistry::new();
        let h1 = tokio::spawn(async {
            Ok::<_, String>(SubAgentResult {
                name: "a".into(),
                output: String::new(),
                finished: true,
                rounds_used: 0,
                tokens_consumed: 0,
            })
        });
        let h2 = tokio::spawn(async {
            Ok::<_, String>(SubAgentResult {
                name: "b".into(),
                output: String::new(),
                finished: true,
                rounds_used: 0,
                tokens_consumed: 0,
            })
        });
        let id1 = reg.register("a".into(), h1);
        let id2 = reg.register("b".into(), h2);
        assert_eq!(id1, "bg-1");
        assert_eq!(id2, "bg-2");
    }

    #[test]
    fn registry_status_summary_empty() {
        let reg = BackgroundAgentRegistry::new();
        assert_eq!(reg.status_summary(), "No background agents.");
    }

    #[tokio::test]
    async fn registry_drop_aborts_running_tasks() {
        let handle = tokio::spawn(async {
            // Long-running task that should be aborted.
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            Ok::<_, String>(SubAgentResult {
                name: "slow".into(),
                output: String::new(),
                finished: false,
                rounds_used: 0,
                tokens_consumed: 0,
            })
        });
        let reg = BackgroundAgentRegistry::new();
        let _id = reg.register("slow".into(), handle);
        drop(reg); // Should abort the task without hanging.
    }

    #[test]
    fn check_agent_args_defaults() {
        let args: CheckAgentArgs = serde_json::from_str(r#"{"agent_id": "bg-1"}"#).unwrap();
        assert_eq!(args.agent_id, "bg-1");
        assert!(args.block); // default true
    }

    #[test]
    fn check_agent_args_non_blocking() {
        let args: CheckAgentArgs =
            serde_json::from_str(r#"{"agent_id": "bg-1", "block": false}"#).unwrap();
        assert!(!args.block);
    }

    #[tokio::test]
    async fn registry_collect_completed_task() {
        let reg = BackgroundAgentRegistry::new();
        let handle = tokio::spawn(async {
            Ok::<_, String>(SubAgentResult {
                name: "fast".into(),
                output: "done".into(),
                finished: true,
                rounds_used: 1,
                tokens_consumed: 100,
            })
        });
        let id = reg.register("fast".into(), handle);

        // Wait briefly for the task to complete.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Extract handle and collect.
        let handle = {
            let mut entries = reg.entries.lock().unwrap();
            let entry = entries.get_mut(&id).unwrap();
            match std::mem::replace(&mut entry.state, AgentState::Collected) {
                AgentState::Running(h) => h,
                _ => panic!("expected Running"),
            }
        };
        let result = handle.await.unwrap().unwrap();
        assert_eq!(result.name, "fast");
        assert_eq!(result.output, "done");
        assert!(result.finished);
    }
}
