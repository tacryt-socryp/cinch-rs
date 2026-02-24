# cinch-rs Tool Implementation Plan

> A model-agnostic tool system designed for maximum token efficiency.
>
> Synthesized from ground-truth analysis of OpenAI Codex and Anthropic Claude Code,
> adapted for cinch-rs's role as a general-purpose harness that works with any model.

---

## Design Principles

### Why Model-Agnostic Matters

Codex can train models on `apply_patch` grammar. Claude Code can rely on Claude's native XML parsing. cinch-rs can't assume any model-specific behavior. Every efficiency gain must come from:

1. **Tool interface design** — schemas that produce compact, predictable output regardless of which model calls them
2. **System prompt guidance** — explicit routing rules that any instruction-following model can learn
3. **Structural enforcement** — tool-level constraints (read-before-write, output caps, default modes) that prevent waste even when the model makes suboptimal choices

### Core Token Efficiency Strategy

Both Codex and Claude Code converge on the same fundamental insight: **control the output, not just the input**. The biggest context waste comes from unstructured tool results flooding the conversation. The ideal tool set:

- Defaults to the **smallest useful output** (file paths, not file contents; confirmations, not echoes)
- **Separates discovery from retrieval** (search returns pointers; read returns content)
- **Enforces safety at the tool level** (read-before-write, path validation) so prompt guidance is defense-in-depth, not the only guardrail
- **Caps output structurally** (truncation with preserved boundaries, not arbitrary byte cuts)

---

## Tool Inventory

### Tier 1: Core Tools (Always Loaded)

These tools are present in every cinch-rs session. Their definitions are compact and stable, maximizing prompt cache hits.

#### `read_file`

Read a file with line numbers.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `path` | string (required) | — | Absolute or workspace-relative path |
| `offset` | integer | 1 | Starting line number (1-indexed) |
| `limit` | integer | 2000 | Max lines to return |

**Output format**: Numbered lines, one per line.
```
L1: use std::fs;
L2: use std::path::Path;
L3:
L4: fn main() {
```

**Efficiency design**:
- **Line numbers in output** (like both Codex `L{n}:` and Claude Code `cat -n`). This gives the model anchors for `edit_file` operations without re-reading.
- **2000-line default cap**. Large files require explicit offset/limit, forcing the model to be surgical.
- **Truncation message**: If the file exceeds the limit, append `[truncated: {total_lines} total lines in file]` so the model knows there's more.
- **Long line truncation**: Lines exceeding 500 characters are truncated with `... [line truncated at 500 chars]`.
- **Read tracking**: Every successful read registers the file path + content hash in a session-scoped `ReadTracker`. This is consumed by `edit_file` and `write_file` for read-before-write enforcement.

**System prompt guidance**:
```
Use `read_file` to read files. Do NOT use shell commands like `cat`, `head`, or `tail` for file reading —
they produce unstructured output that wastes context. The `read_file` tool returns numbered lines that
you can reference when editing. For large files, use `offset` and `limit` to read specific sections
rather than reading the entire file.
```

#### `edit_file`

Replace an exact string in a file.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `path` | string (required) | — | File to edit |
| `old_string` | string (required) | — | Exact text to find and replace |
| `new_string` | string (required) | — | Replacement text |
| `replace_all` | boolean | false | Replace all occurrences (not just first) |

**Output format**: Confirmation with diff summary.
```
Edited path/to/file.rs: replaced 1 occurrence (lines 42-45)
```

**Efficiency design**:
- **Read-before-write enforcement**: If the file hasn't been read via `read_file` in this session, the tool returns an error: `"Error: You must read this file before editing it. Use read_file first."` This is enforced at the tool level (like Claude Code), not just the prompt level.
- **Uniqueness check**: If `old_string` matches multiple locations and `replace_all` is false, return an error with match count and line numbers: `"Error: old_string matches 3 locations (lines 12, 45, 89). Provide more context to make it unique, or set replace_all=true."` This prevents silent wrong-location edits.
- **Compact output**: Returns only a confirmation line, not the modified file contents. The model already knows what it wrote. (Codex's prompt explicitly says "Do not waste tokens by re-reading files after patching.")
- **Whitespace-aware matching**: Normalize leading whitespace (tabs→spaces) before matching to handle indentation variants.

**System prompt guidance**:
```
Use `edit_file` for precise file modifications. You MUST read a file with `read_file` before editing it.
The `old_string` must exactly match text in the file. Include enough surrounding context to make the
match unique. Do NOT use shell commands like `sed` or `awk` for file editing.

After a successful edit, do NOT re-read the file to verify — the tool confirms the edit or returns
an error. Trust the confirmation and move on.
```

#### `write_file`

Create a new file or overwrite an existing file.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `path` | string (required) | — | File to write |
| `content` | string (required) | — | Full file contents |

**Output format**: Confirmation.
```
Wrote 142 lines to path/to/new_file.rs
```

**Efficiency design**:
- **Read-before-overwrite enforcement**: If the file already exists and hasn't been read in this session, return an error. New files (that don't exist yet) can be written without a prior read.
- **Prefer `edit_file`**: The system prompt directs the model to prefer `edit_file` for existing files and reserve `write_file` for new file creation.

**System prompt guidance**:
```
Use `write_file` to create new files. For modifying existing files, ALWAYS prefer `edit_file` —
it's more precise and avoids accidentally losing content. If you must overwrite an existing file,
read it first with `read_file`.
```

#### `grep`

Search file contents using regex, powered by ripgrep.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `pattern` | string (required) | — | Regex pattern |
| `path` | string | workspace root | Directory or file to search |
| `include` | string | — | Glob filter (e.g., `*.rs`, `*.{ts,tsx}`) |
| `mode` | enum | `"files"` | `"files"` (paths only), `"content"` (matching lines), `"count"` (match counts) |
| `limit` | integer | 100 | Max results |
| `context_lines` | integer | 0 | Lines of context around matches (only in `content` mode) |

**Output format** (by mode):

`files` mode (default):
```
src/tools/common.rs
src/agent/harness.rs
```

`content` mode:
```
src/tools/common.rs:42: fn execute(&self, arguments: &str) -> ToolFuture {
src/tools/common.rs:58:     fn execute(&self, args: &str) -> ToolFuture {
```

`count` mode:
```
src/tools/common.rs: 3
src/agent/harness.rs: 1
```

**Efficiency design**:
- **Default to `files` mode** (paths only). This is the single most impactful token-saving decision, validated by both Codex (`--files-with-matches`) and Claude Code (`files_with_matches` default). The model gets a compact list of relevant files, then uses `read_file` on the ones it actually needs.
- **Sort by modification time** (`--sortr=modified`), like Codex. Recently-changed files are more likely to be relevant.
- **`content` mode is opt-in**. When the model needs to see actual matches (e.g., finding a specific function signature), it explicitly requests content mode with a tight `limit`.
- **`context_lines` only works in content mode**. Prevents accidental context explosion.
- **Hard limit cap**: `limit` is silently capped at 2000 to prevent unbounded output.

**System prompt guidance**:
```
Use `grep` for searching file contents. It defaults to returning file paths only (not content) —
this keeps results compact. Use `read_file` to examine specific matches.

For targeted searches where you need to see the matching lines, use mode="content" with a low limit.

Do NOT use shell commands like `grep` or `rg` directly — the `grep` tool has controlled output
that prevents context waste.
```

#### `find_files`

Find files by glob pattern.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `pattern` | string (required) | — | Glob pattern (e.g., `**/*.rs`, `src/**/test_*.py`) |
| `path` | string | workspace root | Directory to search from |
| `limit` | integer | 100 | Max results |

**Output format**: File paths sorted by modification time (most recent first).
```
src/tools/common.rs
src/tools/core.rs
src/tools/cache.rs
```

**Efficiency design**:
- **Modification-time sorting**: Like `grep`, surfaces recently-changed files first.
- **Hard limit cap at 1000**: Prevents unbounded output from broad patterns like `**/*`.
- **No metadata**: Returns paths only, not sizes/dates/permissions. The model rarely needs this, and it doubles the output size.

**System prompt guidance**:
```
Use `find_files` to locate files by name pattern. Returns paths sorted by most recently modified.
Do NOT use shell commands like `find` or `ls` for file discovery.
```

#### `list_dir`

List directory contents with depth control.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `path` | string (required) | — | Directory to list |
| `depth` | integer | 2 | How many levels deep to recurse |
| `limit` | integer | 50 | Max entries to return |
| `offset` | integer | 1 | 1-indexed pagination offset |

**Output format**: Indented tree with type indicators.
```
Absolute path: /Users/dev/project/src
tools/
  common.rs
  core.rs
  cache.rs
  filter.rs
agent/
  harness.rs
  config.rs
  sub_agent.rs
README.md
```

**Efficiency design**:
- **Depth 2 default** (like Codex). Shows immediate children and one level of nesting — enough for orientation without dumping deep trees.
- **Type indicators**: `/` for directories, `@` for symlinks. No clutter from file sizes or dates.
- **Alphabetical sorting**: Predictable ordering for pagination.
- **Pagination**: For large directories, the model can page through with `offset`. Trailing `"More than {limit} entries found"` message signals when there's more.
- **Entry name truncation**: Names longer than 500 chars are truncated.

**System prompt guidance**:
```
Use `list_dir` to explore directory structure. Defaults to 2 levels deep. Use `depth=1` for a
quick overview of a directory, or `depth=3` for deeper exploration.
```

#### `shell`

Execute a shell command and return output.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `command` | string (required) | — | Shell command to execute |
| `timeout` | integer | 120 | Timeout in seconds (max 600) |
| `working_dir` | string | workspace root | Working directory |

**Output format**: Combined stdout/stderr with exit code.
```
[exit: 0]
running 12 tests
test tools::tests::grep_basic ... ok
test tools::tests::grep_limit ... ok
...
```

**Efficiency design**:
- **Output truncation with begin+end preservation** (inspired by Codex). When output exceeds 30KB:
  ```
  [first 12KB of output]
  ... [truncated: 85KB total, showing first and last 12KB] ...
  [last 12KB of output]
  ```
  This preserves the command header (what ran) and the tail (final results/errors), which are almost always the most useful parts. Pure head-truncation loses error messages at the end; pure tail-truncation loses context about what was executed.
- **Exit code prefix**: Always show the exit code first so the model can quickly branch on success/failure.
- **Blocked commands**: Configurable blocklist (default: `rm -rf /`, `mkfs`, `> /dev/`) that returns an error rather than executing.
- **No shell for file ops**: The system prompt aggressively routes the model away from using `shell` for anything that has a dedicated tool.

**System prompt guidance**:
```
Use `shell` for commands that don't have a dedicated tool: running tests, building, git operations,
installing packages, running scripts. The working directory persists between calls.

IMPORTANT: Do NOT use `shell` for these operations — use the dedicated tool instead:
- Reading files → use `read_file` (not `cat`, `head`, `tail`)
- Searching file contents → use `grep` (not `grep`, `rg`, `ag`)
- Finding files → use `find_files` (not `find`, `fd`, `ls -R`)
- Editing files → use `edit_file` (not `sed`, `awk`, `perl -i`)
- Writing files → use `write_file` (not `echo >`, `cat <<EOF`)
- Listing directories → use `list_dir` (not `ls`, `tree`)

Dedicated tools have controlled output formats that prevent context waste. Shell commands produce
unstructured output that can flood your context window.
```

#### `think`

Scratchpad for reasoning without executing anything.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `thought` | string (required) | — | Your reasoning |

**Output format**: Empty string (the thought itself is the value — it appears in the conversation as the model's reasoning).

**Efficiency design**:
- **Zero-output tool**: Returns empty string. The value is in the model's reasoning trace, not the result.
- **Pseudo-tool**: Doesn't actually execute anything. Useful for models that benefit from explicit reasoning steps.

**System prompt guidance**:
```
Use `think` when you need to reason through a complex problem before acting. This is a scratchpad —
it doesn't execute anything. Use it to plan multi-step operations, analyze code, or work through
tricky logic before committing to tool calls.
```

### Tier 2: Standard Extensions (Loaded by Default, Configurable)

These tools are included in the standard `ToolSet::with_common_tools()` but can be individually disabled via configuration.

#### `web_search`

Search the web and return results.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `query` | string (required) | — | Search query |
| `limit` | integer | 5 | Max results |

**Output format**: Structured results with titles, URLs, and snippets.
```
[1] "Rust async trait guide" — https://example.com/async-traits
    How to use async traits in Rust with the async-trait crate...

[2] "tokio::select! macro" — https://docs.rs/tokio/...
    The select! macro waits on multiple async operations...
```

**Efficiency design**:
- **Structured output with numbering**: The model can reference results by number.
- **Snippet, not full page**: Returns search result snippets, not full page content. For full content, the model should use `web_fetch`.
- **Low default limit**: 5 results is usually enough. More costs tokens.
- **Feature-gated**: Only loaded if a search API key is configured (e.g., `BRAVE_SEARCH_KEY`, `SERP_API_KEY`).

#### `web_fetch`

Fetch a URL and extract information.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `url` | string (required) | — | URL to fetch |
| `prompt` | string (required) | — | What to extract from the page |
| `max_tokens` | integer | 2000 | Max tokens for the extracted response |

**Output format**: Extracted information based on the prompt.

**Efficiency design**:
- **Secondary model extraction** (like Claude Code). The raw page content is never injected into the main context. Instead:
  1. Fetch URL → convert HTML to markdown
  2. Send markdown + extraction prompt to a small/cheap model
  3. Return the small model's response (capped at `max_tokens`)
- **Why this matters**: A typical web page is 10-50K tokens of raw content. The extraction model distills this to 500-2000 tokens of relevant information. This is a 10-25x context savings.
- **Configurable extraction model**: `WebFetchConfig { extraction_model: String }` — defaults to the cheapest available model.
- **Cache**: 15-minute TTL cache keyed on URL to avoid re-fetching.

**System prompt guidance**:
```
Use `web_fetch` to read web pages. Provide a specific `prompt` describing what information you
need — the page content is processed by a separate model and only the relevant information is
returned. This keeps your context clean.

For API documentation, be specific: "Extract the function signature and parameter descriptions for
the `connect` method" is better than "Summarize this page".
```

#### `todo`

Persistent task checklist for tracking multi-step work.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `action` | enum (required) | — | `"add"`, `"complete"`, `"remove"`, `"list"` |
| `task` | string | — | Task description (for `add`) |
| `index` | integer | — | Task index (for `complete`, `remove`) |

**Output format**: Current task list after modification.
```
Tasks:
  [x] 1. Read the authentication module
  [ ] 2. Fix the token refresh bug  ← in progress
  [ ] 3. Add unit tests for refresh
  [ ] 4. Run existing test suite
```

**Efficiency design**:
- **Pseudo-tool**: Maintains state in memory, not on disk. The task list is compact and helps the model maintain focus across long sessions.
- **Displayed after every modification**: The model always sees the current state, preventing drift.

### Tier 3: Delegation Tools (Loaded When Sub-Agents Enabled)

#### `delegate`

Spawn a sub-agent to handle a task autonomously.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `task` | string (required) | — | What the sub-agent should do |
| `type` | enum | `"worker"` | `"explore"`, `"worker"`, `"planner"` |
| `context` | string | — | Relevant context from the parent conversation |
| `model` | string | — | Override model for the sub-agent |
| `background` | boolean | false | Run without blocking the parent |

**Sub-agent types and their tool access**:

| Type | Tools Available | Model | Use Case |
|------|----------------|-------|----------|
| `explore` | read_file, grep, find_files, list_dir, think | Cheapest available | Read-only codebase exploration |
| `worker` | All Tier 1 + Tier 2 | Inherited from parent | Full-capability delegated task |
| `planner` | read_file, grep, find_files, list_dir, think | Inherited from parent | Produce a plan without executing |

**Efficiency design**:
- **Typed specialization** (like Claude Code's sub-agent types). An `explore` agent can't accidentally modify files and uses a cheaper model, saving both tokens and cost.
- **Context injection**: The `context` parameter lets the parent pass relevant findings to the child, avoiding expensive re-discovery. This is injected as a system message: `"Context from parent task:\n{context}"`.
- **Result truncation**: Sub-agent results are truncated to `max_result_chars` (default 4000) before returning to the parent.
- **Background execution**: When `background=true`, the tool returns immediately with an agent ID. The parent can check results later with `check_agent`.

**System prompt guidance**:
```
Use `delegate` to spawn a sub-agent for tasks that would benefit from focused, autonomous work:
- Exploring a large codebase section → type="explore" (fast, cheap, read-only)
- Implementing a self-contained change → type="worker" (full tool access)
- Planning an approach before committing → type="planner" (read-only, returns a plan)

Pass relevant `context` to avoid the sub-agent re-discovering what you already know.
Launch multiple agents in parallel when their tasks are independent.
```

#### `check_agent`

Check on a background sub-agent.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `agent_id` | string (required) | — | ID from a background `delegate` call |
| `block` | boolean | false | Wait for completion |

**Output format**: Agent status and result (if complete).
```
Status: completed
Result: Found 3 authentication-related modules...
```

### Tier 4: MCP Tools (Dynamic, Feature-Gated)

MCP tools are loaded dynamically when MCP servers are configured. Their management follows these rules:

**Tool definition budget**: When total MCP tool definitions exceed 10% of the effective context window, switch to deferred loading:
- Remove MCP tool definitions from the tool list
- Add a `search_tools` meta-tool that searches MCP tool metadata (name, description, parameter names) using BM25 or keyword matching
- Discovered tools are added to the session (additive)

**Output caps**: MCP tool output is capped at 25,000 tokens with a warning at 10,000.

**Schema sanitization**: Before sending MCP tool schemas to the model:
- Infer missing `type` fields from structure (`properties` → object, `items` → array)
- Coerce `integer` → `number` for broader model compatibility
- Add `additionalProperties: false` if the model provider requires strict schemas

---

## System Prompt: Complete Tool Section

The following is the full tool guidance section to be injected into the system prompt. This is the primary mechanism for teaching any model to use tools efficiently.

```markdown
## Tool Usage

You have access to tools for file operations, search, shell commands, and more.
Follow these rules to use tools efficiently and avoid wasting context.

### Routing Rules

Always use the dedicated tool instead of shell commands:

| Task | Use This | NOT This |
|------|----------|----------|
| Read a file | `read_file` | `shell("cat file.txt")` |
| Edit a file | `edit_file` | `shell("sed -i ...")` |
| Create a file | `write_file` | `shell("echo '...' > file")` |
| Search file contents | `grep` | `shell("grep ...")` or `shell("rg ...")` |
| Find files by pattern | `find_files` | `shell("find ...")` |
| List a directory | `list_dir` | `shell("ls ...")` or `shell("tree ...")` |

Dedicated tools have controlled output that keeps your context clean.
Shell commands produce unstructured output that can flood your context.

Reserve `shell` for: running tests, building, git commands, package management,
and other tasks with no dedicated tool.

### Parallel Calls

When you need multiple independent pieces of information, call tools in parallel.
For example, if you need to read three files, call `read_file` three times in
a single response — don't read them sequentially.

Good parallel candidates:
- Multiple `read_file` calls for different files
- Multiple `grep` / `find_files` calls with different patterns
- Multiple `delegate` calls for independent sub-tasks

Do NOT parallelize when there are dependencies:
- Don't `edit_file` before `read_file` on the same file
- Don't run tests before writing the code
- Don't commit before staging

### Efficient Search

The `grep` tool returns file paths by default, not file contents. This is intentional.
Use this two-step pattern:

1. `grep` with a pattern to find relevant files (compact output)
2. `read_file` on the specific files you need (targeted content)

Only use `grep` with `mode="content"` when you need to see the matching lines
themselves (e.g., finding exact function signatures).

### File Editing

Before editing, you MUST read the file first. Then:

1. Use `edit_file` for surgical changes to existing files (preferred)
2. Use `write_file` only for creating new files

After a successful edit, do NOT re-read the file to verify. The tool confirms
success or returns an error. Trust the confirmation.

Include enough context in `old_string` to uniquely identify the location.
If the tool reports multiple matches, add more surrounding lines.

### Output Awareness

Tool outputs consume your context window. To stay efficient:
- Start searches with tight limits, broaden only if needed
- Read specific file sections (use `offset`/`limit`) rather than entire large files
- Use `think` to reason through complex problems before making tool calls
- When exploring unfamiliar code, start with `list_dir` (depth=1) and `grep`
  before reading full files
```

---

## Tool Output Truncation Strategy

All tools share a unified truncation framework, but strategies vary by tool type.

### Framework

```rust
pub struct TruncationConfig {
    /// Global max output bytes (applied to all tools).
    pub max_bytes: usize,              // default: 30_000
    /// Strategy for how to truncate.
    pub strategy: TruncationStrategy,
}

pub enum TruncationStrategy {
    /// Keep the first N bytes, append truncation notice. For file reads.
    Head,
    /// Keep first and last portions, insert truncation notice in middle. For shell output.
    HeadAndTail { tail_ratio: f32 },
    /// Keep first N lines. For line-oriented output (grep, find_files).
    HeadLines { max_lines: usize },
}
```

### Per-Tool Defaults

| Tool | Strategy | Rationale |
|------|----------|-----------|
| `read_file` | `HeadLines(2000)` | Line-based cap; model can request offset for more |
| `edit_file` | `Head(1000)` | Output is tiny (confirmation only); cap is safety net |
| `write_file` | `Head(1000)` | Same as edit_file |
| `grep` (files mode) | `HeadLines(100)` | Cap at 100 file paths; model can raise limit explicitly |
| `grep` (content mode) | `HeadLines(200)` | More generous for content, but still bounded |
| `find_files` | `HeadLines(100)` | Same as grep files mode |
| `list_dir` | `HeadLines(50)` | Paginated; model uses offset for more |
| `shell` | `HeadAndTail(0.4)` | Keep 60% head, 40% tail. Preserves command output start + error messages at end |
| `web_fetch` | `Head(8000)` | Extraction model already caps output; this is a safety net |
| `web_search` | `HeadLines(10)` | Cap at 10 search results |
| `delegate` | `Head(4000)` | Sub-agent results are pre-summarized |

### Truncation Notice Format

When truncation occurs, append a structured notice:

```
... [output truncated: {total_size} total, showing first {shown_size}. Use offset/limit for more.]
```

For `HeadAndTail`:
```
[first {head_size} of output]
... [{total_size} total — {omitted_size} omitted] ...
[last {tail_size} of output]
```

---

## Tool Result Caching

cinch-rs already has `ToolResultCache` with FNV-1a hashing. The ideal configuration:

### Cacheable Tools

| Tool | Cacheable? | Mutation? | Rationale |
|------|-----------|-----------|-----------|
| `read_file` | Yes | No | File content is stable within a session (unless edited) |
| `grep` | Yes | No | Search results are stable (unless files change) |
| `find_files` | Yes | No | File listings are stable |
| `list_dir` | Yes | No | Directory structure is stable |
| `edit_file` | No | Yes | Mutates files; invalidates read_file cache for that path |
| `write_file` | No | Yes | Same as edit_file |
| `shell` | No | Maybe | Could mutate anything; conservatively treat as mutation |
| `web_search` | Yes | No | Search results are stable within a session |
| `web_fetch` | Yes | No | Web content has TTL-based cache (15 min) |
| `think` | No | No | No output to cache |
| `todo` | No | No | Stateful; always returns current state |

### Cache Invalidation Rules

1. **Any mutation tool** (`edit_file`, `write_file`) invalidates all cached `read_file` results for the affected path (not all paths)
2. **Shell commands** conservatively invalidate the entire cache (they could modify anything)
3. **Age-based eviction**: Results older than `max_age_rounds` (default: 10) are evicted, since files may have changed
4. **Explicit invalidation**: After compaction, clear the cache (the model's context no longer contains the cached results)

---

## Tool Filtering and Progressive Loading

### Problem

Sending all tool definitions in every API request wastes context. With 10+ tools, definitions alone can consume 2-4K tokens per request — multiplied across 50+ rounds, that's 100-200K tokens wasted on identical definitions.

### Solution: Three-Tier Loading

**Always loaded** (Tier 1 core tools): `read_file`, `edit_file`, `write_file`, `grep`, `find_files`, `list_dir`, `shell`, `think`. These are always relevant and their definitions are compact (~1.5K tokens total).

**Loaded on relevance** (Tier 2): `web_search`, `web_fetch`, `todo`, `delegate`. Included in the tool list only when:
- The model has used them before in this session, OR
- The task description suggests they're relevant (keyword matching), OR
- The user explicitly enables them

**Loaded on demand** (Tier 3: MCP tools): Hidden behind `search_tools` when definitions exceed the budget. Discovered tools persist for the session.

### Implementation

cinch-rs already has `ToolFilter` with categories and usage tracking. Extend it:

```rust
pub enum ToolVisibility {
    /// Always included in tool definitions.
    AlwaysVisible,
    /// Included when relevance criteria are met.
    OnRelevance { keywords: Vec<String>, categories: Vec<String> },
    /// Hidden behind search_tools meta-tool.
    OnDemand,
}
```

Each tool declares its visibility. The `ToolSet` evaluates visibility per round:
- `AlwaysVisible` tools are always in the API request
- `OnRelevance` tools are included if they've been used, or if the current user message matches keywords
- `OnDemand` tools (MCP) are searched via meta-tool

---

## Parallel Execution Design

### Current State

cinch-rs has a DAG-based parallel execution system (`dag.rs`) that extracts `depends_on` from tool arguments. This is more sophisticated than both Codex and Claude Code, which rely on the model to naturally batch independent calls.

### Recommended Approach

Keep the DAG system as an advanced feature, but optimize for the common case: **the model naturally batches independent calls in a single response**.

**Model-side parallelism** (what the model decides):
- The system prompt instructs the model to batch independent calls
- Most models support multiple tool calls per response
- This is the primary parallelism mechanism

**Harness-side parallelism** (what cinch-rs enforces):
- When the model returns multiple tool calls, execute them concurrently via `join_all`
- Exception: tools marked `sequential_only` are executed one at a time
- The DAG system handles the rare case where tool calls have explicit dependencies

### Sequential-Only Tools

| Tool | Sequential? | Reason |
|------|------------|--------|
| `shell` | Yes | Commands may share state; ordering matters |
| `edit_file` | Per-file | Two edits to the same file must be sequential; edits to different files can be parallel |
| `write_file` | Per-file | Same as edit_file |
| All others | No | Safe to run in parallel |

---

## Implementation Phases

### Phase 1: Upgrade Existing Tools

Update the current `common.rs` tools to match the designs above.

| Task | Files | Effort |
|------|-------|--------|
| Add line numbers to `ReadFile` output (`L{n}: content`) | `common.rs` | Small |
| Add read-before-write enforcement to editing/writing | `common.rs`, `core.rs` (add `ReadTracker`) | Medium |
| Change `Grep` default to files-only mode, add `mode` parameter | `common.rs` | Small |
| Add modification-time sorting to `Grep` and `FindFiles` | `common.rs` | Small |
| Implement `HeadAndTail` truncation for `Shell` | `core.rs` | Small |
| Add `context_lines` parameter to `Grep` content mode | `common.rs` | Small |
| Add exit code prefix to `Shell` output | `common.rs` | Small |
| Add `depth` and `offset` parameters to `ListFiles` | `common.rs` | Medium |
| Update `ToolSpec` descriptions to match system prompt guidance | `spec.rs` | Small |

### Phase 2: New Tool Capabilities

| Task | Files | Effort |
|------|-------|--------|
| Implement `web_fetch` with secondary model extraction | New `web_fetch.rs` | Medium |
| Add `edit_file` as a separate tool (currently modifications go through shell?) | `common.rs` or new `edit.rs` | Medium |
| Implement `write_file` with existence check | `common.rs` | Small |
| Add per-file sequential enforcement for parallel edit/write | `dag.rs`, `execution.rs` | Medium |
| Implement path-scoped cache invalidation (not global) | `cache.rs` | Small |

### Phase 3: Tool Efficiency Infrastructure

| Task | Files | Effort |
|------|-------|--------|
| Implement `ToolVisibility` enum and per-round filtering | `filter.rs`, `core.rs` | Medium |
| Add tool definition token budget tracking | `core.rs` | Small |
| Implement `search_tools` meta-tool for MCP deferred loading | New `search_tools.rs` | Medium |
| Add `TruncationStrategy` enum with per-tool defaults | `core.rs` | Medium |
| Add truncation notice format with actionable guidance | `core.rs` | Small |

### Phase 4: System Prompt Integration

| Task | Files | Effort |
|------|-------|--------|
| Write the full tool guidance section for the system prompt | `prompt.rs`, `spec.rs` | Medium |
| Add routing rules (dedicated tool > shell) | `prompt.rs` | Small |
| Add parallel execution guidance | `prompt.rs` | Small |
| Add search pattern guidance (two-step discovery→retrieval) | `prompt.rs` | Small |
| Add edit workflow guidance (read→edit, don't re-read) | `prompt.rs` | Small |

### Phase 5: Sub-Agent Tooling

| Task | Files | Effort |
|------|-------|--------|
| Implement typed sub-agent specializations (`explore`, `worker`, `planner`) | `sub_agent.rs` | Medium |
| Add `context` parameter for parent→child context passing | `sub_agent.rs` | Small |
| Add `background` execution with `check_agent` tool | `sub_agent.rs`, new `check_agent` tool | Large |
| Per-type tool restriction enforcement | `sub_agent.rs`, `core.rs` | Medium |

---

## Success Metrics

How to measure whether the tool system is achieving its efficiency goals:

| Metric | Target | How to Measure |
|--------|--------|---------------|
| **Shell usage for file ops** | <5% of file reads/searches should use shell | Track tool name vs operation type |
| **Grep output tokens** | 80%+ of grep calls should use files mode | Track mode parameter usage |
| **Read-before-write violations** | 0 successful blind writes | Enforced at tool level |
| **Context usage per round** | Tool results should be <40% of total context | Track via `ContextBreakdown` |
| **Cache hit rate** | >20% for read-heavy sessions | Track via `ToolResultCache` stats |
| **Parallel execution ratio** | >50% of multi-tool rounds should use parallel calls | Track concurrent vs sequential execution |
| **Sub-agent cost savings** | `explore` agents should use 3-5x fewer tokens than `worker` | Track per-type token usage |
