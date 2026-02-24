# Claude Code: Agent Context, Memory & Compaction Analysis

> Comprehensive analysis of [anthropics/claude-code](https://github.com/anthropics/claude-code) — how it manages agent context, persistent memory, and conversation compaction.
>
> Based on the open-source companion repo, official documentation, changelog history, and community research.

---

## Table of Contents

1. [Architecture Overview](#1-architecture-overview)
2. [Context Management](#2-context-management)
3. [Compaction System](#3-compaction-system)
4. [Persistent Memory](#4-persistent-memory)
5. [Sub-Agent & Task Model](#5-sub-agent--task-model)
6. [Plugin & Extension Architecture](#6-plugin--extension-architecture)
7. [Key Takeaways for cinch-rs](#7-key-takeaways-for-cinch-rs)

---

## 1. Architecture Overview

Claude Code implements a **classical single-threaded agent loop**:

```
while(tool_call) → execute tool → feed results → repeat
```

The loop terminates when the model produces a plain text response with no tool calls.

### Three-Phase Execution Pattern

1. **Gather context** — search files, read code, understand the codebase
2. **Take action** — edit files, run commands, make changes
3. **Verify results** — run tests, check outputs, validate

These phases blend together; Claude decides what each step requires based on what it learned from the previous step, chaining dozens of actions and course-correcting along the way.

### Session Independence

Each new session starts with a **fresh context window**. No conversation history from previous sessions is carried over. Cross-session persistence is handled exclusively by CLAUDE.md files and the auto memory system.

---

## 2. Context Management

### 2.1 The 200K Token Context Window

Claude Code operates within a 200,000-token context window (Claude Opus 4.6 / Sonnet 4.6). The context holds:

| Content Type | Loading Behavior |
|---|---|
| Conversation history | Accumulated during session |
| File contents (Read tool) | On demand |
| Command outputs (Bash tool) | On demand |
| CLAUDE.md files | At session start (parent dirs) or on demand (child dirs) |
| Auto memory (MEMORY.md) | First 200 lines at session start |
| System instructions | Always present |
| Tool definitions (28 built-in + MCP) | Always present |
| Loaded skill content | On demand when invoked |

### 2.2 System Prompt Structure

The system prompt is **modular, not monolithic** — comprising **110+ conditionally-loaded strings**:

| Component | Approximate Size | Details |
|---|---|---|
| Core identity | ~269 tokens | Establishes Claude Code as an agentic CLI assistant |
| Tool descriptions | 122–2,167 tokens each | 28 built-in tools |
| Sub-agent prompts | Variable | Explore, Plan, Task agents with specialized instructions |
| Utility prompts | Variable | 18+ utility prompts (summarization, session mgmt, docs) |
| System reminders | 12–1,500 tokens each | ~40 brief contextual notices (file mods, token usage, plan mode, memory, hooks, tasks) |
| Embedded reference data | Variable | Claude API docs, Agent SDK patterns, tool use concepts |

Prompts use XML tags and Markdown headers for structure (e.g., `<background_information>`, `<instructions>`, `## Tool guidance`).

### 2.3 Context Costs & Budget

- **MCP servers** add tool definitions to every API request — even a few servers can consume significant context before any work begins
- **Skill descriptions** scale with context window at 2% budget (metadata always loaded; full body loads on invocation)
- **Tool descriptions** are pre-warmed with file suggestions using session-based caching
- `/context` shows detailed token accounting; `/mcp` shows per-server costs

### 2.4 Token Tracking

| Metric | Details |
|---|---|
| Effective context window | After reserving space for max output tokens (~98% threshold) |
| 1M context support | Available for Opus 4.6 fast mode (`CLAUDE_CODE_DISABLE_1M_CONTEXT` to disable) |
| Token counter | Real-time updates during streaming in status line |
| Token counting optimization | Batched MCP tool counting in single API call; deferred CLAUDE.md counting in simple mode |

---

## 3. Compaction System

### 3.1 Auto-Compaction Trigger

| Parameter | Value |
|---|---|
| Context window | 200,000 tokens |
| Buffer reservation | ~33,000 tokens (16.5%) |
| Effective working limit | ~167,000 tokens |
| Trigger percentage | ~83.5% of total window |
| Override | `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` env var (1–100) |

The 33K buffer serves three functions:
1. Working space for the summarization process itself
2. Completion buffer allowing tasks to finish before triggering
3. Response generation memory for reasoning

### 3.2 How Compaction Works

When Claude Code detects context approaching the limit:

1. **Tool result clearing** — older raw tool outputs are removed from deep message history (lightweight optimization since agents rarely re-examine old results)
2. **Conversation summarization** — the full message history is passed to the model with a summarization prompt
3. **Summary generation** — the model produces a compressed summary wrapped in `<summary></summary>` tags
4. **Context replacement** — older messages are replaced with the summary; conversation continues with compressed context **plus the five most recently accessed files**

### 3.3 Default Summarization Prompt

```
You have written a partial transcript for the initial task above. Please write a summary
of the transcript. The purpose of this summary is to provide continuity so you can continue
to make progress towards solving the task in a future context, where the raw history above
may not be accessible and will be replaced with this summary. Write down anything that would
be helpful, including the state, next steps, learnings etc. You must wrap your summary in a
<summary></summary> block.
```

### 3.4 What Gets Preserved vs. Lost

| Preserved | Lost/Summarized |
|---|---|
| Architectural decisions | Redundant tool outputs |
| Unresolved bugs & current state | Duplicate messages |
| Implementation details & key snippets | Detailed instructions from early conversation |
| User requests & next steps | Specific variable names / exact error messages from early exchanges |
| Five most recently accessed files | Nuanced decisions (compressed to "gist") |
| Plan mode state | Raw intermediate tool results |
| Session name/title | Subagent skill context (cleaned from main session) |

### 3.5 Manual Compaction

- `/compact` — triggers compaction manually at strategic breakpoints
- `/compact focus on the API changes` — compaction with a focus instruction directing what to preserve
- `/compact` is distinct from `/clear` (which wipes history entirely)
- A "Compact Instructions" section in CLAUDE.md controls what gets preserved during compaction

### 3.6 Hooks Integration

The **PreCompact** hook fires before compaction, allowing custom preservation logic:

```json
{
  "PreCompact": [{
    "hooks": [{
      "type": "prompt",
      "prompt": "Before compacting, ensure the following critical state is preserved: ..."
    }]
  }]
}
```

### 3.7 API-Level Compaction (Server-Side)

The Messages API also supports server-side compaction (beta `compact-2026-01-12`):

| Parameter | Default | Description |
|---|---|---|
| `trigger` | 150,000 tokens | When to trigger (minimum 50,000) |
| `pause_after_compaction` | `false` | Pause to allow injecting preserved messages |
| `instructions` | Default prompt | Custom summarization prompt |

When compaction fires, the API returns a `compaction` content block containing the summary. On subsequent requests, all content blocks before the last `compaction` block are automatically dropped.

### 3.8 Subagent Compaction

Subagents support automatic compaction independently, defaulting to ~95% capacity trigger. Subagent transcripts are stored separately at `~/.claude/projects/{project}/{sessionId}/subagents/agent-{agentId}.jsonl` and are unaffected by main conversation compaction.

### 3.9 Post-Compaction Cleanup

Extensive memory optimizations after compaction:
- Internal caches cleared
- API stream buffers released
- Agent context and skill state released
- Large tool results freed
- Completed task state objects removed

---

## 4. Persistent Memory

### 4.1 Memory Hierarchy

Claude Code has a multi-layered memory system loaded in precedence order (more specific overrides broader):

| Layer | Location | Scope | Shared |
|---|---|---|---|
| **Managed policy** | `/Library/Application Support/ClaudeCode/CLAUDE.md` (macOS) | Organization-wide | All users |
| **Project memory** | `./CLAUDE.md` or `./.claude/CLAUDE.md` | Team-shared | Via source control |
| **Project rules** | `./.claude/rules/*.md` | Modular, topic-specific | Via source control |
| **User memory** | `~/.claude/CLAUDE.md` | Personal, all projects | Just you |
| **Local project memory** | `./CLAUDE.local.md` | Personal, project-specific | Just you |
| **Auto memory** | `~/.claude/projects/<project>/memory/` | Automatic notes per project | Just you |

CLAUDE.md files **in parent directories** are loaded at launch. CLAUDE.md files **in child directories** load on demand when Claude reads files in those directories.

### 4.2 CLAUDE.md Features

- **Imports**: `@path/to/import` syntax; supports relative/absolute paths, recursive (max depth 5), not evaluated inside code spans/blocks
- **Project rules**: `.claude/rules/*.md` files support YAML frontmatter with `paths` field for conditional rules scoped to file glob patterns
- **User-level rules**: `~/.claude/rules/` for personal rules across all projects
- **Bootstrap**: `/init` command walks through creating a CLAUDE.md

### 4.3 Auto Memory System

Auto memory is where **Claude writes notes for itself** (as opposed to CLAUDE.md which contains instructions you write for Claude).

**Storage structure:**
```
~/.claude/projects/<project-path-hash>/memory/
├── MEMORY.md           # Concise index (first 200 lines loaded every session)
├── debugging.md        # Detailed notes on debugging patterns
├── api-conventions.md  # API design decisions
├── patterns.md         # Architecture patterns
└── ...                 # Any topic files Claude creates
```

**How it works:**
- `<project>` path is derived from the git repository root
- All subdirectories within the same repo share one auto memory directory
- Git worktrees get separate memory directories
- First 200 lines of `MEMORY.md` are **injected into the system prompt** at session start
- Lines beyond 200 are truncated — Claude is instructed to keep it concise
- Topic files (e.g., `debugging.md`) are NOT loaded at startup; Claude reads them on demand
- Claude reads and writes memory files during sessions using standard file tools (Read, Write, Edit)

**What to save** (per system prompt guidance):
- Stable patterns confirmed across multiple interactions
- Key architectural decisions, important file paths, project structure
- User preferences for workflow, tools, communication style
- Solutions to recurring problems and debugging insights

**What NOT to save:**
- Session-specific context (current task, in-progress work, temporary state)
- Incomplete or unverified information
- Anything that duplicates CLAUDE.md instructions
- Speculative conclusions from reading a single file

**Organization guidance:**
- Organize semantically by topic, not chronologically
- Check for existing memories before writing new ones
- Update or remove memories that turn out to be wrong
- Link to topic files from MEMORY.md for details

**Control:**
- `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` forces off
- `CLAUDE_CODE_DISABLE_AUTO_MEMORY=0` forces on
- Explicit: "remember that we use pnpm, not npm"
- Explicit: "forget about the old API convention"

### 4.4 Compact Instructions in CLAUDE.md

You can add a section to CLAUDE.md that guides what gets preserved during compaction:

```markdown
## Compact Instructions
When compacting context, always preserve:
- The current task and its acceptance criteria
- Any file paths that have been modified
- Test results and their status
- Architectural decisions made during this session
```

---

## 5. Sub-Agent & Task Model

### 5.1 Built-in Sub-Agents

Sub-agents get their **own fresh context window**, completely separate from the main conversation.

| Agent | Model | Tools | Purpose |
|---|---|---|---|
| **Explore** | Haiku (fast) | Read-only (Glob, Grep, Read) | Codebase search and analysis. Invoked with thoroughness: quick/medium/very thorough |
| **Plan** | Inherits | Read-only | Research and design implementation plans |
| **General-purpose** | Inherits | All tools | Complex multi-step tasks |
| **Bash** | Inherits | Bash only | Terminal commands in separate context |
| **Claude Code Guide** | Haiku | WebFetch, WebSearch, Read, Glob, Grep | Questions about Claude Code features |

### 5.2 Sub-Agent Constraints

- Sub-agents **cannot spawn other sub-agents** (prevents recursive explosion)
- At most one sub-agent branch runs at a time (foreground blocks; background runs concurrently)
- Sub-agents return condensed summaries (~1,000–2,000 tokens) while internal exploration may use tens of thousands
- Sub-agents can be resumed with full conversation history using agent IDs
- Sub-agent compaction triggers independently at ~95% capacity

### 5.3 Custom Sub-Agents

Defined as Markdown files with YAML frontmatter in `.claude/agents/` (project) or `~/.claude/agents/` (user):

```markdown
---
name: agent-identifier
description: "Use this agent when..."
model: inherit|sonnet|opus|haiku
color: blue|cyan|green|yellow|red|magenta
tools: ["Read", "Write", "Bash(git:*)"]
memory: user|project|local
isolation: worktree
background: true
---

[System prompt content — 500-3,000 words]
```

Custom agents support:
- Tool restrictions (allowlist)
- Model selection
- Persistent memory (user/project/local scope)
- Lifecycle hooks (PreToolUse, PostToolUse, Stop)
- Skill preloading
- Git worktree isolation
- Max turn limits

### 5.4 TODO-Based Planning

The `TodoWrite` tool creates structured JSON task lists with IDs, content, status, and priority. TODO state is injected via system messages after tool use, maintaining focus during extended conversations.

### 5.5 Session Management

- Sessions saved locally as conversations proceed (every message, tool use, and result)
- File snapshots taken before edits (checkpoints for reverting)
- `claude --continue` or `claude --resume` restores full conversation history
- `claude --fork-session` creates a new session ID preserving history to that point
- Session-scoped permissions are NOT preserved across resume/fork
- Graceful shutdown: session data flushed before hooks and analytics (SSH-resilient)

---

## 6. Plugin & Extension Architecture

### 6.1 Plugin Structure

```
plugin-name/
├── .claude-plugin/
│   └── plugin.json           # Metadata (name, version, description, author)
├── commands/                  # Slash commands (auto-discovered)
│   ├── command1.md
│   └── review/security.md   # Namespaced: /security (plugin:name:review)
├── agents/                    # Autonomous agents (auto-discovered)
│   └── agent-one.md
├── skills/                    # Knowledge packages (auto-discovered)
│   └── skill-name/
│       ├── SKILL.md          # Required entry point
│       ├── references/       # Detailed docs (loaded on demand)
│       ├── examples/         # Working code (loaded on demand)
│       └── scripts/          # Utility scripts (loaded on demand)
├── hooks/
│   └── hooks.json            # Event handlers
├── .mcp.json                  # External tool servers
├── settings.json              # Default settings
└── README.md
```

### 6.2 Skills: Progressive Disclosure

Three-level loading manages context efficiently:

1. **Metadata** (~100 words) — skill name + description, always loaded
2. **SKILL.md body** (<5K words) — loads when skill triggers
3. **References/Scripts/Examples** — unlimited, loaded on-demand by Claude

### 6.3 Hook Events

| Event | When | Use Cases |
|---|---|---|
| **PreToolUse** | Before tool executes | Approve/deny/modify tool calls |
| **PostToolUse** | After tool completes | React to results, provide feedback |
| **Stop** | Main agent considers stopping | Validate task completion |
| **SubagentStop** | Subagent considers stopping | Ensure subagent completed task |
| **UserPromptSubmit** | User submits prompt | Add context, validate inputs |
| **SessionStart** | Session begins | Load context, set environment |
| **SessionEnd** | Session ends | Cleanup, state preservation |
| **PreCompact** | Before context compaction | Preserve critical info |
| **Notification** | Claude sends notification | React to notifications |
| **ConfigChange** | Config files change | Security auditing |

Hooks can be **prompt-based** (context-aware, natural language) or **command-based** (deterministic shell scripts). All matching hooks run in parallel.

### 6.4 Settings Hierarchy

1. Enterprise/managed settings (highest priority)
2. Project settings (`.claude/settings.json`)
3. Session settings (`.claude/settings.local.json`)
4. User settings (`~/.claude.json`)
5. Defaults (lowest priority)

---

## 7. Key Takeaways for cinch-rs

### Context Management Patterns

1. **Modular system prompt** (110+ conditional strings) is far superior to a monolithic prompt — enables prompt caching and selective loading.

2. **Context budget awareness** — MCP tools, skill descriptions, and CLAUDE.md all consume context before user work begins. Track and expose these costs (via `/context`).

3. **Session independence** — each session starts fresh. Cross-session state lives entirely in files (CLAUDE.md, auto memory), not in conversation history.

4. **Tool result clearing** as a lightweight pre-compaction step — remove old raw tool outputs before expensive summarization.

### Compaction Patterns

1. **Single-pass summarization** with `<summary>` tags is simpler than Codex's two-tier local/remote system. The model generates the summary itself.

2. **Five most recently accessed files preserved** alongside the summary — ensures the model can continue working on current files without re-reading.

3. **Focused compaction** (`/compact focus on X`) lets users direct what's preserved. The "Compact Instructions" section in CLAUDE.md makes this persistent.

4. **PreCompact hook** allows programmatic preservation of critical state before compaction runs.

5. **`CLAUDE_AUTOCOMPACT_PCT_OVERRIDE`** env var for per-project tuning of the trigger threshold.

6. **Post-compaction cleanup** is critical — clear caches, release buffers, free completed task state to prevent memory leaks in long sessions.

### Memory Patterns

1. **File-based memory** (CLAUDE.md + auto memory) is simpler than Codex's DB-backed two-phase pipeline. The model reads/writes memory using the same file tools it uses for code.

2. **200-line MEMORY.md limit** in system prompt is a pragmatic cap. Detailed notes go in topic files that load on demand.

3. **Semantic organization by topic** (not chronological) makes memory searchable and maintainable.

4. **Hierarchical CLAUDE.md** (managed → project → rules → user → local) enables organization-wide policies with project-specific overrides.

5. **Conditional rules** via `paths` frontmatter in `.claude/rules/*.md` — rules that only apply when working on matching files.

6. **Import system** (`@path/to/import`) allows CLAUDE.md composition without duplication.

### Agent Architecture Patterns

1. **Sub-agents get fresh context windows** — this is the key scalability mechanism. The main conversation sees only a 1-2K token summary, while the sub-agent may have used 50K+ tokens internally.

2. **No recursive sub-agents** — hard limit prevents runaway costs and complexity.

3. **Foreground vs background** distinction for sub-agents — foreground blocks (sequential), background runs concurrently.

4. **Custom agents as Markdown files** with YAML frontmatter is an elegant, version-controllable format.

5. **Progressive skill loading** (metadata → body → references) minimizes context cost while maintaining discoverability.

6. **Hook-based lifecycle** (PreToolUse, PostToolUse, Stop, PreCompact, etc.) provides extensibility without modifying core logic.

### Key Numbers

| Parameter | Value |
|---|---|
| Context window | 200,000 tokens |
| Buffer reservation | ~33,000 tokens (16.5%) |
| Auto-compact trigger | ~83.5% of window |
| MEMORY.md auto-load | First 200 lines |
| CLAUDE.md import depth | Max 5 hops |
| System prompt components | 110+ conditional strings |
| Built-in tools | 28 |
| Skill budget | 2% of context window |
| Sub-agent summary return | ~1,000–2,000 tokens |
| Sub-agent auto-compact | ~95% capacity |
| API compaction trigger | 150,000 tokens (default) |
| API compaction minimum | 50,000 tokens |

---

## Sources

- [How Claude Code works](https://code.claude.com/docs/en/how-claude-code-works)
- [Manage Claude's memory](https://code.claude.com/docs/en/memory)
- [Create custom subagents](https://code.claude.com/docs/en/sub-agents)
- [Compaction — Claude API Docs](https://platform.claude.com/docs/en/build-with-claude/compaction)
- [Effective context engineering for AI agents (Anthropic)](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents)
- [Piebald-AI/claude-code-system-prompts](https://github.com/Piebald-AI/claude-code-system-prompts)
- [anthropics/claude-code (GitHub)](https://github.com/anthropics/claude-code)
