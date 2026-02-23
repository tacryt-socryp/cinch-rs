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
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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

/// Typed arguments for the `delegate_sub_agent` tool.
#[derive(Deserialize, JsonSchema, Debug)]
pub struct DelegateSubAgentArgs {
    /// Descriptive name for this sub-agent (for logging and result attribution).
    pub name: String,
    /// The task to delegate to the sub-agent.
    pub task: String,
    /// Optional model override (e.g. use a cheaper model for simple subtasks).
    #[serde(default)]
    pub model: Option<String>,
    /// Maximum rounds for the sub-agent (default: 10).
    #[serde(default = "default_max_rounds")]
    pub max_rounds: u32,
    /// Maximum result characters returned to the parent (default: 4000).
    #[serde(default = "default_max_result_chars")]
    pub max_result_chars: usize,
    /// Whether the sub-agent should plan before executing (default: false).
    /// Enable for complex subtasks that benefit from a read-only exploration
    /// phase before taking action. Most focused subtasks should skip planning.
    #[serde(default)]
    pub plan: bool,
}

fn default_max_rounds() -> u32 {
    10
}

fn default_max_result_chars() -> usize {
    4000
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
            "Delegate a subtask to a child agent that runs in its own isolated \
             context window. The child agent has access to the same tools and returns a \
             compact result. Use this to decompose complex tasks, parallelize independent \
             subtasks (multiple delegate_sub_agent calls in one round run concurrently), \
             or offload work that would consume too much context. The child's full \
             conversation is not visible to you — only the result summary. Set plan=true \
             for complex subtasks that benefit from a planning phase before execution.",
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
                         Optional: model (string), max_rounds (integer), max_result_chars (integer)."
                    );
                }
            };

            // Check depth limit.
            if !shared.can_spawn_child() {
                return format!(
                    "Error: maximum sub-agent depth ({}) reached. Cannot spawn child agent '{}'.",
                    shared.max_depth, args.name
                );
            }

            // Acquire concurrency permit — blocks if too many children are running.
            let _permit = match concurrency.acquire().await {
                Ok(p) => p,
                Err(_) => {
                    return format!(
                        "Error: concurrency semaphore closed. Cannot spawn child agent '{}'.",
                        args.name
                    );
                }
            };

            let child_model = args.model.unwrap_or(parent_model);

            // Create child shared resources with incremented depth.
            let child_shared = match shared.child() {
                Some(s) => s,
                None => {
                    return format!(
                        "Error: cannot create child resources for agent '{}'. Depth limit reached.",
                        args.name
                    );
                }
            };

            info!(
                "Spawning sub-agent '{}' (depth={}, model={}, max_rounds={})",
                args.name, child_shared.depth, child_model, args.max_rounds
            );

            // Build a child harness config — disable checkpoint and memory prompt
            // in children since the parent handles cross-session concerns.
            // Plan-execute is off by default for sub-agents (the parent already
            // planned), but can be opted in for complex subtasks.
            let plan_execute = if args.plan {
                crate::agent::config::HarnessPlanExecuteConfig::default()
            } else {
                crate::agent::config::HarnessPlanExecuteConfig::disabled()
            };

            let child_config = HarnessConfig {
                model: child_model,
                max_rounds: args.max_rounds,
                max_tokens: 4096,
                temperature: 0.7,
                checkpoint: crate::agent::config::HarnessCheckpointConfig::disabled(),
                memory_prompt: None, // Sub-agents don't need persistent memory
                plan_execute,
                ..Default::default()
            };

            // Child messages: system prompt + user task.
            let child_messages = vec![
                Message::system(format!(
                    "You are a focused sub-agent named '{}'. Complete the following task \
                     and provide a concise result. Do not ask clarifying questions — \
                     do your best with the information given.",
                    args.name
                )),
                Message::user(args.task),
            ];

            // Run the child harness.
            let child_harness = Harness::new(&client, &tools, child_config)
                .with_event_handler(&NoopHandler)
                .with_shared_resources(child_shared);

            let child_result = child_harness.run(child_messages).await;

            match child_result {
                Ok(result) => {
                    let tokens_consumed =
                        result.total_prompt_tokens as u64 + result.total_completion_tokens as u64;

                    // Report tokens to the tree-wide budget.
                    shared.budget.acquire(tokens_consumed);

                    debug!(
                        "Sub-agent '{}' completed: rounds={}, tokens={}, finished={}",
                        args.name, result.rounds_used, tokens_consumed, result.finished
                    );

                    // Truncate output to max_result_chars.
                    let output = result.text();
                    let truncated = if output.len() > args.max_result_chars {
                        let mut s: String = output.chars().take(args.max_result_chars).collect();
                        s.push_str("\n[output truncated]");
                        s
                    } else {
                        output
                    };

                    let sub_result = SubAgentResult {
                        name: args.name,
                        output: truncated,
                        finished: result.finished,
                        rounds_used: result.rounds_used,
                        tokens_consumed,
                    };
                    sub_result.to_parent_result()
                }
                Err(e) => {
                    warn!("Sub-agent '{}' failed: {e}", args.name);
                    format!("[Sub-agent '{}' failed] Error: {e}", args.name)
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
}
