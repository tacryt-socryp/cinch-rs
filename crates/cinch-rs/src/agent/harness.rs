//! Agent harness: a reusable agentic tool-use loop on top of the OpenRouter
//! chat completions API.
//!
//! The [`Harness`] sends messages + tool definitions to the LLM, executes
//! any returned tool calls via the [`ToolSet`], appends results, and repeats
//! until the LLM produces a text-only response or the round limit is reached.
//!
//! Advanced modules (eviction, summarization, model routing, tool filtering,
//! checkpointing, memory) are enabled by default with sensible defaults.
//! Callers observe the loop via [`EventHandler`] events.

use super::config::HarnessConfig;
use super::events::{EventHandler, HarnessEvent, HarnessResult};
use super::execution::{execute_and_record_tool_calls, save_round_checkpoint, send_round_request};
use crate::agent::checkpoint::CheckpointManager;
use crate::agent::plan_execute::{Phase, PlanExecuteConfig};
use crate::agent::profile::AgentProfile;
use crate::agent::sub_agent::SharedResources;
use crate::context::eviction::{self, ToolResultMeta};
use crate::context::summarizer::Summarizer;
use crate::context::{ContextBudget, ContextUsage};
use crate::tools::cache::ToolResultCache;
use crate::tools::core::ToolSet;
use crate::tools::filter::ToolFilter;
use crate::{Annotation, ChatRequest, Message, OpenRouterClient};
use tracing::{info, warn};

// ── Harness ────────────────────────────────────────────────────────

/// The agentic tool-use loop.
///
/// All advanced modules are active by default. No builder methods needed
/// for standard use:
///
/// ```ignore
/// let client = OpenRouterClient::new(api_key)?;
/// let tools = ToolSet::new().with(MyTool);
/// let config = HarnessConfig { model: "...".into(), ..Default::default() };
/// let messages = vec![Message::system("..."), Message::user("...")];
///
/// let result = Harness::new(&client, &tools, config)
///     .run(messages)
///     .await?;
///
/// println!("{}", result.text());
/// ```
///
/// # Lifetimes
///
/// `Harness<'a>` borrows the client, tools, and event handler by reference
/// to avoid unnecessary heap allocation. The references must all outlive the
/// `.run()` call. Bind everything to `let` bindings *before* building the
/// harness:
///
/// ```ignore
/// // Correct: handler lives long enough.
/// let handler = CompositeEventHandler::new().with(LoggingHandler);
/// let result = Harness::new(&client, &tools, config)
///     .with_event_handler(&handler)
///     .run(messages)
///     .await?;
///
/// // Wrong — temporary dropped before .run():
/// // let result = Harness::new(&client, &tools, config)
/// //     .with_event_handler(&CompositeEventHandler::new().with(LoggingHandler))
/// //     .run(messages)
/// //     .await?;
/// ```
pub struct Harness<'a> {
    client: &'a OpenRouterClient,
    tools: &'a ToolSet,
    config: HarnessConfig,
    context_budget: Option<ContextBudget>,
    event_handler: &'a dyn EventHandler,
    /// Optional stop signal — checked before each round. If it returns `true`,
    /// the loop stops early (e.g. TUI quit requested).
    stop_signal: Option<Box<dyn Fn() -> bool + Send + Sync + 'a>>,
    /// Optional shared resources for sub-agent delegation.
    shared_resources: Option<SharedResources>,
    /// Optional tool filter for dynamic tool selection.
    tool_filter: Option<ToolFilter>,
}

impl<'a> Harness<'a> {
    /// Create a new harness with the given client, tools, and config.
    pub fn new(client: &'a OpenRouterClient, tools: &'a ToolSet, config: HarnessConfig) -> Self {
        Self {
            client,
            tools,
            config,
            context_budget: None,
            event_handler: &super::events::NoopHandler,
            stop_signal: None,
            shared_resources: None,
            tool_filter: None,
        }
    }

    /// Attach a context budget tracker.
    pub fn with_context_budget(mut self, budget: ContextBudget) -> Self {
        self.context_budget = Some(budget);
        self
    }

    /// Attach an event handler.
    pub fn with_event_handler(mut self, handler: &'a dyn EventHandler) -> Self {
        self.event_handler = handler;
        self
    }

    /// Attach a stop signal. The closure is called before each round; if it
    /// returns `true` the loop stops early.
    pub fn with_stop_signal(mut self, signal: impl Fn() -> bool + Send + Sync + 'a) -> Self {
        self.stop_signal = Some(Box::new(signal));
        self
    }

    /// Attach shared resources for sub-agent delegation.
    pub fn with_shared_resources(mut self, resources: SharedResources) -> Self {
        self.shared_resources = Some(resources);
        self
    }

    /// Attach a tool filter for dynamic tool selection.
    pub fn with_tool_filter(mut self, filter: ToolFilter) -> Self {
        self.tool_filter = Some(filter);
        self
    }

    /// Conditionally attach a stop signal. If `condition` is `false`, this
    /// is a no-op and the harness runs without a stop signal. Avoids the
    /// `let mut harness = ...; if cond { harness = harness.with_stop_signal(...) }`
    /// reassignment pattern.
    pub fn with_stop_signal_if(
        mut self,
        condition: bool,
        signal: impl Fn() -> bool + Send + Sync + 'a,
    ) -> Self {
        if condition {
            self.stop_signal = Some(Box::new(signal));
        }
        self
    }

    /// Run the agentic loop.
    ///
    /// Takes ownership of the initial message list (system + user messages)
    /// and returns the complete [`HarnessResult`] when done.
    ///
    /// All enabled modules are automatically invoked at the appropriate
    /// points in the loop:
    /// - **Start of round:** Model routing, tool filtering, context
    ///   compaction check (eviction + summarization).
    /// - **After tool execution:** Record tool result metadata for eviction,
    ///   update tool filter usage counts, save checkpoint.
    /// - **On completion:** Clean up checkpoints on success, save profile.
    pub async fn run(mut self, mut messages: Vec<Message>) -> Result<HarnessResult, String> {
        let pricing = crate::api::tracing::pricing_for_model(&self.config.model);

        /// Maximum number of retries when the API returns an empty response
        /// (no content, no tool calls, near-zero tokens). Prevents infinite
        /// loops while giving transient API hiccups a chance to recover.
        const MAX_EMPTY_RESPONSE_RETRIES: u32 = 3;

        let mut acc = RunAccumulator {
            trace_id: crate::api::tracing::generate_trace_id(),
            text_output: Vec::new(),
            annotations: Vec::new(),
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            cost_tracker: crate::api::tracing::CostTracker::new(),
            rounds_used: 0,
            finished: false,
        };
        let mut empty_response_retries: u32 = 0;

        info!(
            "Harness run started: trace_id={}, model={}",
            acc.trace_id, self.config.model
        );

        // ── Initialize modules ──
        let mut modules = init_modules(&self.config);
        inject_prompt_extras(&self.config, &mut messages, &modules.agent_profile);

        // Auto-calibrate context budget from the system message if no
        // explicit budget was provided via `with_context_budget()`.
        if self.context_budget.is_none()
            && let Some(sys_content) = messages
                .iter()
                .find(|m| matches!(m.role, crate::MessageRole::System))
                .and_then(|m| m.content.as_deref())
        {
            self.context_budget = Some(ContextBudget::with_calibration(sys_content, None));
        }

        // Get tool definitions (may be filtered).
        let all_tool_defs = self.tools.definitions();
        let full_tool_defs = if let Some(ref filter) = self.tool_filter {
            let task_keywords = extract_task_keywords(&messages);
            let keyword_refs: Vec<&str> = task_keywords.iter().map(|s| s.as_str()).collect();
            filter.filter_for_task(&keyword_refs, &all_tool_defs)
        } else {
            all_tool_defs.clone()
        };

        // ── Plan-execute phase setup ──
        let mut phase = if self.config.plan_execute.enabled {
            Phase::Planning
        } else {
            Phase::Executing
        };

        // Build the planning-phase tool set: read-only tools + submit_plan.
        let planning_tool_defs = if self.config.plan_execute.enabled {
            let pe_config = &self.config.plan_execute.config;
            let mut defs = pe_config.filter_planning_tools(&full_tool_defs);
            defs.push(PlanExecuteConfig::submit_plan_tool_def());
            defs
        } else {
            Vec::new()
        };

        // Inject the planning prompt as a user message at the start.
        if self.config.plan_execute.enabled {
            messages.push(Message::user(
                &self.config.plan_execute.config.planning_prompt,
            ));
        }

        // Current tool definitions — starts as planning set or full set.
        let mut current_tool_defs = if phase == Phase::Planning {
            planning_tool_defs.clone()
        } else {
            full_tool_defs.clone()
        };

        let mut tools_option = non_empty_tools(&current_tool_defs);

        for round in 0..self.config.max_rounds {
            // Check stop signal.
            if let Some(ref signal) = self.stop_signal
                && signal()
            {
                info!("Stop signal received — ending agent loop");
                break;
            }

            acc.rounds_used = round + 1;

            // ── Model routing ──
            let model_for_round = self
                .config
                .routing
                .model_for_round(round, false)
                .to_string();
            if model_for_round != self.config.model {
                self.event_handler.on_event(&HarnessEvent::ModelRouted {
                    model: &model_for_round,
                    round: round + 1,
                });
            }

            // ── Context management (eviction + summarization) ──
            evict_if_needed(
                &self.config,
                &self.context_budget,
                &mut messages,
                &modules.tool_metas,
                round,
                self.event_handler,
            );
            summarize_if_needed(
                &self.config,
                self.client,
                &self.context_budget,
                &mut messages,
                &mut modules.summarizer,
                &mut modules.tool_metas,
                &model_for_round,
                self.event_handler,
            )
            .await;

            // Context budget tracking for round start event.
            let usage = self
                .context_budget
                .as_ref()
                .map(|b| b.estimate_usage(&messages))
                .unwrap_or(ContextUsage {
                    estimated_tokens: 0,
                    max_tokens: 0,
                    usage_pct: 0.0,
                });
            self.event_handler.on_event(&HarnessEvent::RoundStart {
                round: round + 1,
                max_rounds: self.config.max_rounds,
                context_usage: &usage,
            });

            // ── Send request ──
            let completion = send_round_request(
                &self.config,
                self.client,
                &messages,
                &model_for_round,
                &tools_option,
                self.event_handler,
            )
            .await?;

            // Track token usage and cost.
            if let Some(ref u) = completion.usage {
                let pt = u.prompt_tokens.unwrap_or(0);
                let ct = u.completion_tokens.unwrap_or(0);
                acc.total_prompt_tokens += pt;
                acc.total_completion_tokens += ct;
                acc.cost_tracker.record(pt, ct, &pricing);
                self.event_handler.on_event(&HarnessEvent::TokenUsage {
                    prompt_tokens: pt,
                    completion_tokens: ct,
                });
            }

            // Emit reasoning content if present.
            if let Some(ref reasoning) = completion.reasoning
                && !reasoning.is_empty()
            {
                self.event_handler
                    .on_event(&HarnessEvent::Reasoning(reasoning));
            }

            // Emit text.
            if let Some(ref text) = completion.content
                && !text.is_empty()
            {
                self.event_handler.on_event(&HarnessEvent::Text(text));
                acc.text_output.push(text.clone());
            }

            // ── Plan-execute: intercept submit_plan ──
            if phase == Phase::Planning
                && let Some(transition) = handle_plan_submission(
                    &self.config,
                    self.tools,
                    &completion,
                    &mut messages,
                    acc.rounds_used,
                    self.event_handler,
                )
                .await
            {
                phase = Phase::Executing;
                current_tool_defs = full_tool_defs.clone();
                tools_option = non_empty_tools(&current_tool_defs);
                if transition.should_continue {
                    continue;
                }
            }

            // Collect annotations (after all borrows of `completion` are done).
            acc.annotations.extend(completion.annotations);

            // If no tool calls at all, the agent is done — unless the API
            // returned an empty/malformed response (no content, no tool calls,
            // near-zero tokens). In that case, retry instead of exiting.
            //
            // Note: pseudo-tool-only rounds (think, todo) are NOT treated as
            // stop signals. The agent may call `todo(add)` or `think` while
            // planning and still intend to continue with real tool calls on the
            // next round. Pseudo-tool calls flow through the normal execution
            // path below and the agent gets another round.
            if completion.tool_calls.is_empty() {
                let has_content = completion.content.as_ref().is_some_and(|c| !c.is_empty());
                let completion_tokens = completion
                    .usage
                    .as_ref()
                    .and_then(|u| u.completion_tokens)
                    .unwrap_or(0);

                // An empty response is one where the API returned nothing
                // useful: no text, no tool calls, and essentially zero
                // completion tokens. This typically means an OpenRouter
                // transient failure that returned HTTP 200 with an empty body.
                if !has_content && completion_tokens == 0 {
                    empty_response_retries += 1;
                    if empty_response_retries <= MAX_EMPTY_RESPONSE_RETRIES {
                        self.event_handler.on_event(&HarnessEvent::EmptyResponse {
                            round: round + 1,
                            attempt: empty_response_retries,
                            max_retries: MAX_EMPTY_RESPONSE_RETRIES,
                        });
                        // Brief backoff before retrying (500ms * attempt).
                        tokio::time::sleep(std::time::Duration::from_millis(
                            500 * u64::from(empty_response_retries),
                        ))
                        .await;
                        continue;
                    }
                    // Exhausted retries — fall through to the normal exit.
                    warn!(
                        "Empty API response persisted after {MAX_EMPTY_RESPONSE_RETRIES} retries. \
                         Treating as agent completion."
                    );
                }

                acc.finished = true;
                self.event_handler.on_event(&HarnessEvent::Finished);
                break;
            }

            // Reset empty-response counter on any successful round with
            // real tool calls.
            empty_response_retries = 0;

            // ── Execute tool calls ──
            self.event_handler
                .on_event(&HarnessEvent::ToolCallsReceived {
                    round: round + 1,
                    count: completion.tool_calls.len(),
                });

            messages.push(Message::assistant_tool_calls(completion.tool_calls.clone()));

            execute_and_record_tool_calls(
                &self.config,
                self.tools,
                self.event_handler,
                &self.context_budget,
                &mut self.tool_filter,
                &mut messages,
                &mut modules,
                &completion.tool_calls,
                round,
            )
            .await;

            // ── Save checkpoint ──
            save_round_checkpoint(
                &modules.checkpoint_manager,
                &acc.trace_id,
                &messages,
                &acc.text_output,
                round,
                acc.total_prompt_tokens,
                acc.total_completion_tokens,
                acc.cost_tracker.estimated_cost_usd,
                self.event_handler,
            );
        }

        Ok(finalize_run(
            &self.config,
            acc,
            messages,
            &mut modules,
            self.event_handler,
        ))
    }
}

// ── Per-run module state ──────────────────────────────────────────

/// Mutable state for enabled modules during a single harness run.
///
/// Groups the optional subsystems (summarizer, checkpointing, caching,
/// profiling, eviction metadata) so the main `run()` loop doesn't need
/// to juggle five separate local variables.
pub(crate) struct ModuleState {
    pub(crate) summarizer: Option<Summarizer>,
    pub(crate) tool_metas: Vec<ToolResultMeta>,
    pub(crate) checkpoint_manager: Option<CheckpointManager>,
    pub(crate) tool_cache: Option<ToolResultCache>,
    pub(crate) agent_profile: Option<AgentProfile>,
}

/// Values accumulated across rounds during a harness run.
///
/// Groups the counters, text output, and completion flag so the main
/// `run()` loop doesn't need to juggle them as separate local variables.
struct RunAccumulator {
    trace_id: String,
    text_output: Vec<String>,
    annotations: Vec<Annotation>,
    total_prompt_tokens: u32,
    total_completion_tokens: u32,
    cost_tracker: crate::api::tracing::CostTracker,
    rounds_used: u32,
    finished: bool,
}

/// Initialize all optional modules from the harness configuration.
fn init_modules(config: &HarnessConfig) -> ModuleState {
    let summarizer = if config.summarizer.enabled {
        Some(Summarizer::new(config.summarizer.config.clone()))
    } else {
        None
    };

    let checkpoint_manager = if config.checkpoint.enabled
        && let Some(ref ckpt_config) = config.checkpoint.config
    {
        match CheckpointManager::new(ckpt_config.clone()) {
            Ok(mgr) => Some(mgr),
            Err(e) => {
                warn!(
                    "Failed to initialize checkpoint manager: {e}. Continuing without checkpointing."
                );
                None
            }
        }
    } else {
        None
    };

    let tool_cache = if config.cache.enabled {
        Some(ToolResultCache::new(config.cache.max_entries))
    } else {
        None
    };

    let agent_profile = if let Some(ref path) = config.profile.path {
        match AgentProfile::load_or_create(path, &config.profile.agent_id) {
            Ok(p) => Some(p),
            Err(e) => {
                warn!("Failed to load agent profile: {e}. Continuing without profile.");
                None
            }
        }
    } else {
        None
    };

    ModuleState {
        summarizer,
        tool_metas: Vec::new(),
        checkpoint_manager,
        tool_cache,
        agent_profile,
    }
}

/// Inject memory instructions and agent profile instructions into the system prompt.
fn inject_prompt_extras(
    config: &HarnessConfig,
    messages: &mut [Message],
    profile: &Option<AgentProfile>,
) {
    if let Some(ref mem_prompt) = config.memory_prompt
        && let Some(sys_msg) = messages
            .iter_mut()
            .find(|m| matches!(m.role, crate::MessageRole::System))
        && let Some(ref mut content) = sys_msg.content
    {
        content.push_str("\n\n");
        content.push_str(mem_prompt);
    }

    if let Some(profile) = profile
        && let Some(instructions) = profile.instructions_prompt_section()
        && let Some(sys_msg) = messages
            .iter_mut()
            .find(|m| matches!(m.role, crate::MessageRole::System))
        && let Some(ref mut content) = sys_msg.content
    {
        content.push_str(&instructions);
    }
}

/// Post-loop finalization: emit limit event, clean up checkpoints, save profile,
/// parse structured output, and build the final [`HarnessResult`].
fn finalize_run(
    config: &HarnessConfig,
    acc: RunAccumulator,
    messages: Vec<Message>,
    modules: &mut ModuleState,
    event_handler: &dyn EventHandler,
) -> HarnessResult {
    if !acc.finished {
        event_handler.on_event(&HarnessEvent::RoundLimitReached {
            max_rounds: config.max_rounds,
        });
    }

    // Clean up checkpoints on success.
    if acc.finished
        && let Some(ref ckpt_mgr) = modules.checkpoint_manager
        && let Err(e) = ckpt_mgr.cleanup(&acc.trace_id)
    {
        warn!("Failed to clean up checkpoints: {e}");
    }

    // Record run outcome in agent profile and save.
    if let Some(ref mut profile) = modules.agent_profile {
        profile.record_run(
            &config.model,
            acc.rounds_used,
            acc.total_prompt_tokens,
            acc.total_completion_tokens,
            acc.finished,
            acc.cost_tracker.estimated_cost_usd,
        );
        if let Some(ref path) = config.profile.path
            && let Err(e) = profile.save(path)
        {
            warn!("Failed to save agent profile: {e}");
        }
    }

    info!(
        "Harness run completed: trace_id={}, rounds={}, {}",
        acc.trace_id,
        acc.rounds_used,
        acc.cost_tracker.summary()
    );

    // Try to parse structured output from the last text output.
    let structured_output = if config.output_schema.is_some() {
        acc.text_output
            .last()
            .and_then(|text| serde_json::from_str(text).ok())
    } else {
        None
    };

    HarnessResult {
        trace_id: acc.trace_id,
        messages,
        text_output: acc.text_output,
        annotations: acc.annotations,
        total_prompt_tokens: acc.total_prompt_tokens,
        total_completion_tokens: acc.total_completion_tokens,
        rounds_used: acc.rounds_used,
        finished: acc.finished,
        estimated_cost_usd: acc.cost_tracker.estimated_cost_usd,
        structured_output,
    }
}

/// Evict old tool results when context usage exceeds 80%.
#[allow(clippy::ptr_arg)]
fn evict_if_needed(
    config: &HarnessConfig,
    budget: &Option<ContextBudget>,
    messages: &mut Vec<Message>,
    tool_metas: &[ToolResultMeta],
    round: u32,
    event_handler: &dyn EventHandler,
) {
    if !config.eviction.enabled || tool_metas.is_empty() {
        return;
    }
    let Some(budget) = budget else { return };
    let usage = budget.estimate_usage(messages);
    if usage.usage_pct < 0.80 {
        return;
    }
    let target_tokens = (budget.max_tokens() as f64 * 0.60) as usize;
    let freed = eviction::evict_tool_results(
        messages,
        tool_metas,
        round as usize,
        target_tokens,
        &config.eviction.config,
    );
    if freed > 0 {
        let evicted_count = tool_metas
            .iter()
            .filter(|m| {
                messages
                    .get(m.message_index)
                    .and_then(|msg| msg.content.as_ref())
                    .is_some_and(|c| c.starts_with(crate::context::eviction::EVICTED_PREFIX))
            })
            .count();
        event_handler.on_event(&HarnessEvent::Eviction {
            freed_chars: freed,
            evicted_count,
        });
    }
}

/// Summarize (compact) middle-zone messages when context usage is still over 80% after eviction.
#[allow(clippy::too_many_arguments)]
async fn summarize_if_needed(
    config: &HarnessConfig,
    client: &OpenRouterClient,
    budget: &Option<ContextBudget>,
    messages: &mut Vec<Message>,
    summarizer: &mut Option<Summarizer>,
    tool_metas: &mut Vec<ToolResultMeta>,
    model_for_round: &str,
    event_handler: &dyn EventHandler,
) {
    if !config.summarizer.enabled {
        return;
    }
    let Some(summ) = summarizer else {
        return;
    };
    let Some(budget) = budget else { return };
    let usage = budget.estimate_usage(messages);
    if usage.usage_pct < 0.80 {
        return;
    }

    let boundary = summ.boundary_index;
    let messages_len = messages.len();
    let keep_recent = config.keep_recent_messages;
    if messages_len <= keep_recent + boundary + 2 {
        return;
    }
    let end = messages_len.saturating_sub(keep_recent);
    let span = &messages[boundary..end];
    if span.is_empty() {
        return;
    }

    let (sys_prompt, user_prompt) = summ.build_summarization_request(span);
    let summary_model = summ.summary_model(model_for_round).to_string();

    let summary_request = ChatRequest {
        model: Some(summary_model),
        messages: vec![Message::system(&sys_prompt), Message::user(&user_prompt)],
        max_tokens: summ.config.max_summary_tokens,
        temperature: 0.3,
        ..Default::default()
    };

    match client.chat(&summary_request).await {
        Ok(completion) => {
            if let Some(ref summary_text) = completion.content {
                let summary_msg = Message::user(format!(
                    "<context_summary>\n{summary_text}\n</context_summary>"
                ));
                let ack_msg = Message::assistant_text(
                    "I've reviewed the context summary and will continue from where I left off.",
                );

                let drain_range = boundary..end;
                messages.drain(drain_range);
                messages.insert(boundary, ack_msg);
                messages.insert(boundary, summary_msg);

                summ.apply_summary(summary_text.clone(), boundary + 2);

                let compaction_number = summ.boundary_index; // proxy
                event_handler.on_event(&HarnessEvent::Compaction { compaction_number });

                // Reindex tool_metas after message removal.
                tool_metas.retain(|m| m.message_index < boundary || m.message_index >= end);
                let removed = end - boundary;
                let inserted = 2;
                for meta in tool_metas.iter_mut() {
                    if meta.message_index >= end {
                        meta.message_index = meta.message_index.saturating_sub(removed) + inserted;
                    }
                }
            }
        }
        Err(e) => {
            warn!("Summarization failed: {e}. Continuing without compaction.");
        }
    }
}

/// Whether the main loop should `continue` to the next round after a plan transition.
struct PlanTransition {
    should_continue: bool,
}

/// Check if the LLM submitted a plan during the planning phase, and if so,
/// handle the transition to execution phase.
///
/// Returns `Some(PlanTransition)` if the phase transitioned, `None` otherwise.
async fn handle_plan_submission(
    config: &HarnessConfig,
    tools: &ToolSet,
    completion: &crate::ChatCompletion,
    messages: &mut Vec<Message>,
    rounds_used: u32,
    event_handler: &dyn EventHandler,
) -> Option<PlanTransition> {
    let submit_idx = completion
        .tool_calls
        .iter()
        .position(|c| PlanExecuteConfig::is_plan_submission(&c.function.name));

    if let Some(idx) = submit_idx {
        let plan_summary = completion.tool_calls[idx].function.arguments.clone();
        let summary_text: String = serde_json::from_str::<serde_json::Value>(&plan_summary)
            .ok()
            .and_then(|v| v.get("summary").and_then(|s| s.as_str()).map(String::from))
            .unwrap_or_else(|| plan_summary.clone());

        messages.push(Message::assistant_tool_calls(completion.tool_calls.clone()));

        for call in &completion.tool_calls {
            if PlanExecuteConfig::is_plan_submission(&call.function.name) {
                messages.push(Message::tool_result(
                    &call.id,
                    format!(
                        "Plan accepted. Transitioning to execution phase.\n\nPlan summary: {summary_text}"
                    ),
                ));
            } else {
                let result = tools
                    .execute(&call.function.name, &call.function.arguments)
                    .await;
                messages.push(Message::tool_result(&call.id, result));
            }
        }

        event_handler.on_event(&HarnessEvent::PlanSubmitted {
            summary: &summary_text,
        });
        event_handler.on_event(&HarnessEvent::PhaseTransition {
            from: &Phase::Planning,
            to: &Phase::Executing,
        });

        messages.push(Message::user(&config.plan_execute.config.execution_prompt));

        return Some(PlanTransition {
            should_continue: true,
        });
    }

    // Check if planning phase exceeded its round budget.
    if rounds_used >= config.plan_execute.config.max_planning_rounds {
        info!(
            "Planning phase hit round limit ({}). Auto-transitioning to execution.",
            config.plan_execute.config.max_planning_rounds
        );
        event_handler.on_event(&HarnessEvent::PhaseTransition {
            from: &Phase::Planning,
            to: &Phase::Executing,
        });
        messages.push(Message::user(&config.plan_execute.config.execution_prompt));
        return Some(PlanTransition {
            should_continue: false,
        });
    }

    None
}

// ── Small helpers ──────────────────────────────────────────────────

/// Convert tool defs to `Option`, returning `None` if empty.
fn non_empty_tools(defs: &[crate::ToolDef]) -> Option<Vec<crate::ToolDef>> {
    if defs.is_empty() {
        None
    } else {
        Some(defs.to_vec())
    }
}

/// Extract simple keywords from the first user message for tool filtering.
fn extract_task_keywords(messages: &[Message]) -> Vec<String> {
    for msg in messages {
        if matches!(msg.role, crate::MessageRole::User)
            && let Some(ref content) = msg.content
        {
            // Simple keyword extraction: split on whitespace, take significant words.
            return content
                .split_whitespace()
                .filter(|w| w.len() > 3)
                .take(10)
                .map(|w| {
                    w.to_lowercase()
                        .trim_matches(|c: char| !c.is_alphanumeric())
                        .to_string()
                })
                .filter(|w| !w.is_empty())
                .collect();
        }
    }
    Vec::new()
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::config::*;
    use super::super::events::*;
    use super::super::execution::*;
    use super::*;

    #[test]
    fn harness_config_default() {
        let config = HarnessConfig::default();
        assert_eq!(config.max_rounds, 10);
        assert_eq!(config.max_tokens, 1024);
        assert!(config.temperature > 0.0);
        // Advanced modules are enabled by default.
        assert!(config.eviction.enabled);
        assert!(config.summarizer.enabled);
        assert!(config.checkpoint.enabled);
        assert!(config.cache.enabled);
        assert!(config.plan_execute.enabled);
        assert!(!config.streaming);
        assert!(config.approval_required_tools.is_empty());
    }

    #[test]
    fn harness_config_new_with_system_prompt() {
        let config = HarnessConfig::new("anthropic/claude-sonnet-4", "You are a helpful agent.");
        assert_eq!(config.model, "anthropic/claude-sonnet-4");
        assert_eq!(
            config.system_prompt.as_deref(),
            Some("You are a helpful agent.")
        );
    }

    #[test]
    fn harness_config_disable_modules() {
        let config = HarnessConfig {
            eviction: HarnessEvictionConfig::disabled(),
            summarizer: HarnessSummarizerConfig::disabled(),
            checkpoint: HarnessCheckpointConfig::disabled(),
            cache: HarnessCacheConfig::disabled(),
            plan_execute: HarnessPlanExecuteConfig::disabled(),
            ..Default::default()
        };
        assert!(!config.eviction.enabled);
        assert!(!config.summarizer.enabled);
        assert!(!config.checkpoint.enabled);
        assert!(!config.cache.enabled);
        assert!(!config.plan_execute.enabled);
    }

    #[test]
    fn harness_result_text_join() {
        let result = HarnessResult {
            trace_id: "tr-test".into(),
            messages: vec![],
            text_output: vec!["hello".into(), "world".into()],
            annotations: vec![],
            total_prompt_tokens: 100,
            total_completion_tokens: 50,
            rounds_used: 2,
            finished: true,
            estimated_cost_usd: 0.001,
            structured_output: None,
        };
        assert_eq!(result.text(), "hello\n\nworld");
        assert_eq!(result.total_tokens(), 150);
    }

    #[test]
    fn noop_handler_compiles() {
        let handler = super::super::events::NoopHandler;
        let usage = ContextUsage {
            estimated_tokens: 100,
            max_tokens: 200_000,
            usage_pct: 0.0005,
        };
        handler.on_event(&HarnessEvent::RoundStart {
            round: 1,
            max_rounds: 10,
            context_usage: &usage,
        });
        handler.on_event(&HarnessEvent::Finished);
    }

    #[test]
    fn logging_handler_compiles() {
        let handler = super::super::events::LoggingHandler;
        let usage = ContextUsage {
            estimated_tokens: 100,
            max_tokens: 200_000,
            usage_pct: 0.0005,
        };
        handler.on_event(&HarnessEvent::RoundStart {
            round: 1,
            max_rounds: 10,
            context_usage: &usage,
        });
        handler.on_event(&HarnessEvent::Text("hello"));
        handler.on_event(&HarnessEvent::Finished);
    }

    #[test]
    fn extract_keywords_from_user_message() {
        let messages = vec![
            Message::system("system prompt"),
            Message::user("Read the main.rs file and check for compilation errors"),
        ];
        let keywords = extract_task_keywords(&messages);
        assert!(!keywords.is_empty());
        assert!(keywords.iter().any(|k| k.contains("main")));
    }

    #[test]
    fn extract_keywords_empty_messages() {
        let messages: Vec<Message> = vec![];
        let keywords = extract_task_keywords(&messages);
        assert!(keywords.is_empty());
    }

    #[test]
    fn assemble_tool_calls_from_stream_basic() {
        use crate::api::streaming::StreamEvent;

        let events = vec![
            StreamEvent::ToolCallDelta {
                index: 0,
                id: Some("call_1".into()),
                name: Some("read_file".into()),
                arguments_delta: r#"{"pa"#.into(),
            },
            StreamEvent::ToolCallDelta {
                index: 0,
                id: None,
                name: None,
                arguments_delta: r#"th":"test.rs"}"#.into(),
            },
            StreamEvent::Done,
        ];

        let calls = assemble_tool_calls_from_stream(&events);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].function.name, "read_file");
        assert_eq!(calls[0].function.arguments, r#"{"path":"test.rs"}"#);
    }

    #[test]
    fn assemble_tool_calls_multiple_tools() {
        use crate::api::streaming::StreamEvent;

        let events = vec![
            StreamEvent::ToolCallDelta {
                index: 0,
                id: Some("call_1".into()),
                name: Some("read_file".into()),
                arguments_delta: r#"{"path":"a.rs"}"#.into(),
            },
            StreamEvent::ToolCallDelta {
                index: 1,
                id: Some("call_2".into()),
                name: Some("grep".into()),
                arguments_delta: r#"{"pattern":"foo"}"#.into(),
            },
            StreamEvent::Done,
        ];

        let calls = assemble_tool_calls_from_stream(&events);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function.name, "read_file");
        assert_eq!(calls[1].function.name, "grep");
    }

    #[test]
    fn event_response_variants() {
        let approve = EventResponse::Approve;
        let deny = EventResponse::Deny("not safe".into());
        let inject = EventResponse::InjectMessage("please clarify".into());

        // Just check they compile and debug format.
        assert!(format!("{:?}", approve).contains("Approve"));
        assert!(format!("{:?}", deny).contains("not safe"));
        assert!(format!("{:?}", inject).contains("please clarify"));
    }

    #[test]
    fn cache_config_default_and_disabled() {
        let default = HarnessCacheConfig::default();
        assert!(default.enabled);
        assert_eq!(default.max_entries, 100);
        assert_eq!(default.max_age_rounds, 10);

        let disabled = HarnessCacheConfig::disabled();
        assert!(!disabled.enabled);
    }

    // ── New API: builder methods ──────────────────────────────────

    #[test]
    fn harness_config_builder_methods() {
        let config = HarnessConfig::new("test-model", "system prompt")
            .with_max_rounds(20)
            .with_max_tokens(4096)
            .with_temperature(0.5)
            .with_streaming(true)
            .with_retries(3)
            .with_memory_prompt(None);

        assert_eq!(config.model, "test-model");
        assert_eq!(config.max_rounds, 20);
        assert_eq!(config.max_tokens, 4096);
        assert!((config.temperature - 0.5).abs() < f32::EPSILON);
        assert!(config.streaming);
        assert_eq!(config.retry.max_retries, 3);
        assert!(config.memory_prompt.is_none());
        // Internal modules stay enabled — not exposed via builder.
        assert!(config.eviction.enabled);
        assert!(config.checkpoint.enabled);
    }

    #[test]
    fn harness_config_planning_prompt_builder() {
        let config = HarnessConfig::new("test-model", "prompt")
            .with_planning_prompt("Custom planning instructions");
        assert_eq!(
            config.plan_execute.config.planning_prompt,
            "Custom planning instructions"
        );
        // Execution prompt should still be the default.
        assert!(!config.plan_execute.config.execution_prompt.is_empty());
    }

    #[test]
    fn harness_config_execution_prompt_builder() {
        let config = HarnessConfig::new("test-model", "prompt")
            .with_execution_prompt("Custom execution instructions");
        assert_eq!(
            config.plan_execute.config.execution_prompt,
            "Custom execution instructions"
        );
        // Planning prompt should still be the default.
        assert!(!config.plan_execute.config.planning_prompt.is_empty());
    }

    #[test]
    fn harness_config_both_prompts_builder() {
        let config = HarnessConfig::new("test-model", "prompt")
            .with_planning_prompt("Plan this")
            .with_execution_prompt("Execute this");
        assert_eq!(config.plan_execute.config.planning_prompt, "Plan this");
        assert_eq!(config.plan_execute.config.execution_prompt, "Execute this");
    }

    #[test]
    fn harness_config_builder_preserves_defaults() {
        let config = HarnessConfig::new("test-model", "prompt").with_max_rounds(5);
        // Other fields should still have defaults.
        assert_eq!(config.max_tokens, 1024);
        assert!(config.cache.enabled);
        assert!(config.plan_execute.enabled);
    }

    // ── New API: composite and fn event handlers ─────────────────

    #[test]
    fn fn_event_handler_receives_events() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let call_count = std::sync::Arc::new(AtomicU32::new(0));
        let count = call_count.clone();
        let handler = FnEventHandler::new(move |_event| {
            count.fetch_add(1, Ordering::SeqCst);
            None
        });

        handler.on_event(&HarnessEvent::Finished);
        handler.on_event(&HarnessEvent::Text("hello"));
        assert_eq!(call_count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn composite_handler_delegates_to_all() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let count_a = std::sync::Arc::new(AtomicU32::new(0));
        let count_b = std::sync::Arc::new(AtomicU32::new(0));

        let ca = count_a.clone();
        let cb = count_b.clone();
        let handler = CompositeEventHandler::new()
            .with(FnEventHandler::new(move |_| {
                ca.fetch_add(1, Ordering::SeqCst);
                None
            }))
            .with(FnEventHandler::new(move |_| {
                cb.fetch_add(1, Ordering::SeqCst);
                None
            }));

        handler.on_event(&HarnessEvent::Finished);
        assert_eq!(count_a.load(Ordering::SeqCst), 1);
        assert_eq!(count_b.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn composite_handler_with_if_true_includes() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let count = std::sync::Arc::new(AtomicU32::new(0));
        let c = count.clone();
        let handler = CompositeEventHandler::new().with_if(
            true,
            FnEventHandler::new(move |_| {
                c.fetch_add(1, Ordering::SeqCst);
                None
            }),
        );

        handler.on_event(&HarnessEvent::Finished);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn composite_handler_with_if_false_skips() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let count = std::sync::Arc::new(AtomicU32::new(0));
        let c = count.clone();
        let handler = CompositeEventHandler::new().with_if(
            false,
            FnEventHandler::new(move |_| {
                c.fetch_add(1, Ordering::SeqCst);
                None
            }),
        );

        handler.on_event(&HarnessEvent::Finished);
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn composite_handler_with_opt_some_includes() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let count = std::sync::Arc::new(AtomicU32::new(0));
        let c = count.clone();
        let handler = CompositeEventHandler::new().with_opt(Some(FnEventHandler::new(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
            None
        })));

        handler.on_event(&HarnessEvent::Finished);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn composite_handler_with_opt_none_skips() {
        let handler =
            CompositeEventHandler::new().with_opt(None::<FnEventHandler<fn(&HarnessEvent) -> _>>);

        // Just verify it compiles and doesn't panic.
        handler.on_event(&HarnessEvent::Finished);
    }

    #[test]
    fn composite_handler_returns_first_response() {
        let handler = CompositeEventHandler::new()
            .with(FnEventHandler::new(|_| None))
            .with(FnEventHandler::new(|_| {
                Some(EventResponse::Deny("blocked".into()))
            }))
            .with(FnEventHandler::new(|_| Some(EventResponse::Approve)));

        let response = handler.on_event(&HarnessEvent::Finished);
        assert!(matches!(response, Some(EventResponse::Deny(_))));
    }

    // ── ToolResultHandler::with_state() ─────────────────────────────

    #[test]
    fn stateful_tool_result_handler_shares_state() {
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct Counts {
            saves: u32,
            posts: u32,
        }

        let state = Arc::new(Mutex::new(Counts::default()));
        let handler = ToolResultHandler::with_state(state.clone())
            .on("save_draft", |s, _result| {
                s.saves += 1;
            })
            .on("post_tweet", |s, _result| {
                s.posts += 1;
            })
            .build();

        // Simulate tool result events.
        handler.on_event(&HarnessEvent::ToolResult {
            name: "save_draft",
            call_id: "1",
            result: "ok",
        });
        handler.on_event(&HarnessEvent::ToolResult {
            name: "save_draft",
            call_id: "2",
            result: "ok",
        });
        handler.on_event(&HarnessEvent::ToolResult {
            name: "post_tweet",
            call_id: "3",
            result: "ok",
        });
        // Non-matching tool should not fire.
        handler.on_event(&HarnessEvent::ToolResult {
            name: "other_tool",
            call_id: "4",
            result: "ok",
        });

        let s = state.lock().unwrap();
        assert_eq!(s.saves, 2);
        assert_eq!(s.posts, 1);
    }

    #[test]
    fn stateful_tool_result_handler_receives_result_string() {
        use std::sync::{Arc, Mutex};

        let captured = Arc::new(Mutex::new(Vec::<String>::new()));
        let handler = ToolResultHandler::with_state(captured.clone())
            .on("echo", |results, result| {
                results.push(result.to_string());
            })
            .build();

        handler.on_event(&HarnessEvent::ToolResult {
            name: "echo",
            call_id: "1",
            result: "hello world",
        });

        let results = captured.lock().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], "hello world");
    }

    #[test]
    fn stateful_tool_result_handler_ignores_non_tool_events() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::{Arc, Mutex};

        let count = Arc::new(AtomicU32::new(0));
        let state = Arc::new(Mutex::new(()));
        let c = count.clone();
        let handler = ToolResultHandler::with_state(state)
            .on("test", move |_s, _r| {
                c.fetch_add(1, Ordering::SeqCst);
            })
            .build();

        // These should not trigger the callback.
        handler.on_event(&HarnessEvent::Finished);
        handler.on_event(&HarnessEvent::Text("hello"));
        handler.on_event(&HarnessEvent::RoundLimitReached { max_rounds: 10 });

        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    // ── HarnessEvent::total_tokens() ────────────────────────────────

    #[test]
    fn total_tokens_on_token_usage_event() {
        let event = HarnessEvent::TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
        };
        assert_eq!(event.total_tokens(), Some(150));
    }

    #[test]
    fn total_tokens_on_non_token_event() {
        let event = HarnessEvent::Finished;
        assert_eq!(event.total_tokens(), None);

        let event = HarnessEvent::Text("hello");
        assert_eq!(event.total_tokens(), None);
    }

    // ── EventObserver ───────────────────────────────────────────────

    #[test]
    fn event_observer_receives_events() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let count = std::sync::Arc::new(AtomicU32::new(0));
        let c = count.clone();
        let handler = EventObserver::new(move |_event| {
            c.fetch_add(1, Ordering::SeqCst);
        });

        handler.on_event(&HarnessEvent::Finished);
        handler.on_event(&HarnessEvent::Text("hello"));
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn event_observer_always_returns_none() {
        let handler = EventObserver::new(|_event| {});
        // Even for events that might normally get a response, observer returns None.
        let response = handler.on_event(&HarnessEvent::ApprovalRequired {
            name: "shell",
            arguments: "{}",
        });
        assert!(response.is_none());
    }

    #[test]
    fn event_observer_in_composite() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let count = std::sync::Arc::new(AtomicU32::new(0));
        let c = count.clone();
        let composite = CompositeEventHandler::new()
            .with(EventObserver::new(move |_| {
                c.fetch_add(1, Ordering::SeqCst);
            }))
            .with(FnEventHandler::new(|_| Some(EventResponse::Approve)));

        // Observer should fire but not block the Approve response.
        let response = composite.on_event(&HarnessEvent::Finished);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(matches!(response, Some(EventResponse::Approve)));
    }
}
