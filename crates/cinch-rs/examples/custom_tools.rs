//! Custom tools example — define domain-specific tools with `FnTool` and `ToolSpec`.
//!
//! Demonstrates:
//! - Typed argument structs with `Deserialize` + `JsonSchema`
//! - Rich tool descriptions via `ToolSpec::builder()`
//! - Conditional tool registration with `with_if` / `DisabledTool::from_tool`
//! - Event handling with `CompositeEventHandler`
//!
//! # Usage
//!
//! ```bash
//! OPENROUTER_KEY=sk-... cargo run --example custom_tools
//! ```

use cinch_rs::prelude::*;
use cinch_rs::schemars;
use schemars::JsonSchema;
use serde::Deserialize;

// ── Typed argument structs ──────────────────────────────────────────

/// Arguments for the `lookup_word` tool.
#[derive(Deserialize, JsonSchema)]
struct LookupWordArgs {
    /// The word to look up.
    word: String,
}

/// Arguments for the `save_note` tool.
#[derive(Deserialize, JsonSchema)]
struct SaveNoteArgs {
    /// Title for the note.
    title: String,
    /// Note content (markdown).
    content: String,
}

// ── Tool constructors ───────────────────────────────────────────────

/// A read-only tool that "looks up" a word (stub implementation).
fn lookup_word_tool() -> FnTool {
    let def = ToolSpec::builder("lookup_word")
        .purpose("Look up the definition of a word")
        .when_to_use("When the user asks about a word's meaning or etymology")
        .when_not_to_use("When the user wants a translation — use translate instead")
        .parameters_for::<LookupWordArgs>()
        .example(
            "lookup_word(word='ephemeral')",
            "ephemeral: lasting for a very short time.",
        )
        .output_format("Plain text definition")
        .build()
        .to_tool_def();

    FnTool::new(def, |args: LookupWordArgs| async move {
        // In a real tool, you'd call an API here.
        format!(
            "{}: [stub] this is where the definition would go.",
            args.word
        )
    })
}

/// A mutation tool that "saves" a note (stub implementation).
fn save_note_tool() -> FnTool {
    let def = ToolSpec::builder("save_note")
        .purpose("Save a markdown note to the user's notebook")
        .when_to_use("When the user asks to save, remember, or write down something")
        .when_not_to_use("When the user is just asking a question — answer directly instead")
        .parameters_for::<SaveNoteArgs>()
        .build()
        .to_tool_def();

    FnTool::new(def, |args: SaveNoteArgs| async move {
        // In a real tool, you'd write to disk or a database.
        format!("Saved note '{}' ({} bytes)", args.title, args.content.len())
    })
    .mutation(true)
}

// ── Main ────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), String> {
    let api_key = std::env::var("OPENROUTER_KEY")
        .map_err(|_| "Set OPENROUTER_KEY env var to your OpenRouter API key")?;
    let client = OpenRouterClient::new(api_key)?;

    // Simulate a feature flag.
    let notes_enabled = true;

    // Build the tool set with conditional registration.
    let save_note = save_note_tool();
    let tools = ToolSet::new()
        .with_common_tools(".")
        .with(lookup_word_tool())
        .with_if(notes_enabled, save_note);

    // Compose event handlers: logging + a custom closure handler.
    let handler = CompositeEventHandler::new()
        .with(LoggingHandler)
        .with(FnEventHandler::new(|event| {
            if let HarnessEvent::ToolResult { name, result, .. } = event {
                eprintln!("[callback] {name} → {} bytes", result.len());
            }
            None
        }));

    let config = HarnessConfig::new(
        "anthropic/claude-sonnet-4",
        "You are a helpful assistant with access to a word lookup tool and a notebook.",
    )
    .with_max_rounds(10)
    .with_max_tokens(4096);

    let messages = vec![
        Message::system(
            "You are a helpful assistant with access to a word lookup tool and a notebook.",
        ),
        Message::user("Look up the word 'ephemeral' and save a note about it."),
    ];

    let result = Harness::new(&client, &tools, config)
        .with_event_handler(&handler)
        .run(messages)
        .await?;

    println!("\n{}", result.text());
    println!(
        "\n--- {} rounds | {} tokens | ${:.4} ---",
        result.rounds_used,
        result.total_tokens(),
        result.estimated_cost_usd
    );

    Ok(())
}
