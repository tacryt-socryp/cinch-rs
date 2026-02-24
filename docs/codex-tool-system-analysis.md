# Codex Tool System Analysis

Ground-truth analysis from [openai/codex](https://github.com/openai/codex) source code (`codex-rs/core/`).

---

## Tool Inventory (~30 tools)

### Shell Execution
| Tool | Description | Parallel? |
|------|-------------|-----------|
| `shell` | Runs command in persistent shell session | No |
| `shell_command` | Runs command and returns output (Responses API variant) | No |
| `exec_command` | Same as shell_command (Chat Completions variant) | No |
| `write_stdin` | Sends input to a running shell session | No |

### File Operations
| Tool | Description | Parallel? |
|------|-------------|-----------|
| `read_file` | Read file with slice or indentation-aware mode | Yes |
| `list_dir` | BFS directory listing with depth/pagination | Yes |
| `grep_files` | rg-based file search (paths only) | Yes |

### File Editing
| Tool | Description | Parallel? |
|------|-------------|-----------|
| `apply_patch` | Freeform Lark grammar-based patch (not JSON) | Yes |

### Planning & User
| Tool | Description | Parallel? |
|------|-------------|-----------|
| `update_plan` | Step-by-step plan tracking | N/A |
| `request_user_input` | Ask user for clarification | N/A |

### Multi-Agent
| Tool | Description | Parallel? |
|------|-------------|-----------|
| `spawn_agent` | Launch sub-agent with instructions | Yes |
| `send_message_to_agent` | Send message to running agent | Yes |
| `resume_agent` | Resume paused agent | Yes |
| `wait_for_agent` | Block until agent completes | No |
| `close_agent` | Terminate agent | Yes |

### Extended / Feature-Gated
| Tool | Description | Gate |
|------|-------------|------|
| `js_repl` | JavaScript REPL | Feature flag |
| `view_image` | View image file | Feature flag |
| `web_search` | Web search | `WebSearchMode` config |
| `search_tool_bm25` | BM25 search over MCP tool metadata | Apps integration |
| `mcp_list_resources` / `mcp_read_resource` | MCP resource access | MCP servers configured |

---

## Key Design Decisions

### 1. apply_patch: Freeform Grammar Over JSON

Codex's most distinctive tool. Uses a Lark grammar (not JSON-wrapped diffs) so the model writes patches naturally:

```
*** Begin Patch
*** Add File: path/to/new_file.py
+line one
+line two

*** Update File: path/to/existing.py
@@ def example():
-    pass
+    return 123

*** Delete File: path/to/old.py
*** End Patch
```

**Why this matters**: Avoids JSON escaping of code (which wastes tokens and introduces errors). The grammar is designed to look like a natural unified diff that models produce well.

### 2. read_file: Indentation-Aware Mode

Beyond simple slice reading (offset + limit), `read_file` has an **indentation mode** for reading code blocks:

```rust
struct IndentationArgs {
    anchor_line: usize,      // Center line to read around
    max_levels: usize,       // How many indentation levels up to expand
    include_siblings: bool,  // Include sibling blocks at same level
    include_header: bool,    // Include file header (imports, etc.)
    max_lines: usize,        // Total line budget
}
```

This walks up the indentation tree from an anchor line, expanding to include parent scopes. For example, reading line 50 of a deeply nested function would automatically include the function signature, class definition, and import header — providing full context without reading the entire file.

**Constants**: `MAX_LINE_LENGTH = 500`, `TAB_WIDTH = 4`, default `limit = 2000 lines`.
**Output format**: `L{number}: {content}` per line (numbered for model reference).

### 3. grep_files: Paths Only, Sorted by Modification Time

```rust
command.arg("--files-with-matches")  // Only file paths, not content
       .arg("--sortr=modified")       // Most recently modified first
       .arg("--regexp").arg(pattern)
       .arg("--no-messages");         // Suppress error messages
```

**Design rationale**: Returns file paths only (not matched content), then the model uses `read_file` on relevant matches. This keeps grep output compact and lets the model decide how much context to read. Sorting by modification time surfaces recently-changed files first.

**Limits**: `DEFAULT_LIMIT = 100`, `MAX_LIMIT = 2000`, `TIMEOUT = 30s`.

### 4. list_dir: BFS with Depth Control and Pagination

- BFS traversal (not recursive DFS) for predictable ordering
- **Depth control**: default 2 levels deep
- **Pagination**: 1-indexed offset, default 25 entries per page
- **Sorting**: Alphabetical with type indicators (`/` dir, `@` symlink, `?` other)
- **Indentation**: 2 spaces per depth level
- **Truncation**: `MAX_ENTRY_LENGTH = 500` chars per entry name

### 5. Feature-Gated Tool Loading

Tools are conditionally enabled via `ToolsConfig`:

```rust
struct ToolsConfig {
    shell_tool_type: ConfigShellToolType,  // Shell, ShellCommand, ExecCommand
    web_search_mode: WebSearchMode,        // Enabled, Disabled
    experimental_supported_tools: Vec<String>,  // Dynamic tool list
    mcp_servers: Vec<McpServerConfig>,     // MCP integrations
}
```

Tools not enabled are never sent to the model, keeping the tool schema compact and focused. This avoids token waste on irrelevant tool definitions.

### 6. Parallel Execution Marking

Each tool explicitly declares whether it supports parallel invocation. File operations (`read_file`, `list_dir`, `grep_files`, `apply_patch`) are parallel-safe. Shell commands are sequential-only (shared session state).

### 7. Output Truncation Strategy

Multi-strategy approach in `truncate.rs`:

- **Byte budget**: `APPROX_BYTES_PER_TOKEN = 4` for quick estimation
- **Telemetry preview**: First 2KB / 64 lines for logging
- **Structured truncation**: For JSON output, preserves structure while trimming
- **Begin+End preservation**: For large outputs, keeps first and last portions with a truncation notice in the middle
- **Per-tool formatting**: Each handler controls its own output format

---

## System Prompt Tool Guidance

The system prompt (`prompt.md`) embeds specific tool usage patterns:

1. **Prefer `rg` over `grep`**: "prefer using `rg` or `rg --files` because `rg` is much faster"
2. **Don't re-read after patching**: "Do not waste tokens by re-reading files after calling `apply_patch`"
3. **Group related actions**: Preamble messages should logically group related tool calls
4. **Start specific, broaden**: "start as specific as possible to the code you changed, then make your way to broader tests"
5. **Don't over-read**: "Do not use python scripts to attempt to output larger chunks of a file"

### BM25 Search for MCP Tool Discovery

When MCP servers are connected, tools are hidden by default and discovered via `search_tool_bm25`:

```
query: "focused terms describing needed capability"
limit: 5-10 (default 8)
```

Matches against: `name`, `tool_name`, `server_name`, `title`, `description`, `connector_name`, `input_keys`. Results are **additive** — discovered tools persist for the session.

---

## Schema Sanitization for MCP Tools

Codex sanitizes MCP tool schemas before sending to the model:
- **Type inference**: If `type` field is missing, infers from other fields (`properties` → object, `items` → array)
- **Integer coercion**: `integer` → `number` for broader compatibility
- **Missing properties fill**: Adds empty `properties: {}` if an object type lacks them
- **Strict mode**: Adds `additionalProperties: false` for OpenAI structured output compliance

---

## Implications for cinch-rs

### High-Priority Adoptions
1. **Indentation-aware file reading** — The anchor + level expansion approach is significantly better than simple offset/limit for code navigation
2. **Parallel execution declarations** — Explicitly marking tools as parallel-safe enables the model to batch file reads
3. **Paths-only grep with modification sorting** — Compact results that leverage recency bias
4. **Feature-gated tool loading** — Only send relevant tool schemas to minimize prompt overhead

### Medium-Priority Adoptions
5. **Freeform patch grammar** — Consider a similar non-JSON approach for file editing to reduce token waste
6. **BFS directory listing with pagination** — Predictable, paginated output is better than unbounded recursive listing
7. **Output truncation with begin+end preservation** — Keep structural context even when truncating large outputs
8. **System prompt tool guidance** — Embed usage patterns directly in the prompt to guide efficient tool use

### Lower-Priority
9. **BM25 tool discovery** — Only relevant when MCP tool count is large enough to warrant hiding/searching
10. **Schema sanitization** — Defensive handling of third-party tool schemas
