# cinch-rs

Opinionated Rust agent framework for building LLM-powered tool-use agents.

Provides a complete, model-agnostic agent runtime on top of the [OpenRouter](https://openrouter.ai/) chat completions API. At its core is the **Harness** — a reusable agentic loop that sends messages to an LLM, executes tool calls, appends results, and repeats until the model produces a text-only response or a round limit is reached.

All advanced modules (context eviction, summarization, checkpointing, tool caching, plan-execute workflows) are **enabled by default** with sensible defaults.

## Prerequisites

- Rust 1.93+ (edition 2024)
- Environment variable `OPENROUTER_KEY`

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
cinch-rs = { path = "../cinch-rs" }
```

Then build and run an agent:

```rust
use cinch_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), String> {
    let client = OpenRouterClient::new(std::env::var("OPENROUTER_KEY").unwrap())?;

    let tools = ToolSet::new().with_common_tools("/path/to/workdir");

    let config = HarnessConfig::new("anthropic/claude-sonnet-4", "You are a coding assistant.")
        .with_max_rounds(20)
        .with_max_tokens(4096);

    let messages = vec![
        Message::system("You are a coding assistant."),
        Message::user("Read src/main.rs and summarize it."),
    ];

    let result = Harness::new(&client, &tools, config)
        .with_event_handler(&LoggingHandler)
        .run(messages)
        .await?;

    println!("{}", result.text());
    println!("Cost: ${:.4}", result.estimated_cost_usd);
    Ok(())
}
```

## Modules

| Module | Description |
|--------|-------------|
| `agent` | `Harness` agentic loop, `HarnessConfig`, `EventHandler`, checkpointing, sub-agents, profiles, plan-execute |
| `tools` | `Tool` trait, `ToolSet`, `FnTool`, caching, filtering, DAG execution, common file/shell tools |
| `context` | `ContextBudget`, three-zone message layout, tool result eviction, LLM-based summarization |
| `api` | Model routing, SSE streaming, retry with exponential backoff, cost tracking |
| `ui` | Generic UI state (`UiState`), `UiEventHandler`, `AskUserTool`, question/response types, tracing layer |

### Key Exports

Import everything via `use cinch_rs::prelude::*`, or pick individual items:

- **Core types:** `OpenRouterClient`, `ChatRequest`, `ChatCompletion`, `Message`, `ToolDef`, `Plugin`
- **Agent runtime:** `Harness`, `HarnessConfig`, `HarnessResult`
- **Event handling:** `EventHandler`, `HarnessEvent`, `LoggingHandler`, `CompositeEventHandler`, `FnEventHandler`, `ToolResultHandler`
- **Tools:** `Tool`, `ToolSet`, `FnTool`, `DisabledTool`, `ToolSpec`, `ToolFilter`, `parse_tool_args`
- **Context:** `ContextBudget`
- **UI:** `UiState`, `UiEventHandler`, `AskUserTool`, `UserQuestion`, `QuestionChoice`, `QuestionResponse`, `UiTracingLayer`, `UiExtension`
- **Utilities:** `json_schema_for::<T>()`, `quick_completion()`, `format_citations()`, `SystemPromptBuilder`

## Documentation

| Document | Description |
|----------|-------------|
| [docs/VISION.md](docs/VISION.md) | Architecture, design principles, module inventory, implementation status |
| [docs/DATA-FLOWS.md](docs/DATA-FLOWS.md) | Comprehensive data flow diagrams for every module |
| [API_IMPROVEMENTS.md](API_IMPROVEMENTS.md) | Ranked improvement proposals with before/after code |
| [DEEPER_INTEGRATION.md](DEEPER_INTEGRATION.md) | Phase-based integration guide for consumers |
| **Rustdoc** (`cargo doc --open`) | Full API reference with examples on key types |

## CLI Usage

The `cinch` binary provides a standalone agentic loop from the command line.

```bash
cargo run --bin cinch -- --help

# Simple one-shot query
cargo run --bin cinch -- --user "Summarize this file" --model anthropic/claude-sonnet-4

# With tools from a TOML file
cargo run --bin cinch -- --user "Find TODOs in the codebase" --tools tools.toml
```

## Source Layout

```
src/
├── lib.rs                 Crate root: core types, OpenRouterClient, Message, Plugin
├── prelude.rs             Glob-importable re-exports
├── main.rs                CLI entry point (cinch binary)
├── agent/
│   ├── harness.rs         Core agentic tool-use loop
│   ├── config.rs          HarnessConfig and module toggles
│   ├── events.rs          HarnessEvent, EventHandler, LoggingHandler, FnEventHandler
│   ├── execution.rs       Tool call execution and recording
│   ├── checkpoint.rs      Conversation checkpointing and resume
│   ├── sub_agent.rs       Sub-agent delegation with token budget semaphore
│   ├── plan_execute.rs    Two-phase plan-then-execute workflow
│   ├── profile.rs         Persistent agent identity and per-tool stats
│   ├── memory.rs          Cross-session file-based memory
│   └── prompt.rs          SystemPromptBuilder for multi-section prompts
├── api/
│   ├── retry.rs           Transient error detection + exponential backoff
│   ├── streaming.rs       SSE streaming parser (text/reasoning/tool deltas)
│   ├── router.rs          Per-round model routing strategies
│   └── tracing.rs         Trace IDs, per-model pricing, CostTracker
├── context/
│   ├── budget.rs          Token budget tracking with threshold advisories
│   ├── eviction.rs        Tool result eviction (oldest-first placeholders)
│   ├── layout.rs          Three-zone context management (pinned/compressed/recent)
│   └── summarizer.rs      LLM-based incremental summarization
├── ui/
│   ├── mod.rs             UiState, AgentEntry, LogLine, convenience updaters
│   ├── traits.rs          UiExtension trait, NoExtension
│   ├── question.rs        UserQuestion, QuestionChoice, QuestionResponse, ActiveQuestion
│   ├── tracing.rs         UiTracingLayer (generic tracing_subscriber::Layer)
│   ├── event_handler.rs   UiEventHandler — generic EventHandler → UiState bridge
│   └── ask_user_tool.rs   AskUserTool — LLM-callable human-in-the-loop tool
└── tools/
    ├── core.rs            Tool trait, ToolSet, FnTool, DisabledTool, ThinkTool, TodoTool
    ├── common.rs          Built-in tools: ReadFile, ListFiles, Grep, FindFiles, Shell
    ├── spec.rs            ToolSpec builder (when_to_use / when_not_to_use)
    ├── cache.rs           Tool result caching with FNV-1a hashing
    ├── filter.rs          Dynamic tool filtering by category/keywords/usage
    ├── dag.rs             Dependency-aware parallel execution
    └── reflection.rs      Structured error formatting for LLM self-correction
```

## Testing

```bash
cargo test
```
