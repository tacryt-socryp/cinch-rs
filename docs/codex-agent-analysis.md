# OpenAI Codex CLI: Agent Context, Memory & Compaction Analysis

> Comprehensive analysis of the [openai/codex](https://github.com/openai/codex) codebase — how it manages agent context, persistent memory, and conversation compaction.
>
> Based on the `codex-rs` Rust core implementation.

---

## Table of Contents

1. [Architecture Overview](#1-architecture-overview)
2. [Context Management](#2-context-management)
3. [Compaction System](#3-compaction-system)
4. [Persistent Memory](#4-persistent-memory)
5. [Sub-Agent & Thread Model](#5-sub-agent--thread-model)
6. [Key Takeaways for cinch-rs](#6-key-takeaways-for-cinch-rs)

---

## 1. Architecture Overview

Codex uses a **session → turn → sampling loop** architecture:

```
Session (persistent state, rollout recorder)
└─ Turn (per user-input cycle)
   └─ Sampling Loop (repeated LLM calls within a turn)
      ├─ Tool execution & result recording
      ├─ Mid-turn compaction (if token limit hit)
      └─ Continue until no more follow-ups needed
```

### Key Structures

| Structure | File | Purpose |
|-----------|------|---------|
| `Session` | `core/src/codex.rs:525` | Persistent session-scoped state |
| `SessionState` | `core/src/state/session.rs:17` | History, rate limits, model tracking |
| `TurnContext` | `protocol/src/protocol.rs:2102` | Per-turn config: model, sandbox, approval, personality |
| `TurnState` | `core/src/state/turn.rs:70` | Pending approvals, user input, dynamic tools |
| `ContextManager` | `core/src/context_manager/history.rs:24` | Ordered conversation history + token tracking |

### Session State

```rust
pub struct SessionState {
    pub history: ContextManager,                    // Conversation items (oldest → newest)
    pub latest_rate_limits: Option<RateLimitSnapshot>,
    pub previous_model: Option<String>,             // For model-switch detection
    pub dependency_env: HashMap<String, String>,
    pub active_mcp_tool_selection: Option<Vec<String>>,
    pub active_connector_selection: HashSet<String>,
    // ...
}
```

---

## 2. Context Management

### 2.1 Message Types

The conversation is a `Vec<ResponseItem>` with these variants (`protocol/src/models.rs:93-196`):

| Variant | Description |
|---------|-------------|
| `Message` | User/assistant/developer text (with role, content items, phase) |
| `Reasoning` | Extended thinking (can be encrypted) |
| `FunctionCall` | Tool invocation (name, arguments, call_id) |
| `FunctionCallOutput` | Tool result (text/image content) |
| `LocalShellCall` | Shell command execution |
| `WebSearchCall` | Web search actions |
| `GhostSnapshot` | File state snapshots (for `/undo`) |
| `Compaction` | Summarized context replacement marker |

### 2.2 Prompt Assembly

The `Prompt` struct packages everything for the API (`core/src/client_common.rs:27-45`):

```rust
pub struct Prompt {
    pub input: Vec<ResponseItem>,          // Full conversation history
    pub tools: Vec<ToolSpec>,              // Available tools
    pub parallel_tool_calls: bool,
    pub base_instructions: BaseInstructions, // System prompt
    pub personality: Option<Personality>,   // Friendly | Pragmatic | None
    pub output_schema: Option<Value>,
}
```

History is prepared via `ContextManager::for_prompt()` which:
1. Normalizes message ordering (pairs function calls with outputs)
2. Filters out ghost snapshots
3. Strips image content if model doesn't support images

### 2.3 System Prompt Construction

System instructions are model-specific with personality interpolation (`protocol/src/openai_models.rs:283-302`):

```rust
pub fn get_model_instructions(&self, personality: Option<Personality>) -> String {
    // Template substitution: "{{ personality }}" → personality-specific text
    // Falls back to base_instructions if no template
}
```

Additional developer instructions injected per-turn:
- Collaboration mode instructions (default/plan/execute/pair_programming)
- Memory system instructions (if `MemoryTool` feature enabled)
- Skills/connectors discovered from user input
- MCP tool descriptions

### 2.4 Token Tracking & Budget

**Token Usage** (`protocol/src/protocol.rs:1469-1489`):

```rust
pub struct TokenUsageInfo {
    pub total_token_usage: TokenUsage,      // Cumulative across session
    pub last_token_usage: TokenUsage,       // Last API response
    pub model_context_window: Option<i64>,  // Model's max context
}
```

**Effective Context Window** (`core/src/codex.rs:579-582`):

```rust
fn model_context_window(&self) -> Option<i64> {
    // effective_context_window_percent defaults to 95%
    context_window * effective_context_window_percent / 100
}
```

**Baseline Token Reserve**: 12,000 tokens reserved for system prompt + tool instructions, so percentage calculations reflect user-controllable context only.

**Token Estimation**: Uses byte-based heuristic at 4 bytes/token (`core/src/truncate.rs:10`). Not exact tokenization — a coarse lower bound.

---

## 3. Compaction System

Codex has a **two-tier compaction system**: local (for non-OpenAI providers) and remote (for OpenAI, using a dedicated API endpoint).

### 3.1 Compaction Triggers

There are **three trigger points** in the conversation loop:

| Trigger | When | Injection Strategy | Location |
|---------|------|-------------------|----------|
| **Pre-turn** | Before sampling, if `total_tokens >= auto_compact_token_limit` | `DoNotInject` (reinjects naturally next turn) | `codex.rs:4870-4891` |
| **Mid-turn** | After a response, if token limit hit AND model needs follow-up (pending tool call) | `BeforeLastUserMessage` (same turn continues) | `codex.rs:4738-4772` |
| **Manual** | User runs `/compact` command | `DoNotInject` | `codex.rs:3491` |
| **Model switch** | When switching to a smaller context-window model | Same as pre-turn | `codex.rs:4899-4936` |

### 3.2 Local Compaction Algorithm

File: `core/src/compact.rs` (~485 lines)

```
1. Extract and strip trailing model-switch items
2. Record input into history
3. Loop with context window overflow handling:
   a. If context window exceeded, remove oldest item and retry
   b. Collect user messages (excluding prior summaries)
   c. Get last assistant message as summary basis
   d. Build replacement history with backward token-aware selection
4. Reinject initial context based on injection strategy
5. Restore model-switch items and ghost snapshots
6. Persist to rollout
```

**Backward Token-Aware Selection** (`compact.rs:384-407`):
- Iterates user messages in **reverse chronological order**
- Accumulates messages until token budget exhausted
- Reverses back to preserve chronological order
- Individual messages truncated at `COMPACT_USER_MESSAGE_MAX_TOKENS = 20,000`

**Summary Generation**: The compaction prompt (`templates/compact/prompt.md`) asks the model to create a handoff summary:

> Create a handoff summary for another LLM that will resume the task. Include: current progress, key decisions, user preferences, remaining work, critical references.

The summary is prefixed with a marker (`SUMMARY_PREFIX`) so future compaction rounds can distinguish summaries from real user messages.

### 3.3 Remote Compaction (OpenAI Only)

File: `core/src/compact_remote.rs` (~296 lines)

Decision point (`compact.rs:50-52`):
```rust
pub fn should_use_remote_compact_task(provider: &ModelProviderInfo) -> bool {
    provider.is_openai()
}
```

Remote compaction delegates to OpenAI's `/v1/responses/compact` API endpoint, then post-processes locally:

1. Trim function call history to fit context window (removes **last items first** — opposite of local)
2. Send structured JSON to remote API
3. Filter returned history:
   - **Keep**: Assistant messages, compaction items, real user messages
   - **Drop**: Stale developer messages, session metadata, tool calls, reasoning blocks
4. Reinject fresh initial context (permissions, instructions)

### 3.4 What Gets Preserved vs Discarded

| Preserved | Discarded |
|-----------|-----------|
| All real user messages (up to token limit, newest first) | Assistant responses (compressed to summary) |
| The compaction summary (synthetic user message) | Tool call/output sequences |
| Canonical initial context (permissions, environment) | Stale developer messages |
| Ghost snapshots (for `/undo`) | Session metadata messages |
| Model-switch updates | Intermediate reasoning blocks |
| Prior compaction items | Oldest history when budget exceeded |

### 3.5 Context Reinjection Strategies

The `InitialContextInjection` enum controls how canonical context is placed after compaction:

- **`DoNotInject`** (pre-turn/manual): Clears reference context. Next regular turn naturally reinjects it.
- **`BeforeLastUserMessage`** (mid-turn): Injects initial context *above* the last user message so the model sees it in the same continuing turn.

Placement logic (`insert_initial_context_before_last_real_user_or_summary`, `compact.rs:330-369`):
1. Prefer: before last REAL user message
2. Fallback: before last summary message
3. Fallback: before last compaction item
4. Fallback: append to end

---

## 4. Persistent Memory

Codex has a sophisticated **two-phase asynchronous memory pipeline** that runs at session startup.

### 4.1 Storage Layout

```
~/.codex/memories/
├── raw_memories.md              # Merged phase-1 outputs (temporary, input for phase 2)
├── MEMORY.md                    # Consolidated handbook (task-grouped registry)
├── memory_summary.md            # High-level user profile & index (injected into prompts)
├── rollout_summaries/           # Per-rollout recaps
│   └── 2025-02-11T15-35-19-jqmb-task_name.md
└── skills/                      # Optional reusable procedures
    └── skill-name/SKILL.md
```

### 4.2 Phase 1: Rollout Extraction

File: `core/src/memories/phase1.rs`

Runs at session startup for non-ephemeral root sessions:

1. **Claim eligible rollouts** from state DB (max 16/startup, 30-day age window, 6-hour idle minimum)
2. **Run up to 8 jobs in parallel** using a lightweight model (`gpt-5.1-codex-mini`, Low reasoning effort)
3. Each job processes a rollout and extracts structured JSON:
   - `raw_memory`: YAML-frontmatter task blocks with learnings
   - `rollout_summary`: Comprehensive task recap (context, preferences, tasks, learnings)
   - `rollout_slug`: Short descriptive slug for filename
4. Results stored in state DB

**Key constants**:
```rust
const MODEL: &str = "gpt-5.1-codex-mini";
const REASONING_EFFORT: ReasoningEffort = ReasoningEffort::Low;
const CONCURRENCY_LIMIT: usize = 8;
const DEFAULT_STAGE_ONE_ROLLOUT_TOKEN_LIMIT: usize = 150_000;
const CONTEXT_WINDOW_PERCENT: i64 = 70;
```

### 4.3 Phase 2: Global Consolidation

File: `core/src/memories/phase2.rs`

Single globally-locked consolidation process:

1. **Claim global lock** (serialized — only one consolidation at a time)
2. **Load stage-1 outputs** (up to 1,024 memories)
3. **Sync filesystem artifacts**: write rollout summary files, rebuild `raw_memories.md`
4. **Spawn consolidation agent** (`gpt-5.3-codex`, Medium reasoning effort) that:
   - Reads all inputs (raw_memories, existing MEMORY.md, rollout summaries)
   - Produces updated `MEMORY.md`, `memory_summary.md`, and optional `skills/`
5. **Heartbeat every 90 seconds** to prevent job timeout

### 4.4 Memory Injection into Prompts

File: `core/src/codex.rs:2753-2762`

Every turn (if `MemoryTool` feature enabled):
1. Read `memory_summary.md` from disk
2. Truncate to 5,000 tokens
3. Wrap in template (`templates/memories/read_path.md`) describing:
   - Memory folder layout
   - Decision boundary for when to consult memory
   - Quick memory search procedure
4. Inject as `DeveloperInstructions`

The template instructs the agent to search `MEMORY.md` and referenced rollout summaries when queries match stored topics — memory is treated as **guidance, not truth**.

### 4.5 MEMORY.md Format

Task-grouped structure:

```markdown
# Task Group: <repo/project/workflow>
scope: <operational description>

## Task 1: <task description, outcome>
task: <searchable signature>

### rollout_summary_files
- rollout_summaries/TIMESTAMP-HASH-slug.md (cwd=<path>)

### keywords
- <retrieval handles>

### learnings
- <task-specific insights>

## General Tips
- <cross-task guidance>
```

### 4.6 memory_summary.md Format

```markdown
## User Profile
<vivid snapshot of user>

## General Tips
<durable, actionable guidance>

## What's in Memory
- <topic>: <keywords>
  - desc: <description>
```

### 4.7 Configuration

```rust
pub struct MemoriesToml {
    pub max_raw_memories_for_global: Option<usize>,  // default 1024, max 4096
    pub max_rollout_age_days: Option<i64>,            // default 30, max 90
    pub max_rollouts_per_startup: Option<usize>,      // default 16, max 128
    pub min_rollout_idle_hours: Option<i64>,           // default 6, range 1-48
    pub phase_1_model: Option<String>,                 // default gpt-5.1-codex-mini
    pub phase_2_model: Option<String>,                 // default gpt-5.3-codex
}
```

---

## 5. Sub-Agent & Thread Model

### 5.1 Agent Spawning

File: `core/src/agent/control.rs`

```rust
pub struct AgentControl {
    manager: Weak<ThreadManagerState>,  // Global thread registry
    state: Arc<Guards>,                 // Spawn depth limits, naming
}
```

Methods:
- `spawn_agent()`: Creates new agent thread from config + items
- `resume_agent_from_rollout()`: Resumes existing agent from rollout file
- Agents get unique nicknames from a fixed list

### 5.2 Multi-Agent Tools

File: `core/src/tools/handlers/multi_agents.rs`

| Tool | Description |
|------|-------------|
| `spawn_agent` | Spawn new agent with role/message |
| `send_input` | Send input to running agent |
| `resume_agent` | Resume paused agent |
| `wait` | Wait for agent completion with timeout |
| `close_agent` | Terminate agent |

### 5.3 Delegation Patterns

File: `core/src/codex_delegate.rs`

- **Interactive mode**: Spawns child Codex instance with bidirectional IO channels. Events forwarded to parent; parent handles approval routing.
- **One-shot mode**: Wraps interactive mode. Auto-submits input, auto-shuts down on turn completion.

**Depth control**: Guards prevent infinite recursion via `thread_spawn_depth_limit` checking.

### 5.4 Session Resume & Fork

Sessions are persisted as **rollout files** in `$CODEX_HOME/sessions/`:
- Full event history serialized to disk
- Resume: rollout deserialized and replayed, previous model tracked for context reinjection
- Fork: full context trees cloned from parent session state

---

## 6. Key Takeaways for cinch-rs

### Context Management Patterns Worth Adopting

1. **Byte-based token estimation** (4 bytes/token) is good enough for compaction decisions — no need for exact tokenization in the hot path.

2. **Effective context window = 95% of actual** — reserve 5% headroom for system prompts and tool overhead.

3. **Baseline token reserve** (12,000 tokens) ensures percentage calculations reflect user-controllable context.

4. **`for_prompt()` normalization** before API calls — clean up message ordering, pair tool calls with outputs, strip unsupported modalities.

### Compaction Patterns Worth Adopting

1. **Three trigger points** (pre-turn, mid-turn, manual) cover all scenarios. Mid-turn compaction is critical for long tool-call chains that exhaust context within a single turn.

2. **Backward token-aware selection** — prioritize newest user messages when building compacted history.

3. **Summary prefix markers** — distinguish compaction summaries from real user messages to prevent recursive summarization.

4. **Context reinjection strategy** varies by timing: pre-turn clears and lets natural reinjection happen; mid-turn explicitly places context before last user message.

5. **Ghost snapshots preserved across compaction** for undo support.

### Memory Patterns Worth Adopting

1. **Two-phase async pipeline**: lightweight extraction (phase 1, parallel, cheap model) then heavyweight consolidation (phase 2, serialized, capable model).

2. **Watermark-based dirty tracking** prevents duplicate work across consolidation runs.

3. **memory_summary.md (5K tokens) always in prompt** — cheap way to give agent awareness of what it knows. Full MEMORY.md only consulted on demand.

4. **Task-grouped structure with keywords** enables retrieval-oriented search rather than sequential reading.

5. **Rollout summaries as individual files** — granular, can be pruned independently, referenced by MEMORY.md entries.

### Architecture Patterns

1. **Session → Turn → Sampling Loop** is clean and well-separated.
2. **Rollout persistence** enables resume/fork without re-running anything.
3. **Depth-limited agent spawning** with unique nicknames prevents runaway recursion.
4. **Approval routing** through parent agents keeps security centralized.
