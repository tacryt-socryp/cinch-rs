# Cinch: Vision Document

## What it is

The `cinch-rs` crate (formerly `openrouter`) is a Rust library that provides a complete, model-agnostic agent runtime on top of the OpenRouter chat completions API. At its core is the **Harness** — a reusable agentic tool-use loop that sends messages to an LLM, executes any requested tool calls, appends results, and repeats until the model produces a text-only response or a round limit is reached. Around this core, the crate supplies a layered set of modules for context management, multi-agent orchestration, cost control, and cross-session learning.

The crate serves two roles today:

1. **Library** — consumed by future agents as `cinch_rs::harness::Harness`.
2. **CLI** — a standalone `cinch` binary for ad-hoc LLM requests with tool execution.

## Current architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│  Caller (CLI, future agents)                                        │
│  - Provides system prompt, user task, ToolSet, HarnessConfig        │
│  - Implements EventHandler for logging / TUI / state tracking       │
└──────────────────────────────┬──────────────────────────────────────┘
                               │
              ┌────────────────▼────────────────┐
              │          Harness                 │
              │  (agentic tool-use loop)         │
              │                                  │
              │  for round in 0..max_rounds:     │
              │    1. Build ChatRequest           │
              │    2. Send to OpenRouter API      │
              │    3. Emit events (text, reason)  │
              │    4. If tool calls:              │
              │       a. Execute in parallel      │
              │       b. Inject budget advisories │
              │       c. Append tool results      │
              │    5. Else: finished              │
              └──┬──────────┬──────────┬─────────┘
                 │          │          │
    ┌────────────▼──┐  ┌───▼────┐  ┌──▼──────────────┐
    │  ToolSet       │  │ Client │  │ ContextBudget    │
    │  - Tool trait   │  │ HTTP   │  │ - Token tracking │
    │  - Dispatch     │  │ + JSON │  │ - Advisories     │
    │  - Validation   │  │        │  │ - Calibration    │
    │  - Truncation   │  │        │  │                  │
    │  - Timeouts     │  │        │  │                  │
    └────────────────┘  └────────┘  └──────────────────┘
```

### Module inventory

| Module | Purpose | Status |
|--------|---------|--------|
| `lib.rs` | Core types: `ChatRequest`, `Message`, `ToolDef`, `OpenRouterClient`, plugins | Stable |
| `harness.rs` | Agentic tool-use loop with events, retry, cost tracking | Stable |
| `tool.rs` | `Tool` trait, `ToolSet`, pseudo-tools (`think`, `todo`) | Stable |
| `common_tools.rs` | Reusable tools: `read_file`, `grep`, `find_files`, `list_files`, `shell` | Stable |
| `context.rs` | `ContextBudget` — token usage tracking with threshold advisories | Stable |
| `context_layout.rs` | Three-zone context management (pinned prefix / compressed history / recency window) | Implemented |
| `eviction.rs` | Tool result eviction — replace old results with compact placeholders | Implemented |
| `summarizer.rs` | Anchored incremental summarization for context compaction | Implemented |
| `sub_agent.rs` | Recursive sub-agent delegation with token budget semaphore | Implemented |
| `plan_execute.rs` | Two-phase plan-then-execute workflow | Implemented |
| `reflexion.rs` | Cross-session learning from failed trajectories (JSONL persistence) | Implemented |
| `reflection.rs` | Structured tool failure formatting for LLM self-correction | Implemented |
| `model_router.rs` | Multi-model routing: cheap orchestration, round-based switching | Implemented |
| `tool_filter.rs` | Dynamic tool filtering by category, task keywords, usage frequency | Implemented |
| `tool_spec.rs` | Rich tool metadata: when-to-use, when-not-to-use, disambiguation hints | Implemented |
| `checkpoint.rs` | Checkpoint/resume for long-running loops + adaptive round limits | Implemented |
| `external_memory.rs` | File-system scratch directory for agent overflow storage | Implemented |
| `memory_tools.rs` | Tool trait wrappers for external memory (read/write/list) | Implemented |
| `streaming.rs` | SSE streaming for incremental text/reasoning/tool-call deltas | Implemented |
| `tool_cache.rs` | Tool result caching with FNV-1a hashing and age-based eviction | Implemented |
| `agent_profile.rs` | Persistent agent identity across sessions | Implemented |
| `metrics.rs` | Round-level latency/token metrics + EMA token estimator | Implemented |
| `validation.rs` | JSON Schema validation for tool arguments before execution | Implemented |
| `retry.rs` | Transient error detection and exponential backoff retry | Stable |
| `tracing_ids.rs` | Trace IDs, per-model pricing tables, cost tracking | Stable |
| `ui/mod.rs` | Generic UI state (`UiState`), `AgentEntry`, `LogLine`, convenience updaters | Stable |
| `ui/traits.rs` | `UiExtension` trait for domain-specific state via downcasting | Stable |
| `ui/question.rs` | `UserQuestion`, `QuestionChoice`, `QuestionResponse`, `ActiveQuestion` | Stable |
| `ui/tracing.rs` | `UiTracingLayer` — generic tracing subscriber layer for UI log capture | Stable |
| `ui/event_handler.rs` | `UiEventHandler` — generic `EventHandler` → `UiState` bridge | Stable |
| `ui/ask_user_tool.rs` | `AskUserTool` — LLM-callable human-in-the-loop tool | Stable |

## Design principles

**1. The harness is an opinionated framework.**
The harness makes decisions so callers don't have to. Context management, eviction, summarization, reflexion, checkpointing, and tool filtering are all active by default with sensible defaults. Callers provide a system prompt, a tool set, and a model — the harness handles everything else. You can override any default, but you shouldn't need to. The goal is that `Harness::new(&client, &tools, config).run(messages)` does the right thing out of the box for the vast majority of agent tasks.

**2. Tools are the unit of capability.**
Every agent capability — reading files, searching code, posting tweets, checking prices — is a `Tool` trait implementor with a JSON Schema definition and an async `execute` method. The `ToolSet` handles dispatch, validation, truncation, and timeouts. Adding a capability means implementing one trait.

**3. Context is the scarcest resource.**
Every module in the crate treats the context window as a finite budget. Tool results are truncated at the source (`common_tools`), evicted when stale (`eviction`), compressed via summarization (`summarizer`), and organized into zones (`context_layout`). Budget advisories are injected into tool results to nudge the LLM toward finishing. This is the single most important thing the harness does beyond the basic loop.

**4. Observability over magic.**
The framework is opinionated, not opaque. The `EventHandler` trait and `HarnessEvent` enum give callers full visibility into every round: tool calls, results, token usage, reasoning content, and termination conditions. The `metrics` module adds latency and throughput tracking. The harness makes decisions automatically, but it always tells you what it decided and why.

**5. Cost control is first-class.**
Per-model pricing tables, cumulative cost tracking, token budget semaphores for sub-agent trees, and model routing strategies all exist to keep API spend predictable. The harness reports `estimated_cost_usd` on every run.

## What the harness does well today

- **Single-agent tool-use loops** work reliably with retry, truncation, pseudo-tool handling, and clean termination.
- **Token accounting** is accurate enough for budget management (EMA-calibrated chars-per-token estimation, actual API usage tracking).
- **The tool ecosystem** is rich: typed argument structs with `schemars`, path traversal blocking, destructive command blocking, tool specs with disambiguation hints.
- **Cross-session learning** via reflexion memory is a genuine differentiator — agents learn from past failures without fine-tuning.
- **The three-zone context layout** is the right architecture for long-running agents (pinned prefix for cache stability, compressed middle for history, raw recency for fidelity).

## Implementation status

### Completed

**1. Integration of advanced modules into the harness loop** — All advanced modules are wired into `Harness::run()` with sensible defaults enabled by default. The loop integrates: model routing per round, eviction at 80% context usage, LLM-based summarization, reflexion lesson injection and recording, checkpointing, tool filter usage tracking, and structured output parsing. Callers override via `HarnessConfig` struct fields; disabling is explicit (`CheckpointConfig::disabled()`).

**2. Sub-agent execution** — `DelegateSubAgentTool` spawns child `Harness::run()` with isolated context, `Arc<OpenRouterClient>` and `Arc<ToolSet>` sharing, `tokio::sync::Semaphore` for concurrency limiting (max 5 concurrent children), and tree-wide token budget via `TokenBudgetSemaphore`. Children disable checkpoint/reflexion (parent handles those). Multiple `delegate_sub_agent` calls in one round run concurrently via the harness's existing `join_all`.

**3. Streaming responses** — `streaming.rs` provides `StreamEvent` enum, `OpenRouterClient::chat_stream()` with SSE parsing, and helper functions (`collect_text`, `collect_reasoning`, `extract_usage`). Integrated into `Harness::run()` via `HarnessConfig::streaming` flag — when enabled, emits `TextDelta` and `ReasoningDelta` events during generation. Tool call deltas are assembled into complete `ToolCall` objects.

**4. Structured output mode** — `HarnessConfig::output_schema` sends `response_format: { type: "json_schema" }` in every request. On the final round, the harness parses the last text output as JSON into `HarnessResult::structured_output`.

**5. Multi-turn human-in-the-loop** — `EventHandler::on_event()` returns `Option<EventResponse>` (breaking change, backward-compatible via default `None` return). `EventResponse` variants: `Approve`, `Deny(reason)`, `InjectMessage(text)`. `HarnessConfig::approval_required_tools` gates specific tools. Denied tools return error to LLM; `InjectMessage` adds a user message to the conversation.

**6. Tool result caching** — `tool_cache.rs` with FNV-1a hashing, `CACHEABLE_TOOLS` and `MUTATION_TOOLS` constants, age-based eviction. Integrated into `Harness::run()`: cache checked before execution, results stored after, cache invalidated on mutation tools. `HarnessCacheConfig` (enabled by default, 100 entries, 10-round TTL). Emits `ToolCacheHit` events.

**7. External memory tools** — `memory_tools.rs` wraps `ExternalMemory` as `memory_write`/`memory_read`/`memory_list` Tool trait implementations via `Arc<ExternalMemory>`.

**8. Persistent agent identity** — `agent_profile.rs` defines `AgentProfile` with: calibrated token estimator, per-tool usage stats, model observations, user instructions, cost tracking. JSON file persistence via `load_or_create` / `save`. Bounded observations (100 max). User instructions injected into system prompts.

### Remaining gaps

**9. Parallel tool execution with dependency graphs** — The harness executes all tool calls in a round in parallel via `join_all`. This is correct for independent calls but doesn't handle same-round dependencies. LLMs rarely produce dependent calls in a single round today, but as tool sets grow, it becomes a concern. **Goal:** support optional `depends_on` annotations or sequential fallback mode.

**10. Crate rename** — Completed. The crate has been renamed from `openrouter` to `cinch-rs`. The directory is `crates/cinch-rs/`, the package name is `cinch-rs`, and all imports use `cinch_rs::`. The CLI binary is now `cinch`.

## Non-goals

- **Fine-tuning or training.** The harness operates at inference time. Learning happens through reflexion memories and prompt engineering, not weight updates.
- **Multi-provider failover within a single request.** OpenRouter already handles provider routing and failover. The harness doesn't need to replicate this.
- **GUI or web interface.** The harness provides generic UI state management (`ui` module) and traits, but specific rendering is the caller's or companion crate's responsibility (`cinch-tui` for terminal, future `cinch-web` for web).
- **Prompt optimization or DSPy-style compilation.** The system prompt is the caller's domain. The harness provides context management, not prompt engineering.
- **Agent-to-agent communication.** The harness supports hierarchical sub-agent delegation (parent spawns child, child returns result). Peer-to-peer message passing, message buses, and multi-agent orchestration patterns (pipelines, ensembles, debates) are out of scope. Complex multi-agent workflows are the caller's responsibility to compose from individual harness runs.

## Summary

The cinch-rs agent framework is a Rust-native, model-agnostic, opinionated agent framework that handles the hard parts of building LLM agents: tool dispatch, context budgeting, cost tracking, retry logic, cross-session learning, sub-agent orchestration, streaming, human-in-the-loop approval, tool result caching, and dependency-aware tool execution. All modules are active by default with sensible defaults — `Harness::new(&client, &tools, config).run(messages)` gives you the full framework. The only remaining gap is further refinement of the dependency-graph tool execution to support richer annotation formats beyond `depends_on`.
