//! Tool execution, request dispatch, checkpointing, and retry helpers.
//!
//! These functions are called by [`super::harness::Harness::run()`] to handle
//! the per-round mechanics: sending API requests, executing tool calls with
//! approval gates / caching / DAG ordering, saving checkpoints, and retrying
//! transient failures.

use super::config::HarnessConfig;
use super::events::{EventHandler, EventResponse, HarnessEvent};
use super::harness::{ModuleState, compact_if_needed};
use crate::agent::checkpoint::Checkpoint;
use crate::agent::session::SessionManager;
use crate::api::retry::{self, RetryConfig};
use crate::context::ContextBudget;
use crate::context::eviction::{self, ToolResultMeta};
use crate::context::layout::ContextLayout;
use crate::tools::core::ToolSet;
use crate::tools::dag as tool_dag;
use crate::tools::filter::ToolFilter;
use crate::{ChatCompletion, ChatRequest, Message, OpenRouterClient};
use tracing::warn;

// ── Send request ──────────────────────────────────────────────────

/// Build and send the chat completion request, handling streaming vs non-streaming.
pub(crate) async fn send_round_request(
    config: &HarnessConfig,
    client: &OpenRouterClient,
    messages: &[Message],
    model_for_round: &str,
    tools_option: &Option<Vec<crate::ToolDef>>,
    event_handler: &dyn EventHandler,
) -> Result<ChatCompletion, String> {
    let response_format = if config.output_schema.is_some() {
        Some(crate::ResponseFormat {
            fmt_type: crate::ResponseFormatType::JsonSchema,
        })
    } else {
        None
    };

    let body = ChatRequest {
        model: Some(model_for_round.to_string()),
        messages: messages.to_vec(),
        max_tokens: config.max_tokens,
        temperature: config.temperature,
        tools: tools_option.clone(),
        plugins: config.plugins.clone(),
        reasoning: config.reasoning.clone(),
        response_format,
        ..Default::default()
    };

    if config.streaming {
        let events = retry_api_call(&config.retry, || {
            client.chat_stream_live(&body, |event| {
                match event {
                    crate::api::streaming::StreamEvent::TextDelta(delta) => {
                        event_handler.on_event(&HarnessEvent::TextDelta(delta));
                    }
                    crate::api::streaming::StreamEvent::ReasoningDelta(delta) => {
                        event_handler.on_event(&HarnessEvent::ReasoningDelta(delta));
                    }
                    _ => {}
                }
            })
        })
        .await?;

        let text = crate::api::streaming::collect_text(&events);
        let reasoning = crate::api::streaming::collect_reasoning(&events);
        let usage = crate::api::streaming::extract_usage(&events);
        let tool_calls = assemble_tool_calls_from_stream(&events);

        Ok(ChatCompletion {
            content: if text.is_empty() { None } else { Some(text) },
            tool_calls,
            usage,
            annotations: vec![],
            finish_reason: Some("stop".into()),
            reasoning: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning)
            },
        })
    } else {
        retry_api_call(&config.retry, || client.chat(&body)).await
    }
}

// ── Tool execution ────────────────────────────────────────────────

/// Execute tool calls for a round: approval gates, caching, dispatch, and bookkeeping.
///
/// Pushes tool result messages into the [`ContextLayout`] and records eviction
/// metadata using the layout's [`next_message_index()`](ContextLayout::next_message_index).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_and_record_tool_calls(
    config: &HarnessConfig,
    tools: &ToolSet,
    event_handler: &dyn EventHandler,
    client: &OpenRouterClient,
    model_for_round: &str,
    context_budget: &Option<ContextBudget>,
    tool_filter: &mut Option<ToolFilter>,
    layout: &mut ContextLayout,
    modules: &mut ModuleState,
    tool_calls: &[crate::ToolCall],
    round: u32,
) {
    let mut tool_results: Vec<(String, String, String, String)> = Vec::new();
    let mut denied_tools: Vec<(String, String, String)> = Vec::new();

    // Check approval gates (must be sequential — we need handler responses).
    let mut approved_calls: Vec<&crate::ToolCall> = Vec::new();
    for call in tool_calls {
        if config.approval_required_tools.contains(&call.function.name) {
            let response = event_handler.on_event(&HarnessEvent::ApprovalRequired {
                name: &call.function.name,
                arguments: &call.function.arguments,
            });
            match response {
                Some(EventResponse::Deny(reason)) => {
                    denied_tools.push((
                        call.id.clone(),
                        call.function.name.clone(),
                        format!(
                            "Tool '{}' was denied by the user: {}",
                            call.function.name, reason
                        ),
                    ));
                    continue;
                }
                Some(EventResponse::InjectMessage(msg)) => {
                    layout.push_message(Message::user(&msg));
                    denied_tools.push((
                        call.id.clone(),
                        call.function.name.clone(),
                        format!(
                            "Tool '{}' was redirected. User message injected.",
                            call.function.name
                        ),
                    ));
                    continue;
                }
                _ => {}
            }
        }
        approved_calls.push(call);
    }

    // Emit executing events for approved tool calls.
    for call in &approved_calls {
        event_handler.on_event(&HarnessEvent::ToolExecuting {
            name: &call.function.name,
            arguments: &call.function.arguments,
        });
    }

    // Separate cache hits from cache misses.
    let mut cache_hits: Vec<(String, String, String, String)> = Vec::new();
    let mut to_execute: Vec<&crate::ToolCall> = Vec::new();

    for call in &approved_calls {
        if let Some(ref mut cache) = modules.tool_cache
            && tools.is_cacheable(&call.function.name)
            && let Some(cached_result) = cache.get(&call.function.name, &call.function.arguments)
        {
            event_handler.on_event(&HarnessEvent::ToolCacheHit {
                name: &call.function.name,
                arguments: &call.function.arguments,
            });
            cache_hits.push((
                call.id.clone(),
                call.function.name.clone(),
                call.function.arguments.clone(),
                cached_result.to_string(),
            ));
            continue;
        }
        to_execute.push(call);
    }

    // Execute remaining tool calls with dependency-aware ordering.
    let executed = dispatch_tool_execution(config, tools, &to_execute).await;

    // Store executed results in cache and handle invalidation.
    for (_call_id, name, args, result) in &executed {
        if let Some(ref mut cache) = modules.tool_cache {
            if tools.is_mutation_tool(name) {
                cache.invalidate_all();
            } else if tools.is_cacheable(name) {
                cache.put(name, args, result.clone(), round + 1);
            }
        }
    }

    // Evict old cache entries periodically.
    if let Some(ref mut cache) = modules.tool_cache {
        cache.evict_older_than(round + 1, config.cache.max_age_rounds);
    }

    // Combine results: denied tools + cache hits + executed.
    for (call_id, name, reason) in denied_tools {
        tool_results.push((call_id, name, String::new(), reason));
    }
    tool_results.extend(cache_hits);
    tool_results.extend(executed);

    let total_results = tool_results.len();

    // Append results to layout with context budget advisories.
    for (i, (call_id, name, arguments, mut result)) in tool_results.into_iter().enumerate() {
        if let Some(budget) = context_budget {
            let current_messages = layout.to_messages();
            if let Some(advisory) = budget.advisory(&current_messages) {
                result.push_str("\n\n");
                result.push_str(&advisory);
            }
        }

        event_handler.on_event(&HarnessEvent::ToolResult {
            name: &name,
            call_id: &call_id,
            result: &result,
        });

        // Track the to_messages() index before pushing, for eviction.
        let message_index = layout.next_message_index();
        layout.push_message(Message::tool_result(&call_id, result.clone()));

        // Track tool result metadata for eviction.
        if config.eviction.enabled {
            modules.tool_metas.push(ToolResultMeta {
                tool_name: name.clone(),
                args_summary: eviction::summarize_args(&arguments, 80),
                round: round as usize,
                message_index,
                char_count: result.len(),
                estimated_tokens: crate::context::layout::message_tokens(
                    &Message::tool_result(&call_id, result.clone()),
                    config.eviction.config.chars_per_token,
                ),
            });
        }

        // Record file access for preservation through compaction.
        if let Some(ref mut tracker) = modules.file_tracker {
            tracker.record_tool_access(&name, &arguments, round as usize);
        }

        // Update tool filter usage counts.
        if let Some(filter) = tool_filter {
            filter.record_usage(&name);
        }

        // Progressive tool loading: inject extended description on first use.
        if config.progressive_tools
            && !modules.expanded_tools.contains(&name)
            && let Some(ext) = tools.extended_description(&name)
        {
            layout.push_message(Message::user(format!(
                "<tool_guide name=\"{}\">\n{}\n</tool_guide>",
                name, ext
            )));
            modules.expanded_tools.insert(name.clone());
        }

        // Mid-turn compaction: check if context needs compacting after this
        // tool result, but only when there are remaining results to process.
        // The between-rounds compaction handles the case after the last result.
        let remaining = total_results - i - 1;
        if remaining > 0 {
            compact_if_needed(
                config,
                client,
                context_budget,
                layout,
                &mut modules.summarizer,
                &mut modules.tool_metas,
                model_for_round,
                round,
                event_handler,
                &modules.file_tracker,
            )
            .await;
        }
    }
}

/// Dispatch tool execution using the appropriate strategy (sequential, DAG, or parallel).
async fn dispatch_tool_execution(
    config: &HarnessConfig,
    tools: &ToolSet,
    to_execute: &[&crate::ToolCall],
) -> Vec<(String, String, String, String)> {
    if config.sequential_tools {
        let mut results = Vec::new();
        for call in to_execute {
            let result = tools
                .execute(&call.function.name, &call.function.arguments)
                .await;
            results.push((
                call.id.clone(),
                call.function.name.clone(),
                call.function.arguments.clone(),
                result,
            ));
        }
        return results;
    }

    // Check for dependency annotations, with configurable sequential enforcement.
    let annotated = tool_dag::annotate_tool_calls_with_policy(
        &to_execute.iter().map(|c| (*c).clone()).collect::<Vec<_>>(),
        &config.sequential_policy,
    );
    let has_deps = annotated.iter().any(|a| a.depends_on.is_some());

    if has_deps {
        // DAG mode: execute in dependency-ordered waves.
        match tool_dag::build_execution_waves(annotated) {
            Ok(waves) => {
                let mut results = Vec::new();
                for wave in waves {
                    if wave.len() == 1 {
                        let call = &wave[0];
                        let result = tools.execute(&call.name, &call.arguments).await;
                        results.push((
                            call.call_id.clone(),
                            call.name.clone(),
                            call.arguments.clone(),
                            result,
                        ));
                    } else {
                        let futures: Vec<_> = wave
                            .iter()
                            .map(|call| {
                                let name = call.name.clone();
                                let args = call.arguments.clone();
                                let call_id = call.call_id.clone();
                                async move {
                                    let result = tools.execute(&name, &args).await;
                                    (call_id, name, args, result)
                                }
                            })
                            .collect();
                        results.extend(futures::future::join_all(futures).await);
                    }
                }
                results
            }
            Err(e) => {
                warn!("Dependency cycle in tool calls: {e}. Falling back to sequential.");
                let mut results = Vec::new();
                for call in to_execute {
                    let result = tools
                        .execute(&call.function.name, &call.function.arguments)
                        .await;
                    results.push((
                        call.id.clone(),
                        call.function.name.clone(),
                        call.function.arguments.clone(),
                        result,
                    ));
                }
                results
            }
        }
    } else if to_execute.len() > 1 {
        // No deps, multiple calls: parallel.
        let futures: Vec<_> = to_execute
            .iter()
            .map(|call| {
                let name = call.function.name.clone();
                let args = call.function.arguments.clone();
                let call_id = call.id.clone();
                async move {
                    let result = tools.execute(&name, &args).await;
                    (call_id, name, args, result)
                }
            })
            .collect();
        futures::future::join_all(futures).await
    } else {
        // Single call.
        let mut results = Vec::new();
        for call in to_execute {
            let result = tools
                .execute(&call.function.name, &call.function.arguments)
                .await;
            results.push((
                call.id.clone(),
                call.function.name.clone(),
                call.function.arguments.clone(),
                result,
            ));
        }
        results
    }
}

// ── Checkpointing ─────────────────────────────────────────────────

/// Save a checkpoint for the current round via the [`SessionManager`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn save_round_checkpoint(
    session_manager: &Option<SessionManager>,
    trace_id: &str,
    messages: &[Message],
    text_output: &[String],
    round: u32,
    total_prompt_tokens: u64,
    total_completion_tokens: u64,
    estimated_cost_usd: f64,
    event_handler: &dyn EventHandler,
) {
    let Some(mgr) = session_manager else {
        return;
    };
    let checkpoint = Checkpoint {
        trace_id: trace_id.to_string(),
        messages: messages.to_vec(),
        text_output: text_output.to_vec(),
        round: round + 1,
        total_prompt_tokens,
        total_completion_tokens,
        estimated_cost_usd,
        timestamp: format!(
            "epoch:{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        ),
    };
    match mgr.save_checkpoint(&checkpoint) {
        Ok(path) => {
            let path_str = path.display().to_string();
            event_handler.on_event(&HarnessEvent::CheckpointSaved {
                round: round + 1,
                path: &path_str,
            });
        }
        Err(e) => {
            warn!("Failed to save checkpoint: {e}");
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────

/// Retry an async API call with exponential backoff for transient errors.
pub(crate) async fn retry_api_call<T, F, Fut>(
    config: &RetryConfig,
    mut call: F,
) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let mut attempt = 0;
    loop {
        match call().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if attempt < config.max_retries
                    && retry::is_transient_error(&e)
                    && !retry::is_permanent_error(&e)
                {
                    let delay = config.delay_for_attempt(attempt);
                    warn!(
                        "Transient API error (attempt {}/{}): {e}. Retrying in {delay:?}...",
                        attempt + 1,
                        config.max_retries,
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                } else {
                    return Err(e);
                }
            }
        }
    }
}

/// Assemble complete tool calls from streaming ToolCallDelta events.
///
/// Stream events deliver tool calls incrementally — the first delta for an
/// index carries the id and name, subsequent deltas carry argument fragments.
/// This function accumulates them into complete `ToolCall` objects.
pub(crate) fn assemble_tool_calls_from_stream(
    events: &[crate::api::streaming::StreamEvent],
) -> Vec<crate::ToolCall> {
    use std::collections::BTreeMap;

    // Accumulate by index.
    let mut calls: BTreeMap<usize, (Option<String>, Option<String>, String)> = BTreeMap::new();

    for event in events {
        if let crate::api::streaming::StreamEvent::ToolCallDelta {
            index,
            id,
            name,
            arguments_delta,
        } = event
        {
            let entry = calls.entry(*index).or_insert((None, None, String::new()));
            if let Some(id) = id {
                entry.0 = Some(id.clone());
            }
            if let Some(name) = name {
                entry.1 = Some(name.clone());
            }
            entry.2.push_str(arguments_delta);
        }
    }

    calls
        .into_values()
        .filter_map(|(id, name, arguments)| {
            let id = id?;
            let name = name?;
            Some(crate::ToolCall {
                id,
                call_type: crate::CallType::Function,
                function: crate::FunctionCallData { name, arguments },
            })
        })
        .collect()
}
