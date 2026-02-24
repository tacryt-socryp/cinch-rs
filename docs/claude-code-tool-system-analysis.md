# Claude Code Tool System Analysis

Ground-truth analysis from Claude Code's system prompt (observed in-session), the [anthropics/claude-code](https://github.com/anthropics/claude-code) companion repo, and community-tracked prompt archives.

---

## Tool Inventory (24+ built-in)

### File Operations

| Tool | Parameters | Parallel? | Notes |
|------|-----------|-----------|-------|
| `Read` | `file_path`, `offset?`, `limit?`, `pages?` | Yes | 2000-line default limit. Supports images (PNG/JPG), PDFs (max 20 pages/request), Jupyter notebooks. Output: `cat -n` format with line numbers. Lines >2000 chars truncated. |
| `Edit` | `file_path`, `old_string`, `new_string`, `replace_all?` | Yes | Exact string replacement. **Must Read file first in session or errors.** Fails if `old_string` not unique unless `replace_all=true`. |
| `Write` | `file_path`, `content` | Yes | Overwrites entire file. **Must Read existing file first.** Prompt says "ALWAYS prefer editing existing files." |
| `NotebookEdit` | `notebook_path`, `new_source`, `cell_id?`, `cell_type?`, `edit_mode?` | Yes | Cell-level Jupyter editing. Modes: replace, insert, delete. |

### Search

| Tool | Parameters | Parallel? | Notes |
|------|-----------|-----------|-------|
| `Glob` | `pattern`, `path?` | Yes | Fast file pattern matching. Returns paths sorted by modification time. |
| `Grep` | `pattern`, `path?`, `output_mode?`, `glob?`, `type?`, `-A`, `-B`, `-C`, `-i`, `-n`, `multiline?`, `head_limit?`, `offset?` | Yes | Built on ripgrep. **Default output: `files_with_matches` (paths only, not content)**. Also supports `content` and `count` modes. |

### Shell Execution

| Tool | Parameters | Parallel? | Notes |
|------|-----------|-----------|-------|
| `Bash` | `command`, `description?`, `timeout?` (max 600s, default 120s), `run_in_background?` | Depends | Working directory persists between calls; shell state does not. Output truncated at ~30KB. |
| `TaskOutput` | `task_id`, `block?`, `timeout?` | Yes | Retrieves output from background tasks (shells, agents, remote sessions). Supports blocking/non-blocking. |
| `TaskStop` | `task_id` | Yes | Terminates background tasks. |

### Web

| Tool | Parameters | Parallel? | Notes |
|------|-----------|-----------|-------|
| `WebFetch` | `url`, `prompt` | Yes | Fetches URL → converts HTML to markdown → processes with **small/fast model** (not main model). 15-min cache. Fails for authenticated URLs. |
| `WebSearch` | `query`, `allowed_domains?`, `blocked_domains?` | Yes | Returns structured search result blocks. System prompt mandates "Sources:" section in response. |

### Task Management

| Tool | Parameters | Parallel? | Notes |
|------|-----------|-----------|-------|
| `TaskCreate` | `subject`, `description`, `activeForm?`, `metadata?` | Yes | Creates tasks with `pending` status. |
| `TaskUpdate` | `taskId`, `status?`, `subject?`, `description?`, `addBlocks?`, `addBlockedBy?` | Yes | Status: pending → in_progress → completed (or deleted). |
| `TaskList` | (none) | Yes | Summary of all tasks. |
| `TaskGet` | `taskId` | Yes | Full task details with dependency graph. |

### Agent Delegation

| Tool | Parameters | Parallel? | Notes |
|------|-----------|-----------|-------|
| `Task` (sub-agent) | `subagent_type`, `prompt`, `description`, `model?`, `isolation?`, `run_in_background?`, `resume?` | Yes | Launches autonomous sub-agents. Can run in worktree isolation. Supports resume by agent ID. |
| `Skill` | `skill`, `args?` | No | Executes user-defined slash commands (`/commit`, `/review-pr`). Skills expand to full prompts. |

### Planning & Mode Control

| Tool | Parameters | Parallel? | Notes |
|------|-----------|-----------|-------|
| `EnterPlanMode` | (none) | No | Switches to plan mode (exploration only, no writes). |
| `ExitPlanMode` | `allowedPrompts?` | No | Exits plan mode with plan for user approval. |
| `EnterWorktree` | `name?` | No | Creates isolated git worktree for branched work. Auto-cleanup if no changes. |

### User Interaction

| Tool | Parameters | Parallel? | Notes |
|------|-----------|-----------|-------|
| `AskUserQuestion` | `questions` (1-4, each with 2-4 options), `multiSelect?` | No | Structured multi-choice. Supports markdown previews for visual comparison. |

### Conditional / Feature-Gated

| Tool | Condition | Notes |
|------|-----------|-------|
| MCP tools (`mcp__*`) | MCP servers configured | Appear as `mcp__<server>__<tool>` with full JSON Schema params |
| `ToolSearch` | MCP tools exceed 10% of context | Lazy-loads MCP tool definitions on demand |
| `Computer` (Chrome) | Chrome extension configured | Browser automation |
| `LSP` | Code intelligence plugins | go-to-definition, find references, diagnostics |
| `getDiagnostics` | VS Code environment | Language server errors/warnings |
| `executeCode` | Jupyter/notebook context | Python execution in persistent kernel |
| `TeammateTool` / `SendMessageTool` / `TeamDelete` | Multi-agent swarm mode | Cross-agent coordination |

---

## Key Design Decisions

### 1. Specialized Tools Over Shell Commands

Claude Code's most distinctive design choice: **dedicated tools replace common shell operations**.

The system prompt explicitly forbids shell-based alternatives:

| Need | Required Tool | Forbidden Shell Alternative |
|------|--------------|---------------------------|
| Find files | `Glob` | `find`, `ls` |
| Search contents | `Grep` | `grep`, `rg` |
| Read files | `Read` | `cat`, `head`, `tail` |
| Edit files | `Edit` | `sed`, `awk` |
| Write files | `Write` | `echo >`, `cat <<EOF` |
| Output text | Direct response | `echo`, `printf` |

**Why this matters**: Each specialized tool controls its output format precisely. `Grep` defaults to file paths only. `Read` outputs numbered lines. `Edit` requires exact string matches. This prevents uncontrolled shell output from flooding context.

**Contrast with Codex**: Codex uses `rg` via shell commands directly. Claude Code wraps `rg` in a `Grep` tool that enforces output modes and limits.

### 2. Read-Before-Write Enforcement

Both `Edit` and `Write` **fail if the file hasn't been Read in the current session**. This is enforced at the tool level, not just the prompt level.

This serves two purposes:
- **Safety**: Prevents blind overwrites of files the model hasn't seen
- **Accuracy**: Ensures the model's `old_string` in Edit matches the actual file content

**Contrast with Codex**: `apply_patch` has no such enforcement — it trusts the model to produce correct context lines in the diff.

### 3. WebFetch Uses a Separate Model

`WebFetch` doesn't return raw HTML or even raw markdown to the main model. Instead:
1. Fetches URL → converts HTML to markdown
2. Sends markdown + user's `prompt` to a **small, fast model**
3. Returns the small model's extracted answer

This is a **context-saving architecture** — web pages can be enormous, but only the distilled answer enters the main context window.

### 4. Grep Defaults to Paths-Only

Like Codex's `grep_files`, Claude Code's `Grep` defaults to `files_with_matches` mode (file paths only). The model must then `Read` specific files for content. This two-step pattern keeps search results compact.

Additional features beyond Codex:
- `content` mode with context lines (`-A`, `-B`, `-C`)
- `count` mode for match statistics
- `head_limit` and `offset` for pagination
- `multiline` mode for cross-line patterns
- File type filtering (`type` parameter)

### 5. Sub-Agent Type System with Tool Restrictions

Sub-agents launched via `Task` receive restricted tool access based on type:

| Sub-Agent Type | Tools Available |
|---------------|----------------|
| `general-purpose` | All tools |
| `Explore` | Glob, Grep, Read, Bash (no Edit, Write, NotebookEdit) |
| `Plan` | All except Task, ExitPlanMode, Edit, Write, NotebookEdit |
| `statusline-setup` | Read, Edit only |
| `claude-code-guide` | Glob, Grep, Read, WebFetch, WebSearch |

This enables:
- **Read-only exploration** agents that can't accidentally modify files
- **Lightweight specialist** agents with minimal tool overhead
- **Full-capability** agents for complex delegated tasks

Sub-agents also support:
- `isolation: "worktree"` for git-isolated work
- `run_in_background: true` for non-blocking execution
- `resume` by agent ID for continuing prior work
- `model` override (sonnet, opus, haiku) for cost/speed tradeoffs

### 6. MCP Tool Deferred Loading

When MCP servers provide many tools, Claude Code avoids loading all tool schemas upfront:

- If total MCP tool definitions exceed **10% of context**, tools are deferred
- A `ToolSearch` meta-tool is provided instead
- Model searches for relevant tools on-demand
- Discovered tools are added to the session (additive, like Codex's `search_tool_bm25`)

**Output limits for MCP**: Warning at 10,000 tokens, hard limit at 25,000 tokens per tool output (configurable via `MAX_MCP_OUTPUT_TOKENS`).

### 7. Background Execution Model

Bash commands and sub-agents can run in the background:

- `Bash` with `run_in_background: true` → check via `TaskOutput`
- `Task` with `run_in_background: true` → returns `output_file` path
- `TaskOutput` with `block: false` for non-blocking status checks
- `TaskStop` to terminate

This enables parallel workflows where the model starts a long-running build/test, continues other work, then checks results.

---

## System Prompt Tool Guidance

### Routing Hierarchy

The system prompt establishes clear tool preferences:

1. **Dedicated tool > Bash**: Always prefer Glob/Grep/Read/Edit/Write over shell equivalents
2. **MCP tool > WebFetch**: "If an MCP-provided web fetch tool is available, prefer using that tool"
3. **gh CLI > WebFetch**: "For GitHub URLs, prefer using the gh CLI via Bash instead"
4. **Direct Glob/Grep > Task agent**: "For simple, directed codebase searches use Glob or Grep directly"
5. **Task agent > repeated Glob/Grep**: "For broader codebase exploration, use the Task tool with subagent_type=Explore"

### Parallel Execution Guidance

Repeated throughout the system prompt:

- "When multiple independent pieces of information are requested and all commands are likely to succeed, run multiple tool calls in parallel"
- "It is always better to speculatively perform multiple searches in parallel if they are potentially useful" (Glob, Read)
- "Launch multiple agents concurrently whenever possible" (Task)
- "If the commands depend on each other, use a single Bash call with `&&` to chain them"

**Dependency chains that must be sequential**: mkdir→cp, Write→git add, git add→git commit, Read→Edit/Write.

### Git Safety Protocol

Extensive tool-usage rules for git operations:
- NEVER force push, `--no-verify`, amend without explicit request
- NEVER commit without being asked
- Prefer `git add` specific files over `git add -A`
- Use HEREDOC for commit messages
- After hook failure: fix, re-stage, NEW commit (never amend)

### Cost-Aware Tool Selection

The system prompt guides model selection for sub-agents:
- "Prefer haiku for quick, straightforward tasks to minimize cost and latency"
- Different agent types for different scopes (Explore for read-only, general-purpose for full capability)

---

## Output Handling

### Truncation Limits

| Tool | Limit | Strategy |
|------|-------|----------|
| `Bash` | 30,000 characters | Truncated before return |
| `Read` | 2,000 lines default, 2,000 chars/line | Configurable via offset/limit |
| `Grep` | Configurable via `head_limit` | Pagination with offset |
| `MCP tools` | 25,000 tokens (configurable) | Warning at 10,000 |
| `PDF Read` | 20 pages max per request | Must specify page range for large PDFs |

### Output Formats

- **Read**: `cat -n` numbered lines (`     1→content`)
- **Grep files_with_matches**: One file path per line
- **Grep content**: Matching lines with optional context, line numbers
- **Glob**: File paths sorted by modification time
- **Bash**: Raw stdout/stderr combined
- **WebFetch**: Processed summary from small model
- **WebSearch**: Structured result blocks with markdown links
- **TaskOutput**: Incremental output (new since last check)

---

## Implications for cinch-rs

### High-Priority Adoptions
1. **Specialized tools over shell commands** — Wrap common operations (grep, file read, file edit) in dedicated tools with controlled output formats rather than relying on shell execution
2. **Read-before-write enforcement** — Require file read before edit/write at the tool level, not just prompt level
3. **Default to paths-only search** — Grep/search tools should return file paths by default, with content as an opt-in mode
4. **Sub-agent tool restrictions** — Different agent types should receive different tool subsets based on their role

### Medium-Priority Adoptions
5. **WebFetch with secondary model** — Use a smaller/cheaper model to process fetched web content before injecting into main context
6. **Background execution model** — Support background tasks with incremental output retrieval
7. **MCP tool deferred loading** — Lazy-load tool schemas when MCP tool count is large, using a search meta-tool
8. **Parallel execution guidance in system prompt** — Explicitly instruct the model when and how to parallelize tool calls

### Lower-Priority
9. **Structured user interaction** — Multi-choice question tool with markdown previews for visual comparison
10. **Plan mode with tool restrictions** — A mode that limits available tools to read-only for exploration before committing to an approach
