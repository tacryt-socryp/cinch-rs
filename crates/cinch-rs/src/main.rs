//! Send a chat-completion request to OpenRouter and print the response.
//!
//! Reads the API key from the `OPENROUTER_KEY` environment variable.
//!
//! # Examples
//!
//! ```sh
//! # Basic request
//! openrouter --user "Summarize this tweet thread"
//!
//! # With system prompt and model selection
//! openrouter --system "You are a sharp tweet analyst." \
//!   --user "Rate this tweet for engagement potential." \
//!   --model anthropic/claude-sonnet-4
//!
//! # Pipe content from stdin
//! cat draft.md | openrouter --system "Analyze these drafts." --stdin
//!
//! # Tool use with auto-execution
//! openrouter --user "Check price and draft tweets." \
//!   --tools tools.json --auto-execute --max-rounds 10
//!
//! # Enable web search plugin
//! openrouter --user "Latest news" --web-search
//! ```

use cinch_rs::{
    ChatRequest, Message, OpenRouterClient, ProviderPreferences, ResponseFormat, ToolDef,
    format_citations,
};
use clap::Parser;
use serde::Deserialize;
use std::io::{self, Read};
use std::process;

/// Send a chat-completion request to OpenRouter and print the response.
///
/// Reads the API key from the OPENROUTER_KEY environment variable.
#[derive(Parser)]
#[command(name = "openrouter")]
struct Cli {
    // ── Message content ────────────────────────────────────────
    /// System prompt to set the assistant's behavior
    #[arg(long)]
    system: Option<String>,

    /// User message to send
    #[arg(long)]
    user: Option<String>,

    /// Read user content from stdin
    #[arg(long)]
    stdin: bool,

    /// Assistant prefill: partial text the model must continue from
    #[arg(long)]
    prefill: Option<String>,

    /// Do NOT prepend the prefill text to the output
    #[arg(long)]
    no_prefill_echo: bool,

    // ── Model selection ────────────────────────────────────────
    /// Primary model to use
    #[arg(long, default_value = "anthropic/claude-sonnet-4")]
    model: String,

    /// Fallback models tried in order if the primary is unavailable
    #[arg(long = "fallback-model")]
    fallback_models: Vec<String>,

    // ── Sampling parameters ────────────────────────────────────
    /// Sampling temperature (0.0 = deterministic, 2.0 = very creative)
    #[arg(long, default_value_t = 0.7)]
    temperature: f32,

    /// Nucleus sampling threshold (0.0 – 1.0)
    #[arg(long)]
    top_p: Option<f32>,

    /// Top-k sampling
    #[arg(long)]
    top_k: Option<u32>,

    /// Frequency penalty (-2.0 – 2.0)
    #[arg(long)]
    frequency_penalty: Option<f32>,

    /// Presence penalty (-2.0 – 2.0)
    #[arg(long)]
    presence_penalty: Option<f32>,

    /// Repetition penalty multiplier (1.0 = no penalty)
    #[arg(long)]
    repetition_penalty: Option<f32>,

    /// Minimum probability relative to most likely token (0.0 – 1.0)
    #[arg(long)]
    min_p: Option<f32>,

    /// Top-a sampling
    #[arg(long)]
    top_a: Option<f32>,

    // ── Output control ─────────────────────────────────────────
    /// Maximum tokens in the response
    #[arg(long, default_value_t = 1024)]
    max_tokens: u32,

    /// Stop sequence(s)
    #[arg(long)]
    stop: Vec<String>,

    /// Request JSON-formatted output
    #[arg(long)]
    json: bool,

    /// Seed for deterministic sampling
    #[arg(long)]
    seed: Option<u64>,

    // ── Provider / routing ─────────────────────────────────────
    /// Preferred provider(s) in priority order
    #[arg(long)]
    provider: Vec<String>,

    /// Allow fallback to other providers
    #[arg(long)]
    allow_fallbacks: Option<bool>,

    /// Routing strategy
    #[arg(long)]
    route: Option<String>,

    /// Prompt transforms to apply
    #[arg(long)]
    transform: Vec<String>,

    // ── Tool use ───────────────────────────────────────────────
    /// Path to a JSON file defining available tools
    #[arg(long)]
    tools: Option<String>,

    /// Auto-execute tool calls and loop until text response
    #[arg(long)]
    auto_execute: bool,

    /// Maximum tool-use round-trips (default 10)
    #[arg(long, default_value_t = 10)]
    max_rounds: u32,

    // ── Plugins ────────────────────────────────────────────────
    /// Enable a plugin by ID
    #[arg(long)]
    plugin: Vec<String>,

    /// Path to a JSON file with full plugin configurations
    #[arg(long)]
    plugin_file: Option<String>,

    /// Enable the web-search plugin
    #[arg(long)]
    web_search: bool,

    /// Number of web search results (with --web-search)
    #[arg(long)]
    web_max_results: Option<u32>,

    /// Custom prompt for web search results (with --web-search)
    #[arg(long)]
    web_search_prompt: Option<String>,

    /// Enable the response-healing plugin
    #[arg(long)]
    response_healing: bool,

    /// PDF file-parser engine
    #[arg(long)]
    pdf_engine: Option<String>,

    // ── Output mode ────────────────────────────────────────────
    /// Print the full API JSON response
    #[arg(long)]
    raw: bool,
}

// ── Tool file types ────────────────────────────────────────────────

/// Extended tool definition loaded from the tools JSON file, including
/// an "execute" command template for local subprocess execution.
#[derive(Deserialize)]
struct ToolFileEntry {
    #[serde(rename = "type")]
    tool_type: cinch_rs::ToolType,
    function: cinch_rs::FunctionDef,
    /// Shell command template. Use {{param_name}} for argument substitution.
    execute: String,
}

/// A tool loaded from a JSON tools file that executes via shell command.
///
/// This implements the [`Tool`](cinch_rs::tools::core::Tool) trait from the openrouter crate, bridging
/// the JSON-defined tools into the harness abstraction.
struct ShellCommandTool {
    def: ToolDef,
    template: String,
}

impl cinch_rs::tools::core::Tool for ShellCommandTool {
    fn definition(&self) -> ToolDef {
        self.def.clone()
    }

    fn execute(&self, arguments: &str) -> cinch_rs::tools::core::ToolFuture<'_> {
        let template = self.template.clone();
        let name = self.def.function.name.clone();
        let arguments = arguments.to_string();
        Box::pin(async move {
            let cmd = match render_command(&template, &arguments) {
                Ok(c) => c,
                Err(e) => return format!("Error rendering command: {e}"),
            };

            eprintln!("  [tool] {name}: {cmd}");

            let output = match std::process::Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .output()
            {
                Ok(o) => o,
                Err(e) => return format!("Error executing tool '{name}': {e}"),
            };

            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            if !output.status.success() {
                format!(
                    "Tool '{name}' exited with {}.\nstdout:\n{stdout}\nstderr:\n{stderr}",
                    output.status
                )
            } else if stderr.is_empty() {
                stdout
            } else {
                format!("{stdout}\n\n[stderr]\n{stderr}")
            }
        })
    }
}

// ── Helpers ────────────────────────────────────────────────────────

fn read_stdin_content() -> Result<String, String> {
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| format!("failed to read stdin: {e}"))?;
    Ok(buf)
}

fn build_user_content(cli: &Cli) -> Result<String, String> {
    let stdin_text = if cli.stdin {
        Some(read_stdin_content()?)
    } else {
        None
    };

    match (&cli.user, stdin_text) {
        (Some(msg), Some(piped)) => Ok(format!("{msg}\n\n{piped}")),
        (Some(msg), None) => Ok(msg.clone()),
        (None, Some(piped)) => Ok(piped),
        (None, None) => Err("provide --user, --stdin, or both".to_string()),
    }
}

/// Load tools from a JSON file into a [`ToolSet`](cinch_rs::tools::core::ToolSet).
fn load_tools(path: &str) -> Result<cinch_rs::tools::core::ToolSet, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read tools file '{path}': {e}"))?;
    let entries: Vec<ToolFileEntry> = serde_json::from_str(&content)
        .map_err(|e| format!("failed to parse tools file '{path}': {e}"))?;

    let mut set = cinch_rs::tools::core::ToolSet::new();
    for entry in entries {
        set.register(ShellCommandTool {
            def: ToolDef {
                tool_type: entry.tool_type,
                function: entry.function,
            },
            template: entry.execute,
        });
    }
    Ok(set)
}

/// Substitute {{param}} placeholders in a command template.
fn render_command(template: &str, arguments_json: &str) -> Result<String, String> {
    let args: serde_json::Value = serde_json::from_str(arguments_json)
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

    let mut cmd = template.to_string();

    if let Some(obj) = args.as_object() {
        for (key, value) in obj {
            let placeholder = format!("{{{{{key}}}}}");
            let replacement = match value {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            };
            cmd = cmd.replace(&placeholder, &replacement);
        }
    }

    // Remove unsubstituted placeholders.
    let mut cleaned = String::new();
    let chars: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    let mut last = 0;
    while i < chars.len() {
        if i + 1 < chars.len()
            && chars[i] == '{'
            && chars[i + 1] == '{'
            && let Some(end) = cmd[i + 2..].find("}}")
        {
            cleaned.push_str(&cmd[last..i]);
            last = i + 2 + end + 2;
            i = last;
            continue;
        }
        i += 1;
    }
    cleaned.push_str(&cmd[last..]);
    cmd = cleaned;

    while cmd.contains("  ") {
        cmd = cmd.replace("  ", " ");
    }

    Ok(cmd.trim().to_string())
}

/// Build the plugins array from CLI flags and optional plugin file.
fn build_plugins(cli: &Cli) -> Option<Vec<cinch_rs::Plugin>> {
    use cinch_rs::PluginVecExt;

    let mut plugins: Vec<cinch_rs::Plugin> = Vec::new();

    if let Some(ref path) = cli.plugin_file {
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Ok(file_plugins) = serde_json::from_str::<Vec<cinch_rs::Plugin>>(&content) {
                plugins.extend(file_plugins);
            } else {
                eprintln!("  Warning: failed to parse plugin file '{path}'");
            }
        } else {
            eprintln!("  Warning: failed to read plugin file '{path}'");
        }
    }

    for id in &cli.plugin {
        match id.as_str() {
            "web" => plugins.push_if_absent(cinch_rs::Plugin::web()),
            "response-healing" => plugins.push_if_absent(cinch_rs::Plugin::response_healing()),
            _ => {
                eprintln!(
                    "  Warning: unknown plugin ID '{id}', use --plugin-file for custom plugins"
                );
            }
        }
    }

    if cli.web_search {
        plugins.upsert(cinch_rs::Plugin::web_with(
            cli.web_max_results,
            cli.web_search_prompt.clone(),
        ));
    }

    if cli.response_healing {
        plugins.push_if_absent(cinch_rs::Plugin::response_healing());
    }

    if let Some(ref engine) = cli.pdf_engine {
        plugins.upsert(cinch_rs::Plugin::file_parser(engine));
    }

    if plugins.is_empty() {
        None
    } else {
        Some(plugins)
    }
}

/// Build a ChatRequest from CLI flags.
fn build_request_body(
    cli: &Cli,
    messages: Vec<Message>,
    tools: Option<Vec<ToolDef>>,
) -> ChatRequest {
    let (model, models, route) = if cli.fallback_models.is_empty() {
        (Some(cli.model.clone()), None, None)
    } else {
        let mut all = vec![cli.model.clone()];
        all.extend(cli.fallback_models.clone());
        (None, Some(all), Some("fallback".to_string()))
    };

    let provider = if !cli.provider.is_empty() || cli.allow_fallbacks.is_some() {
        Some(ProviderPreferences {
            order: if cli.provider.is_empty() {
                None
            } else {
                Some(cli.provider.clone())
            },
            allow_fallbacks: cli.allow_fallbacks,
        })
    } else {
        None
    };

    let response_format = if cli.json {
        Some(ResponseFormat {
            fmt_type: cinch_rs::ResponseFormatType::JsonObject,
        })
    } else {
        None
    };

    let stop = if cli.stop.is_empty() {
        None
    } else {
        Some(cli.stop.clone())
    };

    let transforms = if cli.transform.is_empty() {
        None
    } else {
        Some(cli.transform.clone())
    };

    let plugins = build_plugins(cli);

    ChatRequest {
        model,
        models,
        route: route.or_else(|| cli.route.clone()),
        messages,
        max_tokens: cli.max_tokens,
        temperature: cli.temperature,
        top_p: cli.top_p,
        top_k: cli.top_k,
        frequency_penalty: cli.frequency_penalty,
        presence_penalty: cli.presence_penalty,
        repetition_penalty: cli.repetition_penalty,
        min_p: cli.min_p,
        top_a: cli.top_a,
        seed: cli.seed,
        stop,
        response_format,
        provider,
        transforms,
        tools,
        plugins,
        reasoning: None,
    }
}

/// Event handler for the CLI that prints round info to stderr.
struct CliEventHandler;

impl cinch_rs::agent::events::EventHandler for CliEventHandler {
    fn on_event(
        &self,
        event: &cinch_rs::agent::events::HarnessEvent<'_>,
    ) -> Option<cinch_rs::agent::events::EventResponse> {
        match event {
            cinch_rs::agent::events::HarnessEvent::RoundStart {
                round, max_rounds, ..
            } => {
                eprintln!("  [round {round}/{max_rounds}]");
            }
            cinch_rs::agent::events::HarnessEvent::ToolExecuting {
                name, arguments, ..
            } => {
                eprintln!("  [tool_call] {name}({arguments})");
            }
            _ => {}
        }
        None
    }
}

async fn send_request(cli: &Cli) -> Result<String, String> {
    let api_key = std::env::var("OPENROUTER_KEY")
        .map_err(|_| "OPENROUTER_KEY environment variable is not set".to_string())?;

    let user_content = build_user_content(cli)?;

    let tool_set = if let Some(tools_path) = &cli.tools {
        let set = load_tools(tools_path)?;
        eprintln!("  Loaded {} tool(s) from {tools_path}", set.len());
        Some(set)
    } else {
        None
    };

    let mut messages = Vec::new();
    if let Some(sys) = &cli.system {
        messages.push(Message::system(sys));
    }
    messages.push(Message::user(&user_content));

    if let Some(prefill) = &cli.prefill {
        messages.push(Message::assistant_text(prefill));
    }

    let client =
        OpenRouterClient::with_headers(api_key, "https://crates.io/crates/cinch-rs", "cinch-rs")?;

    // ── Single-shot mode ────────────────────────────────────────
    if tool_set.is_none() || !cli.auto_execute {
        let tool_defs = tool_set.as_ref().map(|s| s.definitions());
        let body = build_request_body(cli, messages, tool_defs);

        if cli.raw {
            // Raw mode: re-send via reqwest to get the unprocessed JSON.
            let raw_client = reqwest::Client::builder()
                .user_agent("openrouter-client/0.1")
                .build()
                .map_err(|e| format!("failed to build HTTP client: {e}"))?;
            let resp = raw_client
                .post(cinch_rs::OPENROUTER_URL)
                .header(
                    "Authorization",
                    format!("Bearer {}", std::env::var("OPENROUTER_KEY").unwrap()),
                )
                .header("HTTP-Referer", "https://crates.io/crates/cinch-rs")
                .header("X-Title", "cinch-rs")
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("request failed: {e}"))?;
            let text = resp
                .text()
                .await
                .map_err(|e| format!("failed to read response body: {e}"))?;
            let val: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| format!("failed to parse response: {e}"))?;
            return serde_json::to_string_pretty(&val)
                .map_err(|e| format!("failed to format response: {e}"));
        }

        let completion = client.chat(&body).await?;

        if !completion.tool_calls.is_empty() {
            let json = serde_json::to_string_pretty(&completion.tool_calls)
                .map_err(|e| format!("failed to serialize tool calls: {e}"))?;
            return Ok(format!("[tool_calls]\n{json}"));
        }

        let continuation = completion.content.unwrap_or_default();
        let citations = format_citations(&completion.annotations);
        let result = match (&cli.prefill, cli.no_prefill_echo) {
            (Some(prefill), false) => format!("{prefill}{continuation}{citations}"),
            _ => format!("{continuation}{citations}"),
        };
        return Ok(result);
    }

    // ── Agentic tool-use loop (via Harness) ─────────────────────
    let tool_set = tool_set.unwrap();
    let plugins = build_plugins(cli);

    let harness_config = cinch_rs::agent::config::HarnessConfig {
        model: cli.model.clone(),
        max_rounds: cli.max_rounds,
        max_tokens: cli.max_tokens,
        temperature: cli.temperature,
        plugins,
        reasoning: None,
        retry: cinch_rs::api::retry::RetryConfig::default(),
        ..Default::default()
    };

    let handler = CliEventHandler;
    let result = cinch_rs::agent::harness::Harness::new(&client, &tool_set, harness_config)
        .with_event_handler(&handler)
        .run(messages)
        .await?;

    let output = result.text();
    let citations = format_citations(&result.annotations);

    let final_result = match (&cli.prefill, cli.no_prefill_echo) {
        (Some(prefill), false) if !output.is_empty() => {
            format!("{prefill}{output}{citations}")
        }
        _ => format!("{output}{citations}"),
    };

    Ok(final_result)
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match send_request(&cli).await {
        Ok(response) => print!("{response}"),
        Err(e) => {
            eprintln!("Error: {e}");
            process::exit(1);
        }
    }
}
