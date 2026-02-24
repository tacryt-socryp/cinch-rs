//! Terminal coding agent powered by cinch-rs.
//!
//! Provides an interactive TUI-based coding assistant with file, shell,
//! and git tools. Reads the API key from the `OPENROUTER_KEY` environment
//! variable.
//!
//! # Examples
//!
//! ```sh
//! # Interactive mode (opens TUI)
//! cinch-code --workdir /path/to/project
//!
//! # One-shot mode
//! cinch-code --prompt "Add error handling to src/main.rs"
//! ```

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cinch_code::CodeConfig;
use cinch_rs::agent::harness::Harness;
use cinch_rs::prelude::*;
use clap::Parser;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Terminal coding agent powered by cinch-rs.
#[derive(Parser)]
#[command(name = "cinch-code")]
struct Cli {
    /// Initial prompt (one-shot mode). Without this, starts interactive TUI.
    #[arg(long)]
    prompt: Option<String>,

    /// Model to use for completions.
    #[arg(long, default_value = "anthropic/claude-sonnet-4")]
    model: String,

    /// Working directory for file and git operations.
    #[arg(long, default_value = ".")]
    workdir: String,

    /// Maximum agentic round-trips.
    #[arg(long, default_value_t = 50)]
    max_rounds: u32,

    /// Maximum tokens per LLM response.
    #[arg(long, default_value_t = 16384)]
    max_tokens: u32,

    /// Sampling temperature.
    #[arg(long, default_value_t = 0.3)]
    temperature: f32,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Resolve working directory to absolute path.
    let workdir = std::fs::canonicalize(&cli.workdir)
        .unwrap_or_else(|_| PathBuf::from(&cli.workdir))
        .to_string_lossy()
        .to_string();

    // Build config from CLI args.
    let config = CodeConfig {
        model: cli.model,
        max_rounds: cli.max_rounds,
        max_tokens: cli.max_tokens,
        temperature: cli.temperature,
        workdir: workdir.clone(),
        streaming: true,
    };

    let tools = config.build_tool_set();
    let harness_config = config.build_harness_config();

    // API client.
    let api_key = match std::env::var("OPENROUTER_KEY") {
        Ok(key) => key,
        Err(_) => {
            eprintln!("Error: OPENROUTER_KEY environment variable is not set");
            std::process::exit(1);
        }
    };

    let client = match OpenRouterClient::with_headers(
        api_key,
        "https://crates.io/crates/cinch-code",
        "cinch-code",
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: failed to create API client: {e}");
            std::process::exit(1);
        }
    };

    // UI state shared between harness event handler and TUI.
    let ui_state = Arc::new(Mutex::new(UiState::default()));

    // Set up tracing → TUI log buffer.
    let (tracing_layer, log_buffer) = UiTracingLayer::new();
    tracing_subscriber::registry().with(tracing_layer).init();

    // Spawn TUI on a dedicated thread.
    let tui_config = cinch_tui::TuiConfig {
        workdir: PathBuf::from(&workdir),
        log_buffer: Some(log_buffer),
        ..Default::default()
    };
    let tui_handle = cinch_tui::spawn_tui(ui_state.clone(), tui_config);

    // Build messages.
    let prompt_text = match &cli.prompt {
        Some(p) => p.clone(),
        None => {
            // Interactive mode: use a default prompt.
            // In a full implementation this would read from TUI input,
            // but for now we require --prompt.
            eprintln!("Error: --prompt is required (interactive input not yet implemented)");
            ui_state.lock().unwrap().quit_requested = true;
            tui_handle.join().ok();
            std::process::exit(1);
        }
    };

    let messages = vec![
        Message::system(cinch_code::coding_system_prompt()),
        Message::user(&prompt_text),
    ];

    // Event handler: UI state updater.
    let ui_handler = UiEventHandler::new(ui_state.clone());

    // Run the agent.
    let result = Harness::new(&client, &tools, harness_config)
        .with_event_handler(&ui_handler)
        .run(messages)
        .await;

    // Mark agent as finished.
    {
        let mut s = ui_state.lock().unwrap();
        s.running = false;
    }

    match result {
        Ok(r) => {
            let text = r.text();
            if !text.is_empty() {
                push_agent_text(&ui_state, &format!("\n--- Final output ---\n{text}"));
            }
        }
        Err(e) => {
            push_agent_text(&ui_state, &format!("\nError: {e}"));
        }
    }

    // Wait for TUI to exit.
    tui_handle.join().ok();
}
