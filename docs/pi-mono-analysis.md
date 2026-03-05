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

---

## 10. Deep Dive: Tool-Use Architecture

This section examines the complete lifecycle of tool-use in pi-mono — from how
tools are defined and registered, through how the LLM is prompted to use them,
to how tool calls are parsed from streaming responses, validated, executed,
and reported back.

### 10.1 Tool Definition & Schema

Tools are defined using TypeBox JSON Schema with a typed `execute` function:

```typescript
// packages/ai/src/types.ts — base Tool (sent to LLM)
interface Tool<TParameters extends TSchema = TSchema> {
  name: string;
  description: string;
  parameters: TParameters;  // JSON Schema via @sinclair/typebox
}

// packages/agent/src/types.ts — executable AgentTool
interface AgentTool<TParameters, TDetails> extends Tool<TParameters> {
  label: string;  // Human-readable for UI
  execute: (
    toolCallId: string,
    params: Static<TParameters>,  // Type-safe params from schema
    signal?: AbortSignal,
    onUpdate?: AgentToolUpdateCallback<TDetails>,
  ) => Promise<AgentToolResult<TDetails>>;
}
```

Example — the edit tool schema:
```typescript
const editSchema = Type.Object({
  path: Type.String({ description: "Path to the file to edit (relative or absolute)" }),
  oldText: Type.String({ description: "Exact text to find and replace (must match exactly)" }),
  newText: Type.String({ description: "New text to replace the old text with" }),
});
```

Key design: The `description` on each parameter field is included in the JSON
schema sent to the LLM, giving per-parameter guidance beyond the tool-level
description.

### 10.2 Extension Tool Registration

Extensions register tools through the `ToolDefinition` interface, which adds
two prompt-engineering fields not present on the base `AgentTool`:

```typescript
// packages/coding-agent/src/core/extensions/types.ts
interface ToolDefinition<TParams, TDetails> {
  name: string;
  label: string;
  description: string;            // Sent to LLM in tool schema

  // These control how the system prompt references this tool:
  promptSnippet?: string;          // One-liner for "Available tools:" section
  promptGuidelines?: string[];     // Bullet points for "Guidelines:" section

  parameters: TParams;
  execute(...): Promise<AgentToolResult<TDetails>>;
  renderCall?: (...) => Component;    // Custom UI for tool invocations
  renderResult?: (...) => Component;  // Custom UI for tool results
}
```

The `promptSnippet` and `promptGuidelines` are the mechanism by which tools
influence how the LLM is instructed to use them. This is a three-layer approach:

| Layer | What | Where |
|-------|------|-------|
| `description` | Full tool description (sent as JSON schema to the LLM) | Wire protocol |
| `promptSnippet` | One-liner in "Available tools:" system prompt section | System prompt |
| `promptGuidelines` | Usage rules in "Guidelines:" system prompt section | System prompt |

### 10.3 System Prompt Construction for Tool-Use

The `buildSystemPrompt()` function (`packages/coding-agent/src/core/system-prompt.ts`)
dynamically assembles the system prompt based on which tools are active. This is
the core of how the LLM is instructed to use tools appropriately.

**Step 1: Tool listing.** Each active tool gets a one-line entry:

```
Available tools:
- read: Read file contents
- bash: Execute bash commands (ls, grep, find, etc.)
- edit: Make surgical edits to files (find exact text and replace)
- write: Create or overwrite files
- custom_tool: [promptSnippet or description fallback]
```

**Step 2: Conditional guidelines based on tool availability.** The system
generates different instructions depending on which tools are present:

```typescript
// If bash is available but grep/find/ls are NOT:
"Use bash for file operations like ls, rg, find"

// If bash AND specialized tools are both available:
"Prefer grep/find/ls tools over bash for file exploration (faster, respects .gitignore)"

// If read AND edit are both available:
"Use read to examine files before editing. You must use this tool instead of cat or sed."

// If edit is available:
"Use edit for precise changes (old text must match exactly)"

// If write is available:
"Use write only for new files or complete rewrites"

// If edit OR write is available:
"When summarizing your actions, output plain text directly - do NOT use cat or bash to display what you did"
```

This conditional approach means the LLM never gets instructions for tools it
doesn't have, and the guidelines adapt to the tool set's composition.

**Step 3: Extension-contributed guidelines.** Each active tool's
`promptGuidelines` are appended. For example, a custom tool might add:

```typescript
pi.registerTool({
  name: "deploy",
  promptGuidelines: [
    "Use deploy only when the user explicitly asks to deploy",
    "Always run tests before deploying"
  ],
  // ...
});
```

**Step 4: Fixed closing guidelines:**
```
- Be concise in your responses
- Show file paths clearly when working with files
```

**Step 5: Skills section** (XML-formatted, only when `read` tool is available):
```xml
<available_skills>
  <skill>
    <name>review-code</name>
    <description>Review code for bugs and style issues</description>
    <location>/path/to/review-code/SKILL.md</location>
  </skill>
</available_skills>
```

Skills tell the LLM: "Use the read tool to load a skill's file when the task
matches its description." This is a lazy-loading pattern — the full skill
instructions are not included in every prompt, only loaded on demand.

### 10.4 Tool Call Lifecycle

The full lifecycle of a tool call through the system:

```
1. LLM streams response with tool_call content blocks
   │
2. Provider parses streaming JSON via parseStreamingJson()
   │  Uses partial-json library for incomplete JSON during streaming
   │  Emits toolcall_start/toolcall_delta/toolcall_end events
   │
3. Final AssistantMessage contains ToolCall[] content blocks:
   │  { type: "toolCall", id: "...", name: "edit", arguments: {...} }
   │
4. Agent loop extracts tool calls from assistant message
   │  toolCalls = message.content.filter(c => c.type === "toolCall")
   │
5. For each tool call (sequentially):
   │
   ├─ 5a. Find tool: tools.find(t => t.name === toolCall.name)
   │      If not found → error: "Tool X not found"
   │
   ├─ 5b. Validate arguments via AJV against TypeBox schema:
   │      validateToolArguments(tool, toolCall)
   │      - Uses AJV with coerceTypes: true (auto-converts string→number etc.)
   │      - Clones arguments before validation (AJV mutates in-place)
   │      - Returns coerced args on success, throws formatted error on failure
   │
   ├─ 5c. Execute: tool.execute(toolCallId, validatedArgs, signal, onUpdate)
   │      - onUpdate callback streams partial results to UI
   │      - Signal enables abort/cancellation
   │
   ├─ 5d. Build ToolResultMessage:
   │      { role: "toolResult", toolCallId, toolName, content, isError, timestamp }
   │
   └─ 5e. Check for steering messages (user interruption):
          If steering messages queued → skip remaining tool calls
          Skipped tools get: "Skipped due to queued user message." (isError: true)
```

### 10.5 Streaming Tool Call Parsing

When the LLM streams a tool call, the arguments arrive as incremental JSON
chunks. Pi-mono uses the `partial-json` library to parse incomplete JSON:

```typescript
// packages/ai/src/utils/json-parse.ts
function parseStreamingJson<T>(partialJson: string | undefined): T {
  if (!partialJson || partialJson.trim() === "") return {} as T;

  // Try standard JSON.parse first (fastest for complete JSON)
  try { return JSON.parse(partialJson); } catch {}

  // Fall back to partial-json for incomplete JSON
  try { return partialParse(partialJson) ?? {}; } catch {}

  // If all parsing fails, return empty object
  return {} as T;
}
```

This allows the UI to show tool call arguments as they stream in, before the
LLM finishes the full JSON object. For example, the `bash` tool's `command`
field can be displayed as soon as it starts streaming.

Each provider handles streaming tool calls differently:

- **OpenAI Completions**: `delta.tool_calls[i].function.arguments` (JSON fragments)
- **Anthropic**: `content_block_delta` with `input_json_delta` (JSON fragments)
- **Google**: `functionCall.args` (complete object per chunk)
- **Bedrock**: `ContentBlockDeltaEvent` with `toolUse.input` (JSON fragments)

### 10.6 Tool Argument Validation

Validation happens through AJV (Another JSON Validator) with the TypeBox schema:

```typescript
// packages/ai/src/utils/validation.ts
function validateToolArguments(tool: Tool, toolCall: ToolCall): any {
  const validate = ajv.compile(tool.parameters);
  const args = structuredClone(toolCall.arguments);  // Clone for safe mutation

  if (validate(args)) return args;  // AJV coerces types in-place

  // Format errors with paths and messages
  throw new Error(`Validation failed for tool "${toolCall.name}":\n${errors}`);
}
```

Key configuration: `coerceTypes: true` means if the LLM sends `"42"` where a
number is expected, AJV auto-converts it to `42`. This compensates for a common
LLM failure mode where types are correct semantically but wrong syntactically.

### 10.7 Tool Error Handling

Errors during tool execution are caught and returned as `isError: true` tool
results. The LLM sees the error message as the tool's output:

```typescript
try {
  result = await tool.execute(toolCall.id, validatedArgs, signal, onUpdate);
} catch (e) {
  result = {
    content: [{ type: "text", text: e instanceof Error ? e.message : String(e) }],
    details: {},
  };
  isError = true;
}
```

Each built-in tool provides specific, actionable error messages:

- **edit**: "Could not find the exact text in {path}. The old text must match
  exactly including all whitespace and newlines."
- **edit**: "Found {n} occurrences of the text in {path}. The text must be
  unique. Please provide more context to make it unique."
- **edit**: "No changes made to {path}. The replacement produced identical
  content."
- **bash**: "Command timed out after {n} seconds"
- **bash**: "Command exited with code {n}" (includes the output)
- **read**: "Offset {n} is beyond end of file ({m} lines total)"

These error messages are crafted to help the LLM self-correct. For example,
the edit tool's "provide more context" message explicitly tells the LLM what
to do next when it encounters a non-unique match.

### 10.8 Tool Output Truncation

Both `read` and `bash` tools apply output truncation to prevent token explosion:

```typescript
// Default limits
const DEFAULT_MAX_LINES = 1000;
const DEFAULT_MAX_BYTES = 30 * 1024;  // 30KB
```

**Read tool** — head truncation (keeps first N lines):
```
[file content, lines 1-1000]

[Showing lines 1-1000 of 5432. Use offset=1001 to continue.]
```

**Bash tool** — tail truncation (keeps last N lines):
```
[last 1000 lines of output]

[Showing lines 4432-5432 of 5432. Full output: /tmp/pi-bash-abc123.log]
```

The truncation messages are designed as LLM-actionable instructions. They tell
the model exactly how to get more data (use `offset=X` for read, or access the
temp file for bash).

### 10.9 Tool Choice Configuration

Each provider supports `toolChoice` with slightly different semantics:

```typescript
// Anthropic
toolChoice?: "auto" | "any" | "none" | { type: "tool"; name: string };

// OpenAI Completions
toolChoice?: "auto" | "required" | "none" | { type: "function"; function: { name: string } };

// Google
toolChoice?: "auto" | "any" | "none";
```

The `any`/`required` option forces the LLM to make at least one tool call.
The `{ type: "tool"; name: string }` variant forces a specific tool — useful
for structured extraction or when the agent loop knows the next step.

### 10.10 Cross-Provider Tool Call ID Normalization

Different providers have different requirements for tool call IDs:

| Provider | ID Format | Max Length | Constraints |
|----------|-----------|------------|-------------|
| OpenAI Responses | 450+ chars, pipes, special chars | None | Generated by API |
| Anthropic | Alphanumeric + `_-` | 64 chars | `^[a-zA-Z0-9_-]+$` |
| Google Gemini | URL-safe chars | 64 chars | URL encoding safe |
| Mistral | Exactly 9 alphanumeric | 9 chars | Deterministic padding |
| Bedrock | UUIDs | None | No restrictions |

The `transformMessages()` function normalizes IDs when replaying conversations
across different providers:

```typescript
function transformMessages<TApi>(
  messages: Message[],
  model: Model<TApi>,
  normalizeToolCallId?: (id: string, model: Model<TApi>, source: AssistantMessage) => string,
): Message[] {
  // Build map: original ID → normalized ID
  const toolCallIdMap = new Map<string, string>();

  // First pass: normalize assistant message tool call IDs
  // Second pass: update matching toolResult.toolCallId references
  // Third pass: insert synthetic empty results for orphaned tool calls
}
```

The "orphaned tool call" handling is critical: if an assistant message contains
a tool call but no corresponding tool result exists (e.g., due to an error or
abort), the transform inserts a synthetic error result. This prevents API
errors when providers require every tool call to have a matching result.

### 10.11 Built-in Tool Details

Pi-mono ships 7 built-in tools, with 4 active by default:

| Tool | Default | Description | Key Behavior |
|------|---------|-------------|--------------|
| `read` | Yes | Read file contents | Head truncation, image support, offset/limit pagination |
| `bash` | Yes | Execute shell commands | Tail truncation, temp file for overflow, timeout support |
| `edit` | Yes | Find-and-replace in files | Fuzzy matching, BOM handling, line ending normalization, uniqueness check |
| `write` | Yes | Create/overwrite files | Full file writes only |
| `grep` | No | Search file contents | Respects .gitignore |
| `find` | No | Find files by glob | Respects .gitignore |
| `ls` | No | List directory | Respects .gitignore |

Notable implementation details:

**Edit tool fuzzy matching**: The edit tool doesn't require exact whitespace
matches. It normalizes line endings and applies fuzzy matching before falling
back to exact match. This compensates for LLMs that subtly alter whitespace.

**Bash tool process management**: Uses `killProcessTree()` to kill entire
process trees (not just the spawned process) on abort/timeout. Output streams
to a temp file when it exceeds 30KB, and the truncation message includes the
temp file path so the LLM can read it if needed.

**Read tool image support**: When the file is a supported image type (jpg, png,
gif, webp), it auto-resizes to 2000x2000 max and returns it as an
`ImageContent` block rather than text.

**Factory pattern**: All tools use `createXTool(cwd, options?)` factories:
```typescript
const tools = createCodingTools("/path/to/project", {
  read: { autoResizeImages: false },
  bash: { commandPrefix: "source ~/.bashrc" },
});
```

### 10.12 Tool Prompting Patterns — Summary

Pi-mono uses a **layered prompting strategy** for tool-use:

```
Layer 1: Tool JSON Schema
  ├── tool.name, tool.description
  └── Per-parameter descriptions in TypeBox schema
      → Sent directly to the LLM via the provider's tool API

Layer 2: System Prompt — Available Tools Section
  └── tool.promptSnippet (or description fallback)
      → One-liner in "Available tools:" list

Layer 3: System Prompt — Guidelines Section
  ├── Conditional rules based on tool set composition
  │   (e.g., "prefer grep over bash" only when both exist)
  └── tool.promptGuidelines
      → Per-tool usage instructions

Layer 4: System Prompt — Skills Section
  └── XML-formatted skill descriptions with file paths
      → "Use read tool to load a skill when task matches"
      → Lazy-loading pattern: full instructions loaded on demand

Layer 5: Tool Result Messages
  └── Actionable error messages and truncation notices
      → "Use offset=1001 to continue"
      → "Found 3 occurrences, provide more context"
      → Guides the LLM's next action
```

**Key insight**: The system prompt never says "you have these tools, use them."
Instead, it says "here are the tools, and here are the rules for WHEN and HOW
to use each one." The conditional guideline generation ensures the rules are
always consistent with the actual tool set — no dead instructions for unavailable
tools, and composition-aware rules when tools interact (e.g., read-before-edit).

**Comparison with cinch-rs**: cinch-rs currently embeds tool descriptions in the
system prompt statically. Adopting pi-mono's approach of conditional,
composition-aware guidelines would improve tool selection accuracy, especially
as the tool set grows. The `promptSnippet`/`promptGuidelines` fields on tool
definitions are a clean way to let tools declare their own usage instructions
without coupling the system prompt builder to individual tool implementations.

---

## 11. Deep Comparison: Tool-Use in cinch-rs vs pi-mono

This section provides a side-by-side analysis of every aspect of tool-use in
both systems, identifies where cinch-rs is already ahead, where pi-mono is
ahead, and proposes concrete improvements to make cinch-rs's tool-use best in
class.

### 11.1 Tool Definition — cinch-rs Wins

**cinch-rs** has a significantly richer tool description system via `ToolSpec`:

```rust
ToolSpec::builder("grep")
    .purpose("Search for a regex pattern in file contents")
    .when_to_use("When you need to find text matching a pattern across multiple files")
    .when_not_to_use("When you already know the file path — use read_file instead")
    .parameters_for::<GrepArgs>()
    .example("grep(pattern='TODO', glob='*.rs')", "src/main.rs\nsrc/tools/common.rs")
    .output_format("Depends on mode: 'files' returns paths only (default)...")
    .disambiguate("Need to read a file", "read_file", "read_file returns full content")
    .build()
    .to_tool_def()
```

vs pi-mono's flat `description` string:

```typescript
description: "Execute a bash command in the current working directory. Returns stdout
and stderr. Output is truncated to last 1000 lines or 30KB."
```

**cinch-rs advantages:**
- **Structured fields**: `purpose`, `when_to_use`, `when_not_to_use`, `examples`,
  `output_format`, `disambiguation` — each serving a distinct role
- **`when_not_to_use`**: The single highest-value field for preventing tool
  confusion (EASYTOOL paper: 70% token cost reduction with structured descriptions)
- **Disambiguation examples**: Explicitly clarify "use X not Y when Z"
- **Progressive loading**: `to_compact_description()` sends purpose-only in the
  tool schema, with `extended_description()` injected as a system reminder on
  first use — saves tokens on tools the LLM never invokes
- **Type-safe parameters**: `parameters_for::<GrepArgs>()` derives JSON Schema
  from the Rust type, making schema and deserialization impossible to diverge

**pi-mono's only advantage here**: The `promptSnippet`/`promptGuidelines` fields
inject tool-specific instructions into the system prompt, providing a second
channel for guidance beyond the tool schema. See 11.4 for how cinch-rs should
adopt this.

### 11.2 System Prompt — pi-mono Wins

**pi-mono** dynamically assembles tool-use guidelines in the system prompt based
on which tools are active:

```
Available tools:
- read: Read file contents
- bash: Execute bash commands
- edit: Make surgical edits to files

Guidelines:
- Use read to examine files before editing (when both read + edit active)
- Prefer grep/find/ls over bash for file exploration (when specialized tools active)
- Use edit for precise changes (old text must match exactly)
- Use write only for new files or complete rewrites
```

**cinch-rs** has a minimal system prompt with no tool-use instructions:

```rust
"You are a coding assistant. You have access to tools for reading, editing, \
and searching files, running shell commands, and performing git operations.

Guidelines:
- Read files before editing them.
- Make minimal, focused changes.
- Use git tools to understand the repository state.
- Explain what you're doing before making changes."
```

**What's missing in cinch-rs:**

1. **No tool listing in system prompt**: The LLM only sees tools via the JSON
   schema in the API call. Pi-mono duplicates a summary in the system prompt,
   giving the model two representations (summary for selection, full schema for
   invocation).

2. **No conditional guidelines**: The system prompt doesn't adapt to the tool
   set. If `grep` and `find_files` are both active, there's no guidance about
   when to use which. If `web_search` is disabled (no API key), the prompt
   doesn't change.

3. **No tool composition rules**: No "prefer X over Y when Z" instructions.
   The `ToolSpec::disambiguation` field partially addresses this but only in
   the tool schema, not the system prompt.

4. **No output format guidance**: The prompt doesn't tell the LLM how to
   interpret tool output formats (e.g., "L{n}: prefix means line numbers").

**cinch-rs infrastructure advantage**: The `PromptRegistry` and
`SystemPromptBuilder` are more sophisticated than pi-mono's single
`buildSystemPrompt()` function. cinch-rs has stable/dynamic section ordering
for cache optimization, conditional sections with `TurnContext`, and
`ReminderRegistry` for mid-conversation injections. The infrastructure is
excellent — it's just not being used for tool guidance yet.

### 11.3 Tool Execution — cinch-rs Wins

**cinch-rs** has significantly more advanced execution mechanics:

| Feature | cinch-rs | pi-mono |
|---------|----------|---------|
| Execution model | **Parallel (DAG-aware)** | Sequential |
| Result caching | **FNV-1a hash cache** | None |
| Read-before-write | **Shared ReadTracker** | Not enforced |
| Mutation tracking | **`is_mutation()` trait method** | Not tracked |
| Configurable timeout | **Per-ToolSet + per-Shell** | Per-tool only |
| Argument validation | **Optional JSON Schema (schemars)** | AJV with coercion |
| Conditional registration | **`with_if(condition, tool)`** | Dynamic via extension |
| Disabled tools | **`DisabledTool` wrapper** | Not built-in |

cinch-rs's parallel execution via DAG scheduling is a major performance
advantage. The `cacheable()` / `is_mutation()` trait methods enable intelligent
caching that avoids redundant tool calls.

**pi-mono's advantage**: `onUpdate` callback for streaming partial results
during long tool execution. cinch-rs returns the full result only after
completion. This matters for `shell` tool UX — users want to see output as
it streams.

### 11.4 Error Messages — Both Strong, Different Strengths

Both systems produce actionable error messages, but with different approaches.

**cinch-rs** has a **two-layer error system**:

1. **Tool-level errors**: Each tool returns specific messages:
   - `edit_file`: "old_string found 3 times in src/main.rs (lines: 12, 45, 78).
     Provide more surrounding context to make it unique, or set replace_all=true."
   - `read_file`: "'{path}' is a directory, not a file. Use list_dir to browse
     directories."
   - `shell`: "potentially destructive command blocked"

2. **`format_tool_failure` wrapper** (reflection.rs): Automatically wraps any
   `Error:` result with contextual suggestions:
   - File not found → "Use list_dir or find_files to discover the right path"
   - Permission denied → "Try a different approach"
   - Timeout → "Try with smaller input or different arguments"
   - Generic → "Consider using the 'think' tool to reason about what went wrong"

**pi-mono** has **LLM-actionable truncation notices**:
- `[Showing lines 1-1000 of 5432. Use offset=1001 to continue.]`
- `[Full output: /tmp/pi-bash-abc123.log]`

cinch-rs's truncation notices are less actionable:
- `[truncated: 5432 total lines. Use offset/limit for more.]`

**Improvement opportunity**: cinch-rs truncation messages should include the
exact offset value, not just suggest using offset/limit.

### 11.5 Tool Output Truncation — Pi-mono's Bash is Better

| Aspect | cinch-rs | pi-mono |
|--------|----------|---------|
| Read tool | Head truncation (first N lines) | Head truncation (first N lines) |
| Shell tool | **Head+tail** (40% tail ratio) | Tail only (last N lines) |
| Truncation notice | Generic "use offset/limit" | **Exact offset value** |
| Overflow handling | Truncated in memory | **Temp file + path in message** |
| Max bytes | 30KB (configurable) | 30KB |
| Max lines | 2000 (read), no line limit (shell) | 1000 |

cinch-rs's `HeadAndTail` strategy for shell output is smarter than pi-mono's
tail-only approach — it preserves both the initial context and the final
output. But pi-mono's temp file overflow is valuable: when shell output
exceeds 30KB, the full output is saved to a temp file and the path is included
in the truncation message, letting the LLM read the full output if needed.

### 11.6 Concrete Improvements for cinch-rs

Based on the comparison, here are the specific improvements ranked by impact:

#### Priority 1: Add Tool-Use Guidelines to System Prompt

**Problem**: The system prompt says nothing about how to use tools. The LLM
relies entirely on tool schema descriptions for guidance.

**Solution**: Register a "Tool Guidance" section in the `PromptRegistry` that
generates conditional guidelines based on active tools, similar to pi-mono's
`buildSystemPrompt()`.

```rust
// In the harness or coding agent setup:
registry.register_stable("Tool Guidance", 15, |_| true, |_ctx| {
    let mut guidelines = Vec::new();

    // These would check which tools are active (via metadata or tool list)
    guidelines.push(
        "- Always read a file with read_file before editing it with edit_file."
    );
    guidelines.push(
        "- Prefer grep over shell('grep ...') for file content search \
         (respects limits, structured output)."
    );
    guidelines.push(
        "- Prefer find_files over shell('find ...') for file discovery \
         (respects limits, mtime-sorted)."
    );
    guidelines.push(
        "- Use shell only when no dedicated tool covers the operation \
         (git, build commands, data processing)."
    );
    guidelines.push(
        "- When edit_file says old_string was found multiple times, \
         include more surrounding context to disambiguate."
    );

    format!("Tool usage rules:\n{}", guidelines.join("\n"))
});
```

**Impact**: High. Explicit "prefer X over Y" rules dramatically reduce tool
confusion. The LLM currently has to infer these rules from the `when_not_to_use`
field in each tool's schema, which is less effective than a consolidated
guidelines section.

#### Priority 2: Make Truncation Notices Actionable

**Problem**: Current truncation messages say "use offset/limit for more" but
don't provide the exact values.

**Solution**: Include the next offset value in truncation messages.

**Current** (read_file):
```
[truncated: 5432 total lines. Use offset/limit for more.]
```

**Proposed**:
```
[Showing lines 1-2000 of 5432. Use read_file(path='...', offset=2001) to continue.]
```

**Current** (shell, after head+tail truncation):
```
[Output truncated to 30000 bytes. Head and tail preserved.]
```

**Proposed**:
```
[Output truncated: showing first 60% and last 40% of 85432 bytes. \
Full output saved to /tmp/cinch-shell-{hash}.log — use read_file to access.]
```

**Impact**: Medium-high. LLMs follow exact instructions much more reliably than
vague suggestions. Pi-mono's `Use offset=1001 to continue` pattern is
demonstrably effective.

#### Priority 3: Add `prompt_guidelines` to the Tool Trait

**Problem**: Tools can only influence the LLM through the tool schema
description. They can't inject rules into the system prompt.

**Solution**: Add an optional `prompt_guidelines()` method to the `Tool` trait
that returns system-prompt-level usage instructions.

```rust
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDef;
    fn execute(&self, arguments: &str) -> ToolFuture<'_>;

    // New: system prompt guidelines contributed by this tool
    fn prompt_guidelines(&self) -> Vec<String> { vec![] }

    // Existing methods...
    fn cacheable(&self) -> bool { false }
    fn is_mutation(&self) -> bool { false }
    fn extended_description(&self) -> Option<String> { None }
}
```

Then the harness/ToolSet collects all active tools' guidelines and injects them
into the system prompt as a consolidated "Tool Guidance" section. Each tool
owns its own guidelines, and the system prompt adapts automatically as tools
are added or removed.

Example implementation:
```rust
impl Tool for EditFile {
    fn prompt_guidelines(&self) -> Vec<String> {
        vec![
            "Always read a file with read_file before editing with edit_file.".into(),
            "When edit_file reports multiple matches, include more surrounding \
             lines in old_string to disambiguate.".into(),
        ]
    }
}

impl Tool for Shell {
    fn prompt_guidelines(&self) -> Vec<String> {
        vec![
            "Use shell only for operations not covered by dedicated tools \
             (git commands, build scripts, data processing).".into(),
            "Prefer read_file over shell('cat ...'), grep over shell('grep ...'), \
             find_files over shell('find ...').".into(),
        ]
    }
}
```

**Impact**: Medium. This is the architectural foundation for Priority 1. It
decouples tool guidance from the prompt builder, matching pi-mono's
`promptGuidelines` field.

#### Priority 4: Shell Output Temp File Overflow

**Problem**: When shell output exceeds 30KB, cinch-rs truncates with
head+tail but the full output is lost. The LLM cannot access the truncated
middle section.

**Solution**: When shell output exceeds the byte limit, write the full output
to a temp file and include the path in the truncation notice.

```rust
if result.len() > max_result_bytes {
    let temp_path = format!("/tmp/cinch-shell-{}.log", hash_command(&args.command));
    tokio::fs::write(&temp_path, &result).await.ok();

    let truncated = truncate_with_strategy(
        result, max, &TruncationStrategy::HeadAndTail { tail_ratio: 0.4 }
    );
    format!(
        "{truncated}\n\n[Full output ({} bytes) saved to {temp_path}. \
         Use read_file(path='{temp_path}') to view specific sections.]",
        result.len()
    )
}
```

**Impact**: Medium. Prevents information loss from long-running commands.
Pi-mono's temp file pattern is proven effective.

#### Priority 5: Composition-Aware Conditional Guidelines

**Problem**: The tool guidelines in Priority 1 are static. They should adapt
to which tools are actually registered.

**Solution**: The `ToolSet` should expose a method that generates guidelines
based on registered tools and their relationships.

```rust
impl ToolSet {
    /// Generate composition-aware tool usage guidelines.
    pub fn generate_guidelines(&self) -> String {
        let mut guidelines = Vec::new();
        let has = |name: &str| self.tools.contains_key(name);

        // Read-before-edit (only when both exist)
        if has("read_file") && has("edit_file") {
            guidelines.push(
                "Always read a file with read_file before editing with edit_file."
            );
        }

        // Prefer dedicated tools over shell (when dedicated tools exist)
        if has("shell") && has("grep") {
            guidelines.push(
                "Prefer grep over shell('grep ...') for searching file content."
            );
        }
        if has("shell") && has("find_files") {
            guidelines.push(
                "Prefer find_files over shell('find ...') for finding files by name."
            );
        }
        if has("shell") && has("read_file") {
            guidelines.push(
                "Prefer read_file over shell('cat ...') for reading file content."
            );
        }

        // Edit vs write guidance (when both exist)
        if has("edit_file") && has("write_file") {
            guidelines.push(
                "Use edit_file for modifying existing files; use write_file only \
                 for creating new files or complete rewrites."
            );
        }

        // Collect per-tool guidelines
        for tool in self.tools.values() {
            for guideline in tool.prompt_guidelines() {
                if !guidelines.contains(&&*guideline) {
                    guidelines.push(&guideline);
                }
            }
        }

        if guidelines.is_empty() {
            return String::new();
        }

        let bullets: Vec<String> = guidelines.iter()
            .map(|g| format!("- {g}"))
            .collect();
        format!("Tool usage rules:\n{}", bullets.join("\n"))
    }
}
```

**Impact**: Medium. Prevents dead instructions for missing tools and
automatically adapts as the tool set changes. Pi-mono's conditional guideline
generation is one of its best architectural choices.

#### Priority 6: Fuzzy Edit Matching

**Problem**: cinch-rs's `edit_file` requires exact string matching. LLMs
frequently introduce subtle whitespace differences (trailing spaces, tab/space
mixing, line ending variations).

**Solution**: Pi-mono's edit tool uses fuzzy matching — it normalizes line
endings and applies fuzzy whitespace matching before falling back to exact
match. cinch-rs should add similar normalization:

```rust
// Before matching old_string in content:
let normalized_content = normalize_whitespace(&content);
let normalized_old = normalize_whitespace(&args.old_string);

fn normalize_whitespace(s: &str) -> String {
    s.replace("\r\n", "\n")
     .lines()
     .map(|line| line.trim_end())
     .collect::<Vec<_>>()
     .join("\n")
}
```

Try normalized match first, fall back to exact match. Only apply the normalized
match when there's exactly one occurrence (to preserve the uniqueness guarantee).

**Impact**: Medium. Reduces edit failures from whitespace mismatches, a common
LLM error mode. Pi-mono's fuzzy matching compensates for this gracefully.

### 11.7 Summary: Improvement Priority Matrix

| # | Improvement | Effort | Impact | Pi-mono Inspired |
|---|-------------|--------|--------|-----------------|
| 1 | Tool-use guidelines in system prompt | Low | **High** | Yes |
| 2 | Actionable truncation notices | Low | **Med-High** | Yes |
| 3 | `prompt_guidelines()` on Tool trait | Low | **Med** | Yes |
| 4 | Shell temp file overflow | Med | **Med** | Yes |
| 5 | Composition-aware conditional guidelines | Med | **Med** | Yes |
| 6 | Fuzzy edit matching | Med | **Med** | Yes |

### 11.8 What cinch-rs Should NOT Copy from pi-mono

1. **Sequential tool execution**: cinch-rs's parallel DAG scheduler is strictly
   superior. Pi-mono executes tools sequentially to support steering message
   injection between tools, but cinch-rs can achieve the same interrupt
   capability with its event handler system.

2. **Minimal system prompt**: Pi-mono's system prompt is deliberately sparse
   ("You are an expert coding assistant operating inside pi"). cinch-rs should
   keep its richer, more structured prompt approach.

3. **Extension-based tool registration**: Pi-mono's dynamic tool registration
   via extensions adds runtime complexity. cinch-rs's compile-time `ToolSet`
   composition via `.with()` is more Rust-idiomatic and catches errors earlier.

4. **Skills lazy-loading**: Pi-mono loads skill instructions on-demand via the
   `read` tool. This saves tokens but adds latency. cinch-rs's progressive
   loading via `compact_definitions()` + `extended_description()` achieves the
   same token savings with simpler architecture.

### 11.9 What cinch-rs Already Does Better Than Pi-mono

| Feature | How cinch-rs does it better |
|---------|---------------------------|
| **Tool descriptions** | `ToolSpec` with structured `when_to_use`, `when_not_to_use`, disambiguation |
| **Tool execution** | Parallel DAG-aware scheduling |
| **Result caching** | FNV-1a hash with mutation-aware invalidation |
| **Read-before-write** | Shared `ReadTracker` across ReadFile/EditFile/WriteFile |
| **Progressive loading** | `compact_definitions()` + `extended_description()` on first use |
| **Error reflection** | `format_tool_failure()` wraps errors with contextual recovery suggestions |
| **Conditional registration** | `with_if(condition, tool)` and `DisabledTool` wrapper |
| **Shell truncation** | Head+tail strategy preserves both context and final output |
| **Prompt caching** | `PromptRegistry` with stable/dynamic section ordering |
| **Mid-conversation nudges** | `ReminderRegistry` with frequency control and round context |
| **Type safety** | Rust's type system prevents entire categories of tool definition bugs |
