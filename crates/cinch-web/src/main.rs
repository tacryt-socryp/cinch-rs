//! Interactive web chat agent — end-to-end cinch-web demo.
//!
//! Runs an agent server that accepts user messages from the browser via
//! WebSocket or REST, processes them through the LLM, and streams results
//! back in real time.
//!
//! # Usage
//!
//! ```bash
//! OPENROUTER_KEY=sk-... cargo run -p cinch-web
//! OPENROUTER_KEY=sk-... cargo run -p cinch-web -- --model google/gemini-2.5-flash
//! OPENROUTER_KEY=sk-... cargo run -p cinch-web -- --port 8080
//! OPENROUTER_KEY=sk-... cargo run -p cinch-web -- --no-web-search
//! ```
//!
//! Then open the printed URL in a browser (or use curl / wscat) to chat.
//!
//! ## Sending messages
//!
//! **WebSocket** (connect to `/ws`):
//! ```json
//! {"type": "chat", "message": "What is creatine monohydrate?"}
//! ```
//!
//! **REST** (`POST /api/chat`):
//! ```json
//! {"message": "What is creatine monohydrate?"}
//! ```

use std::sync::{Arc, Mutex};

use cinch_rs::format_citations;
use cinch_rs::prelude::*;
use cinch_web::{NoWebExtension, WebBroadcastHandler, WebConfig, WsMessage, spawn_web};
use clap::Parser;

/// Interactive web chat agent.
#[derive(Parser)]
#[command(about = "Interactive chat agent with a browser-based UI")]
struct Args {
    /// LLM model to use.
    #[arg(long, default_value = "anthropic/claude-sonnet-4")]
    model: String,

    /// Port for the web UI server.
    #[arg(long, default_value_t = 3001)]
    port: u16,

    /// Enable the OpenRouter web search plugin (server-side, in addition to
    /// the built-in web_search tool).
    #[arg(long)]
    web_search_plugin: bool,
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let args = Args::parse();

    // 1. Create the OpenRouter client.
    let api_key = std::env::var("OPENROUTER_KEY")
        .map_err(|_| "Set OPENROUTER_KEY env var to your OpenRouter API key")?;
    let client = OpenRouterClient::new(api_key)?;

    // 2. Register tools the LLM can call.
    let tools = ToolSet::new().with_common_tools(".");

    // 3. Shared UI state and WebSocket broadcast channel.
    let ui_state = Arc::new(Mutex::new(UiState::default()));
    let (ws_tx, _) = tokio::sync::broadcast::channel::<WsMessage>(256);

    // 4. Spawn the web server — returns a receiver for chat messages from the browser.
    let web_config = WebConfig {
        bind_addr: ([127, 0, 0, 1], args.port).into(),
        ..Default::default()
    };
    let (addr, mut chat_rx) = spawn_web(ui_state.clone(), ws_tx.clone(), web_config).await;
    println!("Web UI: http://{addr}");
    println!("Waiting for messages from the browser...\n");

    // 5. Compose event handlers: UI state updater + WebSocket broadcaster.
    let ext: Arc<dyn cinch_web::WebExtensionRenderer> = Arc::new(NoWebExtension);
    let handler = CompositeEventHandler::new()
        .with(UiEventHandler::new(ui_state.clone()))
        .with(WebBroadcastHandler::new(
            ws_tx.clone(),
            ext,
            ui_state.clone(),
        ));

    // 6. Chat loop — driven by messages from the web UI.
    let system_prompt = "\
You are an expert advisor integrating deep knowledge across personal training \
and fitness coaching, clinical nutrition, endocrinology and hormone optimization, \
circadian biology, and peptide therapeutics. \
Your user is a methodical, quantitative thinker who wants evidence-based optimization \
over generic advice. Lead with concise, action-oriented recommendations. When the user \
asks follow-up questions, go deeper into mechanisms, dosing rationale, relevant studies, \
and tradeoffs. Cite specific research when available. Skip disclaimers and caveats \
unless clinically critical.\n\n\
You have a web_search tool — use it to look up current information, recent \
research, or any facts you are unsure about. Cite URLs from results when \
available. Do not disclaim that you lack web access.";
    let mut conversation = vec![Message::system(system_prompt)];

    while let Some(user_message) = chat_rx.recv().await {
        println!("> {user_message}");

        // Reset UI state for the new turn (keep agent_output for chat history).
        {
            let mut s = ui_state.lock().unwrap();
            s.streaming_buffer.clear();
            s.phase = "Running".to_string();
            s.round = 0;
            s.running = true;
        }

        conversation.push(Message::user(&user_message));

        let mut config = HarnessConfig::new(&args.model, system_prompt)
            .with_max_rounds(u32::MAX)
            .with_max_tokens(4096)
            .with_streaming(true);

        // Plan-execute mode is off for interactive chat.
        config.plan_execute.enabled = false;

        if args.web_search_plugin {
            config = config.with_plugins(vec![Plugin::web()]);
        }

        let result = Harness::new(&client, &tools, config)
            .with_event_handler(&handler)
            .run(conversation.clone())
            .await?;

        // Print the response in the terminal.
        let text = result.text();
        let citations = format_citations(&result.annotations);
        println!("\n{text}{citations}");
        println!(
            "--- {} rounds | {} tokens | ${:.4} ---\n",
            result.rounds_used,
            result.total_tokens(),
            result.estimated_cost_usd
        );

        // Carry the full conversation forward for multi-turn context.
        conversation = result.messages;
    }

    Ok(())
}
