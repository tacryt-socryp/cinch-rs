# cinch-rs

An opinionated Rust framework for building LLM-powered tool-use agents on top of the [OpenRouter](https://openrouter.ai/) chat completions API.

At its core is the **Harness** — a reusable agentic loop that sends messages to an LLM, executes tool calls in parallel, appends results, and repeats until the model produces a final text response. All advanced modules are enabled by default with sensible defaults:

- Context eviction and LLM-based summarization
- Tool result caching
- Plan-execute workflows
- Sub-agent delegation with token budgets
- Checkpointing and resume
- Cost tracking and token budgeting
- Streaming responses (SSE)
- Human-in-the-loop approval
- Structured prompt assembly with cache-aware section ordering

## Workspace

| Crate | Description |
|---|---|
| [`cinch-rs`](crates/cinch-rs/) | Core framework library and CLI |
| [`cinch-tui`](crates/cinch-tui/) | Terminal UI built on [ratatui](https://ratatui.rs/) + crossterm |
| [`cinch-web`](crates/cinch-web/) | Browser UI built on [axum](https://github.com/tokio-rs/axum) with WebSocket |

## Quick Start

**Prerequisites:** Rust 1.93+, an [OpenRouter API key](https://openrouter.ai/)

```bash
export OPENROUTER_KEY=sk-or-...
cargo run --example basic_agent
```

### Minimal Agent (~30 lines)

```rust
use cinch_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), String> {
    let client = OpenRouterClient::new(
        std::env::var("OPENROUTER_KEY").unwrap(),
    )?;

    let tools = ToolSet::new().with_common_tools(".");

    let config = HarnessConfig::new(
        "anthropic/claude-sonnet-4",
        "You are a helpful coding assistant. Be concise.",
    )
    .with_max_rounds(10)
    .with_max_tokens(4096);

    let messages = vec![
        Message::system("You are a helpful coding assistant. Be concise."),
        Message::user("List the files in this directory and summarize what the project does."),
    ];

    let result = Harness::new(&client, &tools, config)
        .with_event_handler(&LoggingHandler)
        .run(messages)
        .await?;

    println!("{}", result.text());
    println!(
        "--- {} rounds | {} tokens | ${:.4} ---",
        result.rounds_used, result.total_tokens(), result.estimated_cost_usd
    );
    Ok(())
}
```

### Custom Tools

Define domain-specific tools with typed arguments and rich descriptions:

```rust
use cinch_rs::prelude::*;
use cinch_rs::schemars;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
struct LookupWordArgs {
    /// The word to look up.
    word: String,
}

fn lookup_word_tool() -> FnTool {
    let def = ToolSpec::builder("lookup_word")
        .purpose("Look up the definition of a word")
        .when_to_use("When the user asks about a word's meaning")
        .when_not_to_use("When the user wants a translation")
        .parameters_for::<LookupWordArgs>()
        .build()
        .to_tool_def();

    FnTool::new(def, |args: LookupWordArgs| async move {
        format!("{}: lasting for a very short time.", args.word)
    })
}
```

Register tools conditionally and compose event handlers:

```rust
let tools = ToolSet::new()
    .with_common_tools(".")
    .with(lookup_word_tool())
    .with_if(feature_enabled, save_note_tool());

let handler = CompositeEventHandler::new()
    .with(LoggingHandler)
    .with(FnEventHandler::new(|event| {
        if let HarnessEvent::ToolResult { name, result, .. } = event {
            eprintln!("[callback] {name} → {} bytes", result.len());
        }
        None
    }));
```

## Architecture

```
┌─────────────────────────────────────────────────────┐
│                    Your Application                  │
├─────────────┬───────────────────────┬───────────────┤
│  cinch-tui  │       cinch-rs        │   cinch-web   │
│  (terminal) │    (core framework)   │   (browser)   │
│             │                       │               │
│  ratatui    │  Harness  ToolSet     │  axum + WS    │
│  crossterm  │  Context  EventHandler│  REST API     │
│             │  CostTracker          │               │
└─────────────┴───────────┬───────────┴───────────────┘
                          │
                   OpenRouter API
```

### Core Modules

| Module | Purpose |
|---|---|
| `agent` | Harness loop, config, event system, sub-agents, plan-execute, checkpointing, prompt registry |
| `tools` | `Tool` trait, `ToolSet`, `FnTool`, caching, DAG execution, filtering, common tools |
| `context` | Token budget tracking, three-zone layout, eviction, LLM summarization |
| `api` | Retry with backoff, streaming SSE, model routing strategies, cost tracking |
| `ui` | Shared `UiState`, event handler bridge, `AskUserTool`, tracing integration |

### Built-in Tools

`ToolSet::with_common_tools(workdir)` registers: `read_file`, `list_files`, `grep`, `find_files`, `shell`, `save_draft`

Additional pseudo-tools: `ThinkTool` (reasoning scratchpad), `TodoTool` (checklist management), `AskUserTool` (human-in-the-loop)

## UI Wrappers

### Terminal UI (`cinch-tui`)

A generic terminal dashboard for any cinch-rs agent. Renders agent output, tool calls, log lines, and interactive question prompts.

```rust
use cinch_tui::{TuiConfig, spawn_tui, NoTuiExtension};

let config = TuiConfig::default();
let tui_handle = spawn_tui::<NoTuiExtension>(ui_state.clone(), config);
```

### Web UI (`cinch-web`)

A browser-based chat interface with REST + WebSocket endpoints. Real-time state sync via broadcast channels.

```rust
use cinch_web::{WebConfig, spawn_web, NoWebExtension};

let config = WebConfig::default();
spawn_web::<NoWebExtension>(ui_state.clone(), config).await;
```

Both UIs support domain-specific extensions via the `TuiExtensionRenderer` and `WebExtensionRenderer` traits.

## CLI

```bash
# One-shot query
cargo run --bin cinch -- --user "Summarize this file" --model anthropic/claude-sonnet-4

# With tools and auto-execution
cargo run --bin cinch -- --user "Find TODOs" --tools tools.json --auto-execute --max-rounds 10

# With web search plugin
cargo run --bin cinch -- --user "Latest Rust news" --web-search
```

## Building and Testing

```bash
cargo build              # Debug build
cargo build --release    # Release build
cargo test               # Run all tests
cargo doc --open         # Browse API docs locally
```

## Project Structure

```
crates/
├── cinch-rs/           Core framework library
│   ├── src/
│   │   ├── lib.rs          Crate root — OpenRouterClient, Message, ChatRequest
│   │   ├── prelude.rs      Convenience re-exports
│   │   ├── main.rs         CLI binary
│   │   ├── agent/          Harness, config, events, sub-agents, plan-execute
│   │   ├── api/            Retry, streaming, routing, cost tracking
│   │   ├── context/        Budget, eviction, summarization, layout
│   │   ├── tools/          Tool trait, ToolSet, FnTool, common tools, caching
│   │   └── ui/             UiState, event handler bridge, AskUserTool
│   └── examples/
│       ├── basic_agent.rs
│       └── custom_tools.rs
├── cinch-tui/          Terminal UI (ratatui + crossterm)
└── cinch-web/          Web UI (axum + WebSocket)
```

## License

MIT — see [LICENSE](LICENSE) for details.
