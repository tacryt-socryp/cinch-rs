# cinch-rs — Data Flow Documentation

> **Purpose:** Map every data source, transformation, and output in the agent
> framework so that consumers and contributors can understand exactly how data
> moves through a harness run.
>
> **Last updated:** 2026-02-16

---

## Table of Contents

1. [Architecture Overview](#1-architecture-overview)
2. [The Harness Loop](#2-the-harness-loop)
3. [Module-by-Module Data Flows](#3-module-by-module-data-flows)
4. [Type Flow Summary](#4-type-flow-summary)
5. [Configuration Defaults](#5-configuration-defaults)
6. [Persistent State & Files](#6-persistent-state--files)
7. [Event Flow](#7-event-flow)

---

## 1. Architecture Overview

`cinch-rs` is a **library + CLI** that runs LLM agents via the OpenRouter chat
completions API. The caller provides a system prompt, tool set, and configuration;
the harness handles the agentic loop and all advanced context management.

```
┌─────────────────────────────────────────────────────────────────────┐
│  Caller  (CLI, custom agents)                                       │
│  Provides: system prompt, ToolSet, HarnessConfig, EventHandler      │
└──────────────────────────────┬──────────────────────────────────────┘
                               │
              ┌────────────────▼────────────────┐
              │          Harness::run()          │
              │                                  │
              │  for round in 0..max_rounds:     │
              │    1. Route model for round       │
              │    2. Evict stale tool results    │
              │    3. Summarize if still over 80% │
              │    4. Build ChatRequest            │
              │    5. Send to OpenRouter API       │
              │    6. Emit events (text, reason)   │
              │    7. If tool calls:               │
              │       a. Execute via ToolSet       │
              │       b. Inject budget advisory    │
              │       c. Save checkpoint           │
              │    8. Else: finished               │
              └──┬──────────┬──────────┬──────────┘
                 │          │          │
    ┌────────────▼──┐  ┌───▼────┐  ┌──▼──────────────┐
    │  ToolSet       │  │ Client │  │ Context modules  │
    │  - Tool trait   │  │ HTTP   │  │ - Budget         │
    │  - Dispatch     │  │ + SSE  │  │ - Eviction       │
    │  - Validation   │  │ + JSON │  │ - Summarizer     │
    │  - Cache        │  │        │  │ - Layout         │
    │  - Truncation   │  │        │  │                  │
    └────────────────┘  └────────┘  └──────────────────┘
```

### Data boundaries

| Boundary | Direction | Format |
|----------|-----------|--------|
| Caller → Harness | In | `Vec<Message>`, `ToolSet`, `HarnessConfig`, `&dyn EventHandler` |
| Harness → OpenRouter | Out/In | HTTPS POST JSON (`ChatRequest` → `ChatCompletion`) |
| Harness → ToolSet | Out/In | `(&str, &str)` name+args → `String` result |
| Harness → EventHandler | Out | `&HarnessEvent` → `Option<EventResponse>` |
| Harness → Caller | Out | `HarnessResult` (text, tokens, cost, messages, structured output) |
| Harness → Disk | Out | Checkpoints (JSON), agent profile (JSON) |

---

## 2. The Harness Loop

The core `Harness::run()` method (`agent/harness.rs`) executes the following
pipeline on each round:

```
                    ┌──────────────────────┐
                    │  init_modules()       │
                    │  inject_prompt_extras │
                    │  auto-calibrate       │
                    │  context budget       │
                    └──────────┬───────────┘
                               │
                    ┌──────────▼───────────┐
         ┌────────►│  Round N              │
         │         │                       │
         │         │  1. Check stop signal  │
         │         │  2. Route model        │
         │         │  3. Evict if ≥80%      │
         │         │  4. Summarize if ≥80%  │
         │         │  5. Emit RoundStart    │
         │         └──────────┬───────────┘
         │                    │
         │         ┌──────────▼───────────┐
         │         │  send_round_request() │
         │         │                       │
         │         │  ChatRequest {        │
         │         │    model,             │
         │         │    messages,          │
         │         │    tools,             │
         │         │    max_tokens,        │
         │         │    temperature,       │
         │         │    plugins,           │
         │         │    reasoning,         │
         │         │  }                    │
         │         └──────────┬───────────┘
         │                    │
         │         ┌──────────▼───────────┐
         │         │  Process response     │
         │         │                       │
         │         │  • Record token usage │
         │         │  • Emit Reasoning     │
         │         │  • Emit Text          │
         │         │  • Collect annotations│
         │         └──────────┬───────────┘
         │                    │
         │              ┌─────▼─────┐
         │              │ Tool calls │──── No ───► Finished ──► finalize_run()
         │              │ present?   │                          └► HarnessResult
         │              └─────┬─────┘
         │                    │ Yes
         │         ┌──────────▼────────────────────┐
         │         │  execute_and_record_tool_calls │
         │         │                                │
         │         │  For each tool call:           │
         │         │    1. Check cache              │
         │         │    2. Check approval            │
         │         │    3. Execute via ToolSet       │
         │         │    4. Append budget advisory    │
         │         │    5. Record tool_meta          │
         │         │    6. Update filter stats       │
         │         │    7. Save checkpoint           │
         │         └──────────┬────────────────────┘
         │                    │
         └────────────────────┘
```

### Plan-execute phase overlay

When `plan_execute` is enabled (the default), the loop has two phases:

1. **Planning** — tool defs are restricted to read-only tools + `submit_plan`.
   The LLM gathers context and formulates a plan.
2. **Executing** — after `submit_plan` is called (or planning rounds are
   exhausted), the full tool set is restored and the execution prompt is
   injected.

```
  Phase::Planning ──submit_plan()──► Phase::Executing
         │                                  │
    read-only tools                    full tool set
    + submit_plan                      + all tools
```

---

## 3. Module-by-Module Data Flows

### 3.1 API Client (`lib.rs`)

```
  OpenRouterClient::new(api_key)
         │
         ▼
  OpenRouterClient::chat(&ChatRequest)
         │
         ├──► POST https://openrouter.ai/api/v1/chat/completions
         │    Headers: Authorization, HTTP-Referer, X-Title, Content-Type
         │    Body: JSON-serialized ChatRequest
         │
         ▼
  Parse response → ChatCompletion {
      content: Option<String>,         // LLM text output
      reasoning: Option<String>,       // Extended thinking (if enabled)
      tool_calls: Vec<ToolCall>,       // Function calls to execute
      usage: Option<Usage>,            // Token counts
      annotations: Vec<Annotation>,    // Web-search citations
  }
```

**Streaming variant** (`api/streaming.rs`):
```
  OpenRouterClient::chat_stream(&ChatRequest)
         │
         ├──► POST with stream: true
         │
         ▼
  SSE stream → Vec<StreamEvent> {
      TextDelta(String),
      ReasoningDelta(String),
      ToolCallDelta { index, id, name, arguments_delta },
      Usage { prompt_tokens, completion_tokens },
      Done,
  }
```

### 3.2 Tool Dispatch (`tools/core.rs`)

```
  ToolSet::execute(name, arguments)
         │
         ├── Lookup tool by name → Option<&dyn Tool>
         │     └── Unknown? → "Error: unknown tool '{name}'"
         │
         ├── Validate arguments (if with_arg_validation enabled)
         │     └── Invalid? → structured error for LLM self-correction
         │
         ├── Execute with optional timeout
         │     └── Timeout? → "Error: tool '{name}' timed out"
         │
         ├── Truncate result to max_result_bytes
         │     └── Appends "[truncated: N bytes total]"
         │
         └──► String result
```

**Cache layer** (`tools/cache.rs`):
```
  Before execution:
    cache.get(tool_name, args_hash) → Option<String>
         │
         ├── Hit? → Return cached result, emit ToolCacheHit event
         │
         └── Miss? → Execute normally, then:
               cache.put(tool_name, args_hash, result)

  On mutation tool execution:
    cache.invalidate_all()
```

### 3.3 Context Budget (`context/budget.rs`)

```
  ContextBudget::estimate_usage(&messages)
         │
         ├── Sum character lengths of all message contents
         ├── Add system prompt character count
         ├── Divide by chars_per_token ratio (default 3.5)
         │
         └──► ContextUsage { estimated_tokens, max_tokens, usage_pct }

  ContextBudget::advisory(&messages)
         │
         ├── usage_pct < 0.60 → None
         ├── usage_pct ≥ 0.60 → "Prioritize drafting over research..."
         └── usage_pct ≥ 0.80 → "Prioritize saving drafts NOW..."
```

### 3.4 Tool Result Eviction (`context/eviction.rs`)

Triggered when context usage ≥ 80% at the start of a round.

```
  evict_tool_results(messages, tool_metas, current_round, target_tokens, config)
         │
         ├── Filter candidates: not protected, age ≥ min_age_rounds
         ├── Sort by round (oldest first)
         │
         ├── For each candidate (oldest first):
         │     ├── Already evicted? (starts with "[Cleared:") → skip
         │     ├── Replace content with placeholder:
         │     │     "[Cleared: tool_name(args_summary) — N chars, round M]"
         │     └── Stop when estimated tokens ≤ target_tokens
         │
         └──► freed_chars: usize
```

### 3.5 Summarization (`context/summarizer.rs`)

Triggered when context usage ≥ 80% after eviction.

```
  Summarizer::build_summarization_request(middle_zone_messages)
         │
         ├── System prompt: SUMMARIZATION_PROMPT (preserve tool calls, facts, decisions)
         ├── User prompt: concatenated message contents from middle zone
         │
         └──► (sys_prompt, user_prompt)

  Send to LLM as a side-channel request
         │
         ▼
  Apply compaction:
    1. Remove middle-zone messages from conversation
    2. Insert <context_summary>...</context_summary> at boundary
    3. Insert assistant acknowledgment
    4. Reindex tool_metas for shifted message indices
```

### 3.6 Three-Zone Layout (`context/layout.rs`)

```
  ┌─────────────────────────────────┐
  │  Zone 1: Pinned Prefix          │  ← System prompt, original task
  │  Never modified.                │     Attention sink + cache anchor
  ├─────────────────────────────────┤
  │  Zone 2: Compressed History     │  ← Running summary (post-compaction)
  │  Replaced atomically on each    │     Information-dense but lossy
  │  compaction cycle.              │
  ├─────────────────────────────────┤
  │  Zone 3: Middle (pre-compaction)│  ← Messages between prefix and recency
  │  Drained into Zone 2 when       │     window that haven't been summarized
  │  compaction triggers.            │
  ├─────────────────────────────────┤
  │  Zone 4: Raw Recency Window     │  ← Last N messages, unmodified
  │  Full fidelity. Exploits        │     Exploits LLM recency bias
  │  recency bias.                  │
  └─────────────────────────────────┘

  push_message(msg) → recency_window.push_back(msg)
    └── If recency_window.len() > keep_recent:
          pop_front() → middle.push()

  to_messages() → prefix + summary_pair + middle + recency_window
```

### 3.7 Retry Logic (`api/retry.rs`)

```
  RetryConfig { max_retries, initial_delay, max_delay, multiplier, jitter }

  On API error:
    is_transient_error(error)?
      ├── Yes (429, 500-504, network) → retry after delay_for_attempt(n)
      │     delay = min(initial * multiplier^n, max_delay) × jitter_factor
      │
      └── No (400, 401, 403, 404) → permanent failure, do not retry
```

### 3.8 Model Routing (`api/router.rs`)

```
  RoutingStrategy::model_for_round(round, is_sub_agent)
         │
         ├── Single(model) → always returns model
         ├── RoundBased { default, overrides } → overrides.get(round) or default
         └── CheapOrchestrator { cheap, powerful, switch_at }
               └── round < switch_at ? cheap : powerful
```

### 3.9 Cost Tracking (`api/tracing.rs`)

```
  pricing_for_model("anthropic/claude-sonnet-4")
         │
         └──► ModelPricing { input_per_million: 3.0, output_per_million: 15.0 }

  CostTracker::record(prompt_tokens, completion_tokens, &pricing)
         │
         └──► Accumulates: total_prompt_tokens, total_completion_tokens, estimated_cost_usd
```

### 3.10 Checkpointing (`agent/checkpoint.rs`)

```
  After each round with tool calls:
    CheckpointManager::save(Checkpoint {
        trace_id, messages, text_output, round,
        prompt_tokens, completion_tokens, estimated_cost_usd,
    })
         │
         └──► .agent-checkpoints/{trace_id}_round_{N}.json

  On successful completion:
    CheckpointManager::cleanup(trace_id) → delete checkpoint files

  On resume:
    CheckpointManager::load_latest() → Checkpoint (restores conversation state)
```

### 3.11 Agent Profile (`agent/profile.rs`)

```
  At run start:
    AgentProfile::load_or_create(path, agent_id)
         │
         └──► Loads JSON profile or creates new with defaults
              Injects user_instructions into system prompt

  At run end:
    profile.record_run(model, rounds, prompt_tokens, completion_tokens, finished, cost)
         │
         ├── Updates per-model observations (tokens/round EMA)
         ├── Updates run count and total cost
         │
         └──► profile.save(path) → JSON to disk
```

### 3.12 Tool Filter (`tools/filter.rs`)

```
  ToolFilter::filter_for_task(&keywords, &all_tool_defs)
         │
         ├── Score each tool by:
         │     ├── Category keyword match (tool in a matching category?)
         │     ├── Historical usage frequency
         │     └── always_include set membership
         │
         └──► Filtered Vec<ToolDef> (subset sent to LLM)
```

### 3.13 UI State & Event Bridge (`ui/`)

The `ui` module provides a generic UI abstraction layer that any frontend (TUI, web, headless) can consume.

#### UiState — shared mutable state

```
  Arc<Mutex<UiState>>
         │
         ├── Written by: UiEventHandler, AskUserTool, convenience updaters
         ├── Read by: cinch-tui renderer, domain code
         │
         └── Fields:
               phase, round, max_rounds, context_pct, model, cycle,
               agent_output: Vec<AgentEntry>,
               streaming_buffer: String,
               logs: Vec<LogLine>,
               running, quit_requested,
               active_question: Option<ActiveQuestion>,
               next_cycle_at: Option<Instant>,
               extensions: Box<dyn UiExtension>  ← domain escape hatch
```

#### UiEventHandler — generic EventHandler → UiState bridge

Eliminates per-agent boilerplate by mapping all common `HarnessEvent` variants to `UiState` updates. Domain handlers only need to handle domain-specific events.

```
  HarnessEvent                          UiState update
  ─────────                             ──────────────
  RoundStart { round, max, ctx }   →    update_round()
  Text(text)                       →    push_agent_text()
  TextDelta(delta)                 →    push_agent_text_delta()
  ToolExecuting { name, args }     →    update_phase("Tool: {name}") + push_tool_executing()
  ToolResult { name, result }      →    push_tool_result()
  Reasoning(text)                  →    push_agent_text("[reasoning] ...")
  PhaseTransition { from, to }     →    push_agent_text("[phase] ...")
  PlanSubmitted { summary }        →    push_agent_text("[plan] ...")
  Finished                         →    update_phase("Finished")
  RoundLimitReached                →    update_phase("Round limit reached")
  (all others)                     →    ignored

  Returns: always None (pure state updater, never controls flow)
```

Domain crates compose with `CompositeEventHandler`:

```
  CompositeEventHandler::new()
      .with(LoggingHandler)                         // logging
      .with(domain_result_handler)                  // domain: count tweets, etc.
      .with(UiEventHandler::new(ui_state.clone()))  // generic UI updates
      .with(domain_event_handler)                   // domain: budget, fallback text
```

#### AskUserTool — LLM-callable human-in-the-loop tool

```
  LLM calls ask_user(prompt, choices, editable?, timeout?)
         │
         ├── Headless mode (no UiState)?
         │     └── Return {"status": "timed_out"} immediately
         │
         ├── Validate: 2-10 choices required
         │
         ├── Build UserQuestion from args
         │     └── Each choice string → QuestionChoice { label: "Option N", body, metadata: "" }
         │
         ├── ask_question(state, question, timeout)  ← sets UiState.active_question
         │
         ├── Poll loop (200ms interval):
         │     └── poll_question(state) → Option<QuestionResponse>
         │
         └── Return JSON:
               {"status": "selected"|"edited"|"skipped"|"timed_out",
                "index": N, "text": "..."}
```

#### Question flow (ask_question / poll_question)

```
  Agent or LLM                    UiState                    Frontend (TUI/web)
  ────────────                    ───────                    ──────────────────
  ask_question(q, timeout) ──►   active_question = Some(    Renders question modal
                                   ActiveQuestion {           with choices, countdown
                                     question, deadline,
                                     response: None,
                                     done: false
                                   })

  poll_question() ◄────────────  response: None              User navigates, selects
  poll_question() ◄────────────  response: None              ...
  poll_question() ◄── Some(r) ── response: Some(Selected(i)) User pressed Enter
                                 active_question = None       Modal dismissed
```

### 3.14 Sub-Agent Delegation (`agent/sub_agent.rs`)

```
  Parent Harness::run()
         │
         ├── LLM calls delegate_sub_agent(task, tools)
         │
         └──► Spawn child Harness::run() with:
                ├── Isolated message history
                ├── Shared Arc<OpenRouterClient> and Arc<ToolSet>
                ├── TokenBudgetSemaphore::acquire(estimated_tokens)
                ├── Checkpoint and reflexion disabled (parent handles)
                └── Concurrency limited via tokio::sync::Semaphore (max 5)

              Child completes → result string returned to parent as tool result
              TokenBudgetSemaphore::release(actual_tokens_used)
```

---

## 4. Type Flow Summary

### Request path (Caller → OpenRouter)

```
HarnessConfig + Vec<Message> + ToolSet
    │
    ▼
ChatRequest {
    model: String,
    messages: Vec<Message>,
    tools: Option<Vec<ToolDef>>,
    max_tokens: u32,
    temperature: f32,
    plugins: Option<Vec<Plugin>>,
    reasoning: Option<ReasoningConfig>,
    response_format: Option<ResponseFormat>,
}
    │
    ▼
JSON POST → OpenRouter API
```

### Response path (OpenRouter → Caller)

```
JSON response → ChatCompletion
    │
    ├── content → HarnessEvent::Text → result.text_output
    ├── reasoning → HarnessEvent::Reasoning
    ├── tool_calls → execute → HarnessEvent::ToolResult → append to messages
    ├── usage → HarnessEvent::TokenUsage → result.total_*_tokens
    └── annotations → result.annotations

    │ (on final round or no tool calls)
    ▼
HarnessResult {
    trace_id, messages, text_output, annotations,
    total_prompt_tokens, total_completion_tokens,
    rounds_used, finished, estimated_cost_usd,
    structured_output,
}
```

### Tool call path

```
ChatCompletion.tool_calls: Vec<ToolCall>
    │
    ▼ (for each call, possibly in parallel)
ToolCall { id, function: { name, arguments } }
    │
    ├── Cache check → hit? → return cached String
    ├── Approval check → denied? → return "Error: {reason}"
    │
    ▼
ToolSet::execute(name, arguments) → String
    │
    ├── Argument validation (optional)
    ├── Tool::execute(arguments) with timeout
    ├── Truncation to max_result_bytes
    │
    ▼
Message::tool_result(call_id, result_string)
    │
    ├── Budget advisory appended (if over threshold)
    ├── ToolResultMeta recorded (for future eviction)
    └── Appended to messages Vec
```

---

## 5. Configuration Defaults

All advanced modules are **enabled by default** via `HarnessConfig::default()`:

| Setting | Default | Notes |
|---------|---------|-------|
| `model` | `anthropic/claude-sonnet-4` | Via `DEFAULT_MODEL` constant |
| `max_rounds` | 10 | Tool-use round-trips |
| `max_tokens` | 1024 | Per-response token limit |
| `temperature` | 0.7 | Sampling temperature |
| `streaming` | false | SSE streaming disabled |
| `context_window_tokens` | 200,000 | For budget calculations |
| `keep_recent_messages` | 10 | Raw recency window size |
| `eviction.enabled` | true | 3-round min age, no protected tools |
| `summarizer.enabled` | true | LLM-based incremental summarization |
| `checkpoint.enabled` | true | Dir: `.agent-checkpoints` |
| `cache.enabled` | true | 100 entries, 10-round TTL |
| `plan_execute.enabled` | true | Plan with read-only tools first |
| `retry.max_retries` | 0 | No retries by default |
| `memory_prompt` | `Some(...)` | File-based memory instructions |
| `sequential_tools` | false | Parallel execution by default |
| `max_result_bytes` | 30,000 | Tool output truncation limit |
| `tool_timeout` | 60s | Per-tool execution timeout |

---

## 6. Persistent State & Files

### Written by the harness

| File | Format | Written when | Content |
|------|--------|-------------|---------|
| `.agent-checkpoints/{trace}_round_{N}.json` | JSON | After each round with tool calls | Full conversation state |
| Agent profile path (caller-specified) | JSON | On run completion | Per-tool stats, model observations, cost |

### Read by the harness

| File | Format | Read when | Content |
|------|--------|----------|---------|
| `.agent-checkpoints/*.json` | JSON | On resume (`load_latest`) | Last checkpoint |
| Agent profile path | JSON | On run start | Cross-session identity and stats |

### Written by built-in tools

| Tool | Writes to | Content |
|------|-----------|---------|
| `shell` | Depends on command | Arbitrary (caller's responsibility) |
| `save_draft` | Caller-defined path | Tool-specific output |

### Read by built-in tools

| Tool | Reads from | Content |
|------|------------|---------|
| `read_file` | Caller's workdir | Any text file (path-traversal blocked) |
| `list_files` | Caller's workdir | Directory listings |
| `grep` | Caller's workdir | Regex search results |
| `find_files` | Caller's workdir | Glob-matched file paths |

---

## 7. Event Flow

Events are the primary observability mechanism. Every significant action in the
harness loop emits a `HarnessEvent` to the caller's `EventHandler`.

### Round lifecycle events

```
RoundStart { round, max_rounds, context_usage }
    │
    ├── [optional] ModelRouted { model, round }
    ├── [optional] Eviction { freed_chars, evicted_count }
    ├── [optional] Compaction { compaction_number }
    │
    ├── [streaming] TextDelta(text) ×N
    ├── [streaming] ReasoningDelta(text) ×N
    │
    ├── Text(text)
    ├── Reasoning(text)
    ├── TokenUsage { prompt_tokens, completion_tokens }
    │
    ├── ToolCallsReceived { round, count }
    │     │
    │     ├── [per tool] ToolExecuting { name, arguments }
    │     ├── [per tool] ApprovalRequired { name, arguments }
    │     ├── [per tool] ToolCacheHit { name, arguments }
    │     ├── [per tool] ToolResult { name, call_id, result }
    │     │
    │     └── CheckpointSaved { round, path }
    │
    └── [plan-execute] PlanSubmitted { summary }
        [plan-execute] PhaseTransition { from, to }
```

### Terminal events

```
Finished                              ← Agent completed naturally
RoundLimitReached { max_rounds }      ← Hit round limit without finishing
```

### UiState event flow

When a `UiEventHandler` is composed into the handler chain, UI state updates happen
automatically alongside the harness event flow:

```
HarnessEvent (from Harness::run)
    │
    ├── LoggingHandler          → tracing log output
    ├── ToolResultHandler       → result accumulation for caller
    ├── UiEventHandler          → UiState updates (round, text, tools, phases)
    └── Domain EventHandler     → domain-specific updates (counts, budget, etc.)
```

The `UiTracingLayer` captures `tracing` log events into `UiState.logs` in parallel:

```
tracing::info!(...)  ──►  UiTracingLayer  ──►  UiState.logs.push(LogLine { time, level, message })
```

### EventResponse flow

Most events return `None`. The `ApprovalRequired` event supports feedback:

```
ApprovalRequired { name, arguments }
    │
    ├── EventResponse::Approve → proceed with execution
    ├── EventResponse::Deny(reason) → return "Error: {reason}" to LLM
    ├── EventResponse::InjectMessage(text) → add user message, continue
    └── None → auto-approve (default behavior)
```
