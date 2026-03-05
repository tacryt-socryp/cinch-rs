# Pi-Mono Deep Analysis: Lessons for cinch-rs

**Date:** 2026-03-05
**Repository:** https://github.com/badlogic/pi-mono (~18k stars)
**Author:** Mario Zechner (creator of libGDX)
**Language:** TypeScript monorepo (npm workspaces)
**License:** MIT

---

## 1. Overview

Pi-mono is an AI agent toolkit whose flagship product is **Pi**, a minimal terminal
coding agent (comparable to Claude Code). Its philosophy: "you adapt Pi to your
workflows rather than the other way around" via TypeScript Extensions, Skills,
Prompt Templates, and Themes.

### Monorepo Packages

| Package | Purpose | LOC |
|---------|---------|-----|
| **pi-ai** | Multi-provider LLM API layer (9 wire protocols, 300+ models) | ~21k |
| **pi-agent-core** | Agent loop, tool dispatch, context management | ~1.5k |
| **pi-coding-agent** | Terminal coding agent CLI with tools, extensions, skills | Large |
| **pi-tui** | Custom terminal UI framework (differential rendering) | Medium |
| **pi-web-ui** | Browser UI (Lit + Tailwind) for chat panels | Medium |
| **pi-mom** | Slack bot forwarding to agent sessions | Small |
| **pi-pods** | CLI for vLLM GPU pod management via SSH | Small |

### cinch-rs Equivalent Packages

| cinch-rs | pi-mono equivalent |
|----------|-------------------|
| `crates/cinch-rs` (lib) | `pi-ai` + `pi-agent-core` combined |
| `crates/cinch-code` | `pi-coding-agent` |
| `crates/cinch-tui` | `pi-tui` |
| `crates/cinch-web` | `pi-web-ui` |

---

## 2. Architecture Comparison

### 2.1 Agent Loop

**Pi-mono** (`pi-agent-core`, ~1,500 LOC):
- Dual-entry: `agentLoop()` (new conversation) and `agentLoopContinue()` (resume)
- **Nested loop pattern**: outer loop for follow-up messages, inner loop for tool
  execution + steering messages
- Tools executed **sequentially** with steering check after each tool
- Two-phase message transform: `transformContext()` → `convertToLlm()`
- Partial messages added to context immediately during streaming

**cinch-rs** (`agent/harness.rs`):
- Single `Harness::run()` entry with round-based loop
- Tools executed in **parallel** via DAG-aware scheduler
- Context management built into the loop (eviction, summarization)
- Streaming via SSE parser

**Key Difference:** Pi-mono's loop is intentionally minimal (~400 LOC). It pushes
context management, error recovery, and custom message handling to the app layer
via callbacks. cinch-rs bakes these concerns into the harness itself.

#### Lesson: The `transformContext` / `convertToLlm` Two-Phase Pattern

Pi-mono's cleanest design insight is separating context transforms into two phases:

```
AgentMessage[] → transformContext() → AgentMessage[] → convertToLlm() → LLMMessage[]
```

- **`transformContext`**: Operates on app-level messages. Handles pruning, eviction,
  token budgets. Can see custom message types.
- **`convertToLlm`**: Converts to wire format. Filters out UI-only messages, maps
  custom types to standard roles.

This separation means context management code never needs to know about wire
formats, and wire format code never needs to know about context strategies.

**Recommendation for cinch-rs:** Consider a similar two-phase approach in the
harness. Currently, eviction/summarization and API serialization are interleaved.
A `ContextTransform` trait that operates on `Message` before API conversion would
make context strategies composable and testable in isolation.

### 2.2 Steering & Follow-up Messages

Pi-mono introduces two callback-based injection points:

```typescript
getSteeringMessages?: () => Promise<AgentMessage[]>  // After each tool
getFollowUpMessages?: () => Promise<AgentMessage[]>   // At loop end
```

With mode control: `"one-at-a-time"` | `"all"`.

This allows the UI to queue messages during long tool executions without blocking.
The agent picks them up at natural breakpoints.

**Recommendation for cinch-rs:** The `EventHandler` trait could be extended with a
`steering_messages()` method that returns `Vec<Message>`. This would enable
interrupt-and-redirect during multi-tool rounds without aborting the entire run.

### 2.3 Tool System

**Pi-mono tools:**
```typescript
interface AgentTool<TParameters, TDetails> extends Tool {
  label: string;
  execute: (toolCallId, params, signal?, onUpdate?) => Promise<AgentToolResult<TDetails>>;
}
```

Key features:
- Factory function pattern: `createReadTool(cwd, options?)` returns `AgentTool`
- **Pluggable operations**: e.g., `ReadToolOptions.operations` can override file I/O
  for SSH/remote systems
- `onUpdate` callback for streaming partial results during execution
- Sequential execution with per-tool steering interruption
- Default tool set: `read`, `bash`, `edit`, `write` (only 4 by default!)

**cinch-rs tools:**
```rust
trait Tool: Send + Sync {
    fn definition(&self) -> ToolDef;
    fn execute(&self, input: &str) -> ToolFuture;
    fn cacheable(&self) -> bool { false }
    fn mutates_state(&self) -> bool { false }
}
```

Key features:
- DAG-aware parallel execution
- Result caching with FNV-1a hashing
- Read-before-write enforcement
- Token budget tracking per tool
- Richer built-in set: `ReadFile`, `Shell`, `Grep`, `FindFiles`, `ListDir`, `SaveDraft`

**Key Differences:**

| Aspect | Pi-mono | cinch-rs |
|--------|---------|----------|
| Execution | Sequential | Parallel (DAG) |
| Partial results | `onUpdate` callback | Not supported |
| Caching | Not built-in | FNV-1a hash cache |
| Operations | Pluggable I/O backends | Fixed implementations |
| Default count | 4 tools | 6+ tools |

#### Lessons:

1. **Pluggable operations pattern**: Pi-mono's ability to swap file I/O backends
   per-tool is elegant for remote/SSH scenarios. cinch-rs could add an `IoBackend`
   trait that tools accept, enabling `LocalIo`, `SshIo`, `ContainerIo` variants.

2. **Streaming tool results** (`onUpdate`): For long-running tools like `bash`,
   streaming partial output to the LLM context is valuable. cinch-rs's `ToolFuture`
   could be extended to yield intermediate results via a channel.

3. **Minimal default tools**: Pi starts with only 4 tools and lets extensions add
   more. This "less is more" approach reduces prompt token overhead and cognitive
   load on the LLM.

### 2.4 Context Management

**Pi-mono**: Fully delegated to the app layer via `transformContext`. No built-in
eviction or summarization. The README suggests:
```typescript
transformContext: async (messages) => {
  if (estimateTokens(messages) > MAX_TOKENS) {
    return pruneOldMessages(messages);
  }
  return messages;
}
```

**cinch-rs**: Rich built-in system:
- Three-zone message layout (pinned / working / recent)
- Old tool result eviction
- LLM-based incremental summarization
- `ContextBudget` with 60%/80% warning thresholds
- File access tracking

**Assessment:** cinch-rs is significantly ahead here. Pi-mono's delegation
approach is flexible but pushes complexity to every consumer. cinch-rs's
opinionated defaults with override capability is the better design for a framework.

### 2.5 Multi-Provider AI Layer

**Pi-mono** (`pi-ai`, ~21k LOC):
- 9 wire protocols: OpenAI Completions, OpenAI Responses, Azure OpenAI, Codex,
  Anthropic Messages, Bedrock, Google Generative AI, Gemini CLI, Vertex
- 20+ providers including OpenRouter, xAI, Groq, Cerebras, Mistral, etc.
- 300+ model definitions in generated `models.generated.ts`
- Registry pattern: `registerApiProvider()` / `getApiProvider()`
- Unified `Model<TApi>` type with cost, context window, capabilities
- `OpenAICompletionsCompat` flags for handling provider quirks

**cinch-rs**:
- OpenRouter-only via `OpenRouterClient`
- `RoutingStrategy` for per-round model selection
- Per-model pricing tables in `api/tracing.rs`

**Key Insight from Pi-mono:** The `OpenAICompletionsCompat` struct is gold:
```typescript
interface OpenAICompletionsCompat {
  supportsStore?: boolean;
  supportsDeveloperRole?: boolean;
  supportsReasoningEffort?: boolean;
  supportsUsageInStreaming?: boolean;
  maxTokensField?: "max_completion_tokens" | "max_tokens";
  requiresToolResultName?: boolean;
  requiresAssistantAfterToolResult?: boolean;
  requiresThinkingAsText?: boolean;
  requiresMistralToolIds?: boolean;
  thinkingFormat?: "openai" | "zai" | "qwen";
}
```

This captures the reality that "OpenAI-compatible" APIs are never truly compatible.
Each flag documents a specific provider divergence.

**Recommendation for cinch-rs:** When/if expanding beyond OpenRouter:
1. Adopt a provider registry pattern with `ApiProvider` trait
2. Use a `ProviderCompat` struct to capture known divergences
3. Generate model catalogues from a data file (as pi-mono does with
   `models.generated.ts`)

### 2.6 Extension System

**Pi-mono's extension system is its crown jewel.** It provides:

1. **Event-driven hooks**: `tool_call`, `tool_result`, `session_start`,
   `before_agent_start`, `context_usage`, etc.
2. **Tool registration**: Extensions can add custom LLM-callable tools
3. **Command registration**: `/command` invocations from the user
4. **Keyboard shortcuts**: Custom key bindings
5. **CLI flags**: Extensions can register `--custom-flag` options
6. **UI widgets**: Headers, footers, overlays, dialogs
7. **Provider registration**: Add custom LLM providers at runtime
8. **Blocking gates**: `tool_call` handlers can return `{block: true}` to prevent
   execution

Example:
```typescript
export default function (pi: ExtensionAPI) {
  pi.on("tool_call", async (event, ctx) => {
    if (event.toolName === "bash" && event.input.command?.includes("rm -rf")) {
      const ok = await ctx.ui.confirm("Dangerous!", "Allow rm -rf?");
      if (!ok) return { block: true, reason: "Blocked by user" };
    }
  });

  pi.registerTool({
    name: "greet",
    parameters: Type.Object({ name: Type.String() }),
    execute: async (id, params) => ({
      content: [{type: "text", text: `Hello, ${params.name}!`}],
      details: {}
    })
  });
}
```

**cinch-rs equivalent**: The `EventHandler` trait + `hooks.rs` provide lifecycle
observation, but lack:
- Runtime tool registration
- Event-based blocking/approval gates
- Custom CLI flag registration
- UI widget injection

**Recommendation for cinch-rs:** The extension system is the biggest gap. A phased
approach:

1. **Phase 1**: Add `ToolCallGuard` trait that can block/approve tool calls
   (subsumes human-in-the-loop)
2. **Phase 2**: Allow runtime `ToolSet` modification via `EventHandler` (dynamic
   tool registration)
3. **Phase 3**: Plugin system using Rust dynamic libraries or WASM modules
   (equivalent to pi-mono's jiti-loaded TypeScript)

### 2.7 Session Persistence

**Pi-mono**: JSONL-based session format with:
- Message entries
- Branch entries (fork points for conversation trees)
- Compaction entries (context summarization snapshots)
- Custom entries (extension data via `appendEntry()`)
- Session tree navigation (branch/fork/navigate)

**cinch-rs**: Checkpoint system with serializable round state in
`agent/checkpoint.rs` + session directories.

**Recommendation:** Consider adopting JSONL append-only format for sessions.
Benefits: crash-safe (each line is atomic), streamable, easy to inspect with
standard tools. Branching support is particularly valuable for exploratory coding
sessions where the user wants to try multiple approaches.

### 2.8 Prompt Assembly

**Pi-mono**:
- Template-based with dynamic guidelines based on active tools
- Project context from `AGENTS.md`/`CLAUDE.md` walked up directory hierarchy
- Skills loaded on-demand and injected into system prompt
- Prompt templates: YAML frontmatter + markdown, with `$1`, `$@` arg substitution

**cinch-rs**:
- `SystemPromptBuilder` with `PromptRegistry` for cache-aware section ordering
- `ReminderRegistry` for mid-conversation injections
- `project_instructions.rs` for AGENTS.md hierarchy

**Lesson:** Pi-mono's prompt templates with argument substitution are a nice
user-facing feature. The `/template-name [args]` pattern lets users create reusable
prompts. cinch-rs could add a similar `PromptTemplate` type loaded from
`.cinch/prompts/*.md`.

---

## 3. Design Philosophy Comparison

| Principle | Pi-mono | cinch-rs |
|-----------|---------|----------|
| **Core loop** | Minimal, delegate to app | Opinionated, batteries-included |
| **Context mgmt** | App responsibility | Framework responsibility |
| **Tools** | 4 default, extend via plugins | 6+ built-in, extend via trait |
| **Extensibility** | Rich TypeScript extension API | Trait-based composition |
| **Provider support** | 20+ providers, 9 APIs | OpenRouter gateway |
| **UI** | Custom TUI framework + Lit web | ratatui + axum |
| **Session format** | JSONL append-only | Checkpoint serialization |
| **Error recovery** | App-layer retry | Built-in retry with backoff |

Pi-mono follows the **"library, not framework"** philosophy for its core, while
cinch-rs follows the **"opinionated framework"** philosophy. Both are valid;
cinch-rs's approach is arguably better for Rust where the type system can enforce
invariants that would be error-prone in a delegated callback model.

---

## 4. Top Takeaways for cinch-rs

### Must-Have (High Impact, Aligned with Current Architecture)

1. **Two-phase context transform** (`transformContext` / `convertToLlm` separation)
   - Decouples context strategy from API serialization
   - Makes context management composable and testable
   - Could be a `ContextTransform` trait in cinch-rs

2. **Steering messages** (inject messages mid-execution)
   - Enables interrupt-and-redirect without aborting
   - Natural extension of `EventHandler`
   - Critical for interactive use cases

3. **Tool call guards** (blocking approval gates)
   - Pi-mono's `tool_call` event with `{block: true}` return
   - Generalizes human-in-the-loop beyond just the UI
   - Could be a `ToolCallGuard` trait

4. **Streaming tool results** (`onUpdate` callback pattern)
   - Long-running bash commands should stream output
   - Improves UX for progress visibility
   - Could extend `ToolFuture` with a channel

### Should-Have (Medium Impact, Nice Additions)

5. **Pluggable I/O backends for tools**
   - `ReadToolOptions.operations` pattern enables SSH/container scenarios
   - `IoBackend` trait that tools accept
   - Opens door to remote agent execution

6. **JSONL session format with branching**
   - Crash-safe, append-only, inspectable
   - Branching enables exploratory workflows
   - Standard format is easier to build tooling around

7. **Prompt templates with argument substitution**
   - User-facing `.cinch/prompts/*.md` files
   - `/template-name [args]` invocation
   - Low implementation cost, high user value

8. **Provider compatibility flags**
   - `ProviderCompat` struct documenting API divergences
   - Essential if expanding beyond OpenRouter
   - Pi-mono's `OpenAICompletionsCompat` is a battle-tested model

### Could-Have (Lower Priority, Future Direction)

9. **Dynamic tool registration at runtime**
   - Allow extensions/plugins to add tools
   - Requires `ToolSet` to be mutable during session
   - Foundation for a plugin ecosystem

10. **Custom message types** (Pi-mono's declaration merging pattern)
    - Allow apps to define domain-specific message types
    - Filtered before reaching LLM via `convertToLlm`
    - In Rust: enum variants with `#[non_exhaustive]` or trait objects

11. **Extension/plugin system**
    - Full pi-mono-style extension API is a large undertaking
    - Consider WASM-based plugins for sandboxed extensibility
    - Or simpler: Lua/Rhai scripting for tool definitions

---

## 5. Code Quality & Size Comparison

| Metric | Pi-mono | cinch-rs |
|--------|---------|----------|
| Core agent loop | ~1,500 LOC (TS) | ~2,000+ LOC (Rust) |
| AI/API layer | ~21,000 LOC (TS) | ~2,000 LOC (Rust, OpenRouter only) |
| Coding agent | Large | ~17,600 LOC |
| Total workspace | Very large (5+ packages) | ~17,600 LOC (4 crates) |
| Test coverage | Unit + E2E with mock streams | Unit tests |
| Dependencies | npm ecosystem | Cargo ecosystem |

Pi-mono's codebase is significantly larger due to multi-provider support and the
rich extension system. cinch-rs is more compact and focused, which is appropriate
for its current scope.

---

## 6. What Pi-Mono Does Better

1. **Extension ecosystem**: The TypeScript extension API is comprehensive and
   enables a plugin marketplace. cinch-rs has no equivalent.
2. **Provider breadth**: 9 wire protocols and 20+ providers vs. OpenRouter only.
3. **Session branching**: Conversation tree navigation for exploring alternatives.
4. **Prompt templates**: User-definable reusable prompts with arg substitution.
5. **Tool interception**: Extensions can block, modify, or augment any tool call.

## 7. What cinch-rs Does Better

1. **Context management**: Three-zone layout, eviction, LLM summarization — all
   built-in with sensible defaults.
2. **Parallel tool execution**: DAG-aware scheduling vs. sequential-only.
3. **Tool result caching**: FNV-1a hash-based deduplication saves tokens.
4. **Cost tracking**: Per-model pricing with budget alerts is first-class.
5. **Read-before-write enforcement**: Prevents blind file overwrites.
6. **Plan-execute workflows**: Two-phase planning is built into the harness.
7. **Sub-agent delegation**: Token-budgeted recursive agents.
8. **Type safety**: Rust's type system catches entire categories of bugs that
   TypeScript's type system permits.

---

## 8. Architectural Diagram: Pi-Mono

```
┌─────────────────────────────────────────────────────┐
│                   pi-coding-agent                    │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌────────┐ │
│  │Extensions│ │  Skills  │ │ Prompts  │ │ Themes │ │
│  └────┬─────┘ └────┬─────┘ └────┬─────┘ └───┬────┘ │
│       │             │            │            │      │
│  ┌────▼─────────────▼────────────▼────────────▼────┐ │
│  │              Agent Session                      │ │
│  │  ┌─────────────────────────────────────────┐    │ │
│  │  │ Extension Runner (event dispatch)       │    │ │
│  │  └─────────────────┬───────────────────────┘    │ │
│  │                    │                             │ │
│  │  ┌─────────────────▼───────────────────────┐    │ │
│  │  │ Tool Wrapper (intercept + guard)        │    │ │
│  │  └─────────────────┬───────────────────────┘    │ │
│  └────────────────────┼────────────────────────────┘ │
│                       │                              │
│  ┌────────────────────▼────────────────────────────┐ │
│  │             Built-in Tools                      │ │
│  │   read │ bash │ edit │ write │ grep │ find │ ls │ │
│  └─────────────────────────────────────────────────┘ │
└──────────────────────┬──────────────────────────────┘
                       │
┌──────────────────────▼──────────────────────────────┐
│                  pi-agent-core                       │
│  ┌──────────────────────────────────────────────┐   │
│  │           Agent Loop (nested)                │   │
│  │  outer: follow-up messages                   │   │
│  │  inner: tool execution + steering            │   │
│  │                                              │   │
│  │  transformContext() → convertToLlm() → LLM   │   │
│  └──────────────────────────────────────────────┘   │
└──────────────────────┬──────────────────────────────┘
                       │
┌──────────────────────▼──────────────────────────────┐
│                     pi-ai                            │
│  ┌──────────────────────────────────────────────┐   │
│  │         API Provider Registry                │   │
│  │  ┌─────────┐ ┌─────────┐ ┌─────────┐       │   │
│  │  │OpenAI   │ │Anthropic│ │ Google  │ ...    │   │
│  │  │Compltns │ │Messages │ │Generatv │       │   │
│  │  └─────────┘ └─────────┘ └─────────┘       │   │
│  └──────────────────────────────────────────────┘   │
│  ┌──────────────────────────────────────────────┐   │
│  │    Model Registry (300+ models, generated)   │   │
│  └──────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────┘
```

---

## 9. Conclusion

Pi-mono and cinch-rs are solving the same problem (LLM-powered coding agents) with
different philosophies. Pi-mono excels at **extensibility and provider breadth**
while cinch-rs excels at **context management, parallel execution, and type safety**.

The highest-ROI improvements for cinch-rs, inspired by pi-mono, are:

1. **Steering messages** — enables mid-execution user intervention
2. **Tool call guards** — generalizes approval gates
3. **Two-phase context transforms** — cleaner architecture
4. **Streaming tool results** — better UX for long operations

These four changes would bring the best of pi-mono's interactivity model into
cinch-rs without sacrificing its opinionated, batteries-included approach.
