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
use cinch_rs::agent::session::SessionManager;
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
    #[arg(long, default_value = "minimax/minimax-m2.5")]
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

    /// Resume a previous session. Pass a session/trace ID, or "latest" to
    /// resume the most recently updated session.
    #[arg(long)]
    resume: Option<String>,
}

/// Detect the git repository root for the current directory.
fn detect_git_root() -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Load messages from a saved session checkpoint.
///
/// Pass a trace ID directly, or `"latest"` to resolve the most recently
/// updated session.
fn load_session_messages(
    sessions_dir: &std::path::Path,
    resume_id: &str,
) -> Result<Vec<Message>, String> {
    let mgr = SessionManager::new(sessions_dir)
        .map_err(|e| format!("cannot open sessions dir: {e}"))?;

    let trace_id = if resume_id.eq_ignore_ascii_case("latest") {
        let mut sessions = mgr.list_sessions()?;
        if sessions.is_empty() {
            return Err("no sessions found".to_string());
        }
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        sessions[0].trace_id.clone()
    } else {
        resume_id.to_string()
    };

    let checkpoint = mgr
        .load_latest_checkpoint(&trace_id)?
        .ok_or_else(|| format!("no checkpoint found for session {trace_id}"))?;

    Ok(checkpoint.messages)
}

/// Ask the user for free-text input via the TUI question system.
async fn get_user_input(ui_state: &Arc<Mutex<UiState>>) -> Option<String> {
    let question = UserQuestion {
        prompt: "Enter your message".to_string(),
        choices: vec![],
        editable: false,
        max_edit_length: None,
    };
    ask_question(ui_state, question, 86400);

    loop {
        if ui_state.lock().unwrap().quit_requested {
            return None;
        }
        if let Some(response) = poll_question(ui_state) {
            return match response {
                QuestionResponse::FreeText(text) => Some(text),
                QuestionResponse::Skipped | QuestionResponse::TimedOut => None,
                _ => None,
            };
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Resolve working directory: git root > canonicalize > fallback.
    let workdir = if cli.workdir == "." {
        detect_git_root()
            .or_else(|| {
                std::fs::canonicalize(".")
                    .ok()
                    .map(|p| p.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| ".".to_string())
    } else {
        std::fs::canonicalize(&cli.workdir)
            .unwrap_or_else(|_| PathBuf::from(&cli.workdir))
            .to_string_lossy()
            .to_string()
    };

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

    // Event handler: UI state updater.
    let ui_handler = UiEventHandler::new(ui_state.clone());

    // Conversation loop — optionally resume from a previous session.
    let mut messages = if let Some(ref resume_id) = cli.resume {
        match load_session_messages(&harness_config.session.sessions_dir, resume_id) {
            Ok(msgs) => {
                push_agent_text(
                    &ui_state,
                    &format!("Resumed session ({} messages from checkpoint)", msgs.len()),
                );
                msgs
            }
            Err(e) => {
                push_agent_text(&ui_state, &format!("Failed to resume session: {e}"));
                ui_state.lock().unwrap().running = false;
                tui_handle.join().ok();
                return;
            }
        }
    } else {
        vec![Message::system(cinch_code::coding_system_prompt())]
    };

    // First turn: when resuming, ask for user input first; otherwise use
    // --prompt or interactive input.
    {
        let first_prompt = if cli.resume.is_some() {
            // Resuming — get a new user message to continue the conversation.
            match get_user_input(&ui_state).await {
                Some(text) => text,
                None => {
                    ui_state.lock().unwrap().quit_requested = true;
                    tui_handle.join().ok();
                    return;
                }
            }
        } else if let Some(p) = cli.prompt {
            p
        } else {
            match get_user_input(&ui_state).await {
                Some(text) => text,
                None => {
                    ui_state.lock().unwrap().quit_requested = true;
                    tui_handle.join().ok();
                    return;
                }
            }
        };

        push_user_message(&ui_state, &first_prompt);
        messages.push(Message::user(&first_prompt));
    }

    let ui_state_stop = ui_state.clone();

    loop {
        // Check if quit was requested.
        if ui_state.lock().unwrap().quit_requested {
            break;
        }

        // Run the agent harness for this turn, retrying on transient API errors.
        const MAX_RETRIES: u32 = 3;
        let mut attempt = 0;
        let turn_ok = loop {
            attempt += 1;
            let result = Harness::new(&client, &tools, harness_config.clone())
                .with_event_handler(&ui_handler)
                .with_stop_signal(|| ui_state_stop.lock().unwrap().quit_requested)
                .run(messages.clone())
                .await;

            match result {
                Ok(r) => {
                    let text = r.text();
                    messages = r.messages;
                    if !text.is_empty() {
                        push_agent_text(&ui_state, &text);
                    }
                    break true;
                }
                Err(e) => {
                    if attempt >= MAX_RETRIES {
                        push_agent_text(
                            &ui_state,
                            &format!("Error (attempt {attempt}/{MAX_RETRIES}): {e}"),
                        );
                        break false;
                    }
                    push_agent_text(
                        &ui_state,
                        &format!(
                            "Error (attempt {attempt}/{MAX_RETRIES}): {e} — retrying in 2s..."
                        ),
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    if ui_state.lock().unwrap().quit_requested {
                        break false;
                    }
                }
            }
        };
        // On persistent failure, still continue to the input prompt so the
        // user can retry or adjust their request.
        let _ = turn_ok;

        // Check quit again after harness completes.
        if ui_state.lock().unwrap().quit_requested {
            break;
        }

        // Get next user input.
        match get_user_input(&ui_state).await {
            Some(text) => {
                push_user_message(&ui_state, &text);
                messages.push(Message::user(&text));
            }
            None => break,
        }
    }

    // Mark agent as finished and wait for TUI to exit.
    ui_state.lock().unwrap().running = false;
    tui_handle.join().ok();
}
