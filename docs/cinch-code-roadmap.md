# cinch-code Roadmap

## Current State

`cinch-code` is a new workspace crate providing a terminal coding agent built on `cinch-rs` (agent harness) and `cinch-tui` (terminal UI). It currently includes:

- **Git tools**: `git_status`, `git_diff`, `git_log`, `git_commit` — struct-based `Tool` impls with `GitToolsExt` trait for ToolSet registration
- **CodeConfig**: coding-tuned defaults (sonnet-4, 50 rounds, 16384 tokens, temp 0.3, streaming) with `build_harness_config()` and `build_tool_set()`
- **System prompt**: minimal coding-focused prompt via `coding_system_prompt()`
- **Binary**: clap CLI that spawns a TUI and runs a single harness invocation with `--prompt`

The binary is functional for one-shot use but lacks the interactive loop, project awareness, and safety features needed for real coding work.

---

## High Priority — Functional Gaps

### 1. Interactive Input

**Problem**: `main.rs` requires `--prompt` and exits if absent. There is no way to type a prompt into the TUI.

**Approach**: The TUI already has `InputMode` variants and input handling (`cinch-tui/src/input.rs`). Wire up an initial input mode where the user types their prompt, then the harness runs. This likely means:

- Start the TUI in an "awaiting input" state (no harness running yet)
- When the user submits text, spawn the harness on the tokio runtime
- Display results in the agent output pane

**Files**: `crates/cinch-code/src/main.rs`

### 2. Multi-Turn Conversation Loop

**Problem**: The binary runs a single `Harness::run()` call and then waits for TUI quit. A coding agent needs iterative back-and-forth: run → show result → accept next prompt → run again, preserving conversation history.

**Approach**: After the harness completes, return to the input state. Accumulate `messages` across turns so the LLM has full conversation context. Consider context limits — the harness already handles eviction/summarization within a run, but cross-run context management may need thought.

**Files**: `crates/cinch-code/src/main.rs`

### 3. Project Root Detection

**Problem**: `--workdir` defaults to `"."`, which may not be the git repository root. Tools resolve paths relative to workdir, so running from a subdirectory breaks file references.

**Approach**: Auto-detect the git root via `git rev-parse --show-toplevel` and use it as the default workdir. Fall back to the current directory if not in a git repo.

**Files**: `crates/cinch-code/src/config.rs` or `crates/cinch-code/src/main.rs`

---

## Medium Priority — Quality of Life

### 4. Project Instructions (AGENTS.md)

**Problem**: `CodeConfig::build_harness_config()` doesn't call `with_project_root()`, so project-level instructions from `AGENTS.md` are never loaded. The agent has no project-specific context.

**Approach**: Call `with_project_root(&self.workdir)` in `build_harness_config()`. This loads the AGENTS.md hierarchy and forwards compaction instructions to the summarizer.

**Files**: `crates/cinch-code/src/config.rs`

### 5. Memory System

**Problem**: The harness memory system (MEMORY.md loading, memory prompt injection) is not wired up. The agent cannot persist learnings across sessions.

**Approach**: Add `memory_dir: Option<String>` to `CodeConfig`. In `build_harness_config()`, set `with_memory_file()` pointing to `{workdir}/.agents/memory/MEMORY.md` (or a configurable path). The harness handles the rest (reading, injecting into system prompt, post-session consolidation).

**Files**: `crates/cinch-code/src/config.rs`

### 6. Approval Gating on GitCommit

**Problem**: `git_commit` is marked as `is_mutation() -> true` but runs without human confirmation. The agent could create commits the user didn't intend.

**Approach**: Add `git_commit` to `approval_required_tools` in `build_harness_config()`. The harness will emit an `ApprovalRequired` event, and the TUI's question system will prompt the user before execution. Consider also gating `shell` for destructive commands.

**Files**: `crates/cinch-code/src/config.rs`

### 7. Session Persistence

**Problem**: The harness session system (round checkpoints, manifests) writes to `.agents/sessions` by default, but this isn't configured relative to the workdir. Sessions may land in unexpected locations.

**Approach**: Set `session.sessions_dir` to `{workdir}/.agents/sessions` in `build_harness_config()`. This co-locates sessions with the project.

**Files**: `crates/cinch-code/src/config.rs`

---

## Lower Priority — Polish

### 8. Model Routing and Fallbacks

**Problem**: Only a single model is supported. No fallback if the primary model is unavailable or rate-limited.

**Approach**: Add `fallback_models: Vec<String>` to `CodeConfig`. Map to `RoutingStrategy::Fallback` in `build_harness_config()` when non-empty. Expose via `--fallback-model` CLI flag.

**Files**: `crates/cinch-code/src/config.rs`, `crates/cinch-code/src/main.rs`

### 9. Session Resume (`--resume`)

**Problem**: No way to resume a previous agent session. If the agent is interrupted, all context is lost.

**Approach**: Add `--resume <session-id>` CLI flag. Load the session manifest and checkpoint messages, then continue the harness from where it left off. The harness session system already saves per-round checkpoints.

**Files**: `crates/cinch-code/src/main.rs`

### 10. Git Branch Tools

**Problem**: The agent can view status, diff, log, and commit, but cannot create or switch branches. Branch workflows (feature branches, stashing) require shell tool workarounds.

**Approach**: Add `GitBranch` (list/create branches) and `GitCheckout` (switch branches) tools. Gate `GitCheckout` behind approval since it modifies working tree state.

**Files**: `crates/cinch-code/src/tools/git.rs`, `crates/cinch-code/src/tools/mod.rs`

### 11. Configurable Shell Blocklist

**Problem**: The common `Shell` tool uses `DEFAULT_BLOCKED_COMMANDS` (`rm -rf /`, `mkfs`, `> /dev/`), but a coding agent may want stricter or looser controls.

**Approach**: Add `shell_blocked_commands: Option<Vec<String>>` to `CodeConfig`. Pass through via `CommonToolsConfig` in `build_tool_set()`. Consider a coding-specific default blocklist.

**Files**: `crates/cinch-code/src/config.rs`

### 12. Streaming Display

**Problem**: Streaming is enabled in `CodeConfig` but the harness streaming path needs the TUI to render `TextDelta` events incrementally. The `UiEventHandler` already handles `TextDelta` → `streaming_buffer`, but end-to-end wiring should be verified.

**Approach**: Test that streaming works end-to-end: harness emits `TextDelta` events → `UiEventHandler` updates `streaming_buffer` → TUI renders partial text. Fix any gaps in the pipeline.

**Files**: Verification across `cinch-rs` harness, `cinch-tui` rendering
