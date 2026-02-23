//! Minimal agent example — 30 lines from zero to working agent.
//!
//! Registers the built-in file and shell tools, sends a user prompt, and
//! prints the LLM's response along with token usage and cost.
//!
//! # Usage
//!
//! ```bash
//! OPENROUTER_KEY=sk-... cargo run --example basic_agent
//! ```

use cinch_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), String> {
    // 1. Create the OpenRouter client.
    let api_key = std::env::var("OPENROUTER_KEY")
        .map_err(|_| "Set OPENROUTER_KEY env var to your OpenRouter API key")?;
    let client = OpenRouterClient::new(api_key)?;

    // 2. Register tools the LLM can call.
    let tools = ToolSet::new().with_common_tools(".");

    // 3. Configure the harness (all advanced modules default to enabled).
    let config = HarnessConfig::new(
        "anthropic/claude-sonnet-4",
        "You are a helpful coding assistant. Be concise.",
    )
    .with_max_rounds(10)
    .with_max_tokens(4096);

    // 4. Run the agentic loop.
    let messages = vec![
        Message::system("You are a helpful coding assistant. Be concise."),
        Message::user(
            "List the files in the current directory and summarize what this project does.",
        ),
    ];

    let result = Harness::new(&client, &tools, config)
        .with_event_handler(&LoggingHandler)
        .run(messages)
        .await?;

    // 5. Print results.
    println!("\n{}", result.text());
    println!(
        "\n--- {} rounds | {} tokens | ${:.4} ---",
        result.rounds_used,
        result.total_tokens(),
        result.estimated_cost_usd
    );

    Ok(())
}
