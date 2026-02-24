//! Opinionated Rust agent framework for building LLM-powered tool-use agents.
//!
//! `cinch-rs` provides a complete, model-agnostic agent runtime on top of the
//! [OpenRouter](https://openrouter.ai/) chat completions API. The core abstraction
//! is the [`Harness`](agent::harness::Harness) — a reusable agentic loop that sends messages to an LLM,
//! executes tool calls, appends results, and repeats until the model produces a
//! text-only response or a round limit is reached.
//!
//! All advanced modules (context eviction, summarization, checkpointing, tool
//! caching, plan-execute workflows) are **enabled by default** with sensible
//! defaults. A single call to [`Harness::run()`](agent::harness::Harness::run)
//! gives you the full framework.
//!
//! # Getting started
//!
//! Add `cinch-rs` to your `Cargo.toml`:
//!
//! ```toml
//! [dependencies]
//! cinch-rs = { path = "../cinch-rs" }
//! ```
//!
//! Then build and run an agent:
//!
//! ```ignore
//! use cinch_rs::prelude::*;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), String> {
//!     let api_key = std::env::var("OPENROUTER_KEY").unwrap();
//!     let client = OpenRouterClient::new(api_key)?;
//!
//!     // Register tools the LLM can call.
//!     let tools = ToolSet::new()
//!         .with_common_tools("/path/to/workdir");
//!
//!     // Configure the harness.
//!     let config = HarnessConfig::new(
//!         "anthropic/claude-sonnet-4",
//!         "You are a helpful coding assistant.",
//!     )
//!     .with_max_rounds(20)
//!     .with_max_tokens(4096);
//!
//!     // Run the agentic loop.
//!     let messages = vec![
//!         Message::system("You are a helpful coding assistant."),
//!         Message::user("Read src/main.rs and summarize it."),
//!     ];
//!
//!     let result = Harness::new(&client, &tools, config)
//!         .with_event_handler(&LoggingHandler)
//!         .run(messages)
//!         .await?;
//!
//!     println!("{}", result.text());
//!     println!("Cost: ${:.4}", result.estimated_cost_usd);
//!     Ok(())
//! }
//! ```
//!
//! # Where to find things
//!
//! If you're looking for how to...
//!
//! - **Define tools for the LLM to call:** see the [`Tool`](tools::core::Tool) trait,
//!   [`ToolSet`](tools::core::ToolSet) for collection/dispatch,
//!   [`FnTool`](tools::core::FnTool) for closure-based tools, and
//!   [`tools::common`] for built-in file and shell tools.
//!   Use [`ToolSpec`](tools::spec::ToolSpec) for rich tool descriptions with
//!   `when_to_use` / `when_not_to_use` guidance.
//!
//! - **Run the agent loop:** see [`Harness`](agent::harness::Harness) and
//!   [`HarnessConfig`](agent::config::HarnessConfig). Use
//!   [`HarnessConfig::new()`](agent::config::HarnessConfig::new) for the basics,
//!   or set struct fields directly for advanced module control (eviction,
//!   summarization, caching, plan-execute, checkpointing).
//!
//! - **Observe agent behavior:** implement [`EventHandler`](agent::events::EventHandler)
//!   to react to loop events — tool calls, text output, token usage, reasoning,
//!   approval requests. Use [`LoggingHandler`](agent::events::LoggingHandler) for
//!   tracing-based logging, [`CompositeEventHandler`](agent::events::CompositeEventHandler)
//!   to compose handlers, [`FnEventHandler`](agent::events::FnEventHandler) for
//!   closures, or [`ToolResultHandler`](agent::events::ToolResultHandler) for
//!   per-tool callbacks.
//!
//! - **Manage context and cost:** see [`ContextBudget`](context::ContextBudget) for
//!   token tracking, [`context::eviction`] for old result replacement, and
//!   [`context::layout`] for three-zone message management. Cost tracking is in
//!   [`api::tracing`] with per-model pricing tables. All of this runs automatically
//!   inside the [`Harness`](agent::harness::Harness).
//!
//! - **Stream responses:** enable via
//!   [`HarnessConfig::with_streaming(true)`](agent::config::HarnessConfig::with_streaming).
//!   The harness emits `TextDelta` and `ReasoningDelta` events during generation.
//!   See [`api::streaming`] for the SSE parser.
//!
//! - **Build structured prompts:** use
//!   [`SystemPromptBuilder`](agent::prompt::SystemPromptBuilder) for multi-section
//!   prompt assembly with conditional sections, or
//!   [`PromptRegistry`](agent::prompt::PromptRegistry) for named, prioritized
//!   sections with stable/dynamic cache-aware ordering. Enable via
//!   [`HarnessConfig::with_prompt_registry(true)`](agent::config::HarnessConfig::with_prompt_registry),
//!   or call [`build_default_prompt_registry`](agent::harness::build_default_prompt_registry)
//!   directly for full customization.
//!
//! # Modules
//!
//! | Module | Description |
//! |--------|-------------|
//! | [`agent`] | [`Harness`](agent::harness::Harness) agentic loop, config, events, checkpointing, sub-agents, profiles, plan-execute, memory |
//! | [`tools`] | [`Tool`](tools::core::Tool) trait, [`ToolSet`](tools::core::ToolSet), [`FnTool`](tools::core::FnTool), caching, filtering, DAG execution, common tools |
//! | [`context`] | [`ContextBudget`](context::ContextBudget), three-zone message layout, tool result eviction, summarization |
//! | [`api`] | Model routing, SSE streaming, retry with backoff, cost tracking |
//!
//! # Design principles
//!
//! 1. **Opinionated defaults.** The harness makes decisions so callers don't
//!    have to. Context eviction, summarization, checkpointing, and caching are
//!    all active by default. You can override any default, but you shouldn't
//!    need to.
//!
//! 2. **Tools are the unit of capability.** Every agent capability is a
//!    [`Tool`](tools::core::Tool) implementor with a JSON Schema definition
//!    and an async `execute` method. Adding a capability means implementing
//!    one trait.
//!
//! 3. **Context is the scarcest resource.** Every module treats the context
//!    window as a finite budget. Results are truncated, evicted, compressed,
//!    and organized into zones.
//!
//! 4. **Observability over magic.** The
//!    [`EventHandler`](agent::events::EventHandler) trait gives full visibility
//!    into every round. The harness makes decisions automatically but always
//!    tells you what it decided.
//!
//! 5. **Cost control is first-class.** Per-model pricing, cumulative tracking,
//!    token budget semaphores, and model routing keep API spend predictable.
//!    Every run reports `estimated_cost_usd`.

pub mod agent;
pub mod api;
pub mod context;
pub mod prelude;
pub mod tools;
pub mod ui;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tracing::{debug, trace};

// Re-export pseudo-tools for convenience.
pub use tools::core::{ThinkTool, TodoTool};

// Re-export schemars for downstream crates.
pub use schemars;

// ── Constants ──────────────────────────────────────────────────────

pub const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

/// Default model for all LLM calls.
pub const DEFAULT_MODEL: &str = "z-ai/glm-5";

/// Maximum tokens for lightweight preprocessing calls.
pub const PREPROCESSING_MAX_TOKENS: u32 = 1024;

// ── Schema generation ──────────────────────────────────────────────

/// Generate a JSON Schema `serde_json::Value` from a type that implements
/// `schemars::JsonSchema`. This is the bridge between strong Rust types
/// and the `serde_json::Value` that the OpenAI function-calling API expects.
///
/// # Example
///
/// ```
/// use cinch_rs::json_schema_for;
/// use schemars::JsonSchema;
/// use serde::Deserialize;
///
/// #[derive(Deserialize, JsonSchema)]
/// struct GrepArgs {
///     pattern: String,
///     #[serde(default)]
///     path: Option<String>,
/// }
///
/// let schema = json_schema_for::<GrepArgs>();
/// assert_eq!(schema["type"], "object");
/// assert!(schema["required"].as_array().unwrap().contains(&"pattern".into()));
/// ```
pub fn json_schema_for<T: JsonSchema>() -> serde_json::Value {
    let schema = schemars::schema_for!(T);
    serde_json::to_value(schema)
        .unwrap_or_else(|_| serde_json::json!({"type": "object", "properties": {}}))
}

// ── Request types ──────────────────────────────────────────────────

/// Chat completion request body. Superset of fields supported by the
/// OpenRouter API — unused optional fields are omitted from serialization.
#[derive(Serialize, Debug, Default)]
pub struct ChatRequest {
    // Model selection — use `model` for a single model, or `models` + `route`
    // for a fallback chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,

    // Messages
    pub messages: Vec<Message>,

    // Generation parameters
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "is_zero_f32")]
    pub temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repetition_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_a: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,

    // Output format
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,

    // Provider preferences
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderPreferences>,

    // Transforms
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transforms: Option<Vec<String>>,

    // Tools and plugins
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugins: Option<Vec<Plugin>>,

    // Reasoning / extended thinking
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningConfig>,
}

/// Reasoning effort level for extended thinking / chain-of-thought models.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Xhigh,
    High,
    Medium,
    Low,
    Minimal,
    None,
}

/// Configuration for extended thinking / reasoning tokens.
///
/// Use `effort` for OpenAI-style models or `max_tokens` for Anthropic/Gemini.
/// Do not set both simultaneously.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ReasoningConfig {
    /// Reasoning effort level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    /// Direct token budget for reasoning (Anthropic/Gemini style).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Use reasoning internally but omit from response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude: Option<bool>,
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}
fn is_zero_f32(v: &f32) -> bool {
    *v == 0.0
}

/// JSON output format type.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ResponseFormatType {
    #[serde(rename = "json_object")]
    JsonObject,
    #[serde(rename = "json_schema")]
    JsonSchema,
}

/// JSON output mode.
#[derive(Serialize, Debug)]
pub struct ResponseFormat {
    #[serde(rename = "type")]
    pub fmt_type: ResponseFormatType,
}

/// Provider routing preferences.
#[derive(Serialize, Debug)]
pub struct ProviderPreferences {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_fallbacks: Option<bool>,
}

// ── Plugin types ───────────────────────────────────────────────────

/// A strongly-typed OpenRouter plugin configuration.
///
/// Each variant maps to a known plugin ID and its options. The `Other`
/// variant allows forward-compatible extension for unknown/new plugins.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "id")]
pub enum Plugin {
    /// Web search plugin.
    #[serde(rename = "web")]
    Web {
        #[serde(skip_serializing_if = "Option::is_none")]
        max_results: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        search_prompt: Option<String>,
    },
    /// Response healing plugin (auto-fixes truncated JSON, etc.).
    #[serde(rename = "response-healing")]
    ResponseHealing,
    /// File parser plugin (PDF, etc.).
    #[serde(rename = "file-parser")]
    FileParser {
        #[serde(skip_serializing_if = "Option::is_none")]
        pdf: Option<FileParserPdfConfig>,
    },
}

/// PDF engine configuration for the file-parser plugin.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileParserPdfConfig {
    pub engine: String,
}

impl Plugin {
    /// Convenience constructor for a web-search plugin with defaults.
    pub fn web() -> Self {
        Plugin::Web {
            max_results: None,
            search_prompt: None,
        }
    }

    /// Convenience constructor for a web-search plugin with options.
    pub fn web_with(max_results: Option<u32>, search_prompt: Option<String>) -> Self {
        Plugin::Web {
            max_results,
            search_prompt,
        }
    }

    /// Convenience constructor for response-healing.
    pub fn response_healing() -> Self {
        Plugin::ResponseHealing
    }

    /// Convenience constructor for file-parser with a PDF engine.
    pub fn file_parser(engine: impl Into<String>) -> Self {
        Plugin::FileParser {
            pdf: Some(FileParserPdfConfig {
                engine: engine.into(),
            }),
        }
    }

    /// The plugin's ID string (for deduplication checks).
    pub fn id(&self) -> &str {
        match self {
            Plugin::Web { .. } => "web",
            Plugin::ResponseHealing => "response-healing",
            Plugin::FileParser { .. } => "file-parser",
        }
    }
}

/// Extension trait for `Vec<Plugin>` with deduplication helpers.
///
/// The harness CLI needs to merge plugins
/// from multiple sources (CLI flags, config files, shortcut flags) without
/// duplicates. These helpers centralise the add-or-replace logic.
pub trait PluginVecExt {
    /// Push a plugin only if no plugin with the same ID is already present.
    fn push_if_absent(&mut self, plugin: Plugin);
    /// Insert or replace a plugin by ID.
    fn upsert(&mut self, plugin: Plugin);
}

impl PluginVecExt for Vec<Plugin> {
    fn push_if_absent(&mut self, plugin: Plugin) {
        let id = plugin.id();
        if !self.iter().any(|p| p.id() == id) {
            self.push(plugin);
        }
    }

    fn upsert(&mut self, plugin: Plugin) {
        let id = plugin.id();
        if let Some(idx) = self.iter().position(|p| p.id() == id) {
            self[idx] = plugin;
        } else {
            self.push(plugin);
        }
    }
}

// ── Message types ──────────────────────────────────────────────────

/// Role of a message in the conversation.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

impl std::fmt::Display for MessageRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MessageRole::System => write!(f, "system"),
            MessageRole::User => write!(f, "user"),
            MessageRole::Assistant => write!(f, "assistant"),
            MessageRole::Tool => write!(f, "tool"),
        }
    }
}

/// A message in the conversation.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Message {
    pub role: MessageRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant_text(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant_tool_calls(calls: Vec<ToolCall>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: None,
            tool_calls: Some(calls),
            tool_call_id: None,
        }
    }

    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Tool,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(call_id.into()),
        }
    }
}

// ── Tool types ─────────────────────────────────────────────────────

/// The type of a tool definition. Currently always `Function`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ToolType {
    #[serde(rename = "function")]
    Function,
}

/// Tool definition sent to the API (OpenAI function-calling format).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub tool_type: ToolType,
    pub function: FunctionDef,
}

impl ToolDef {
    /// Create a function-calling tool definition.
    ///
    /// This is the standard constructor — `ToolType` is always `Function` in
    /// the current API, so there's no reason to specify it manually.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            tool_type: ToolType::Function,
            function: FunctionDef {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// The type of a tool call. Currently always `Function`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum CallType {
    #[serde(rename = "function")]
    Function,
}

/// A tool call returned by the model.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: CallType,
    pub function: FunctionCallData,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FunctionCallData {
    pub name: String,
    pub arguments: String,
}

// ── Response types ─────────────────────────────────────────────────

/// Raw API response (internal deserialization target).
#[derive(Deserialize, Debug)]
struct RawChatResponse {
    choices: Option<Vec<RawChoice>>,
    error: Option<ApiErrorResponse>,
    #[serde(default)]
    usage: Option<UsageInfo>,
}

#[derive(Deserialize, Debug)]
struct RawChoice {
    message: RawResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Debug)]
struct RawResponseMessage {
    content: Option<String>,
    tool_calls: Option<Vec<ToolCall>>,
    annotations: Option<Vec<Annotation>>,
    reasoning: Option<String>,
}

#[derive(Deserialize, Debug)]
struct ApiErrorResponse {
    message: String,
}

/// Clean return type from `OpenRouterClient::chat()`.
#[derive(Debug)]
pub struct ChatCompletion {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Option<UsageInfo>,
    pub annotations: Vec<Annotation>,
    pub finish_reason: Option<String>,
    /// Reasoning / extended thinking content returned by the model.
    pub reasoning: Option<String>,
}

/// Token usage statistics.
#[derive(Deserialize, Debug, Clone)]
pub struct UsageInfo {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
}

/// URL citation annotation returned by the web-search plugin.
#[derive(Deserialize, Debug)]
pub struct Annotation {
    #[serde(rename = "type")]
    pub annotation_type: Option<String>,
    pub url_citation: Option<UrlCitation>,
}

#[derive(Deserialize, Debug)]
pub struct UrlCitation {
    pub url: String,
    pub title: Option<String>,
}

// ── Client ─────────────────────────────────────────────────────────

/// Async HTTP client for the OpenRouter chat completions API.
pub struct OpenRouterClient {
    pub(crate) client: reqwest::Client,
    pub(crate) api_key: String,
    pub(crate) referer: String,
    pub(crate) title: String,
}

impl OpenRouterClient {
    /// Create a new client with the given API key and default headers.
    pub fn new(api_key: impl Into<String>) -> Result<Self, String> {
        Self::with_headers(api_key, "https://github.com/cinch-rs", "cinch-rs")
    }

    /// Create a new client with custom Referer and X-Title headers.
    pub fn with_headers(
        api_key: impl Into<String>,
        referer: impl Into<String>,
        title: impl Into<String>,
    ) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .user_agent("openrouter-client/0.1")
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;
        Ok(Self {
            client,
            api_key: api_key.into(),
            referer: referer.into(),
            title: title.into(),
        })
    }

    /// Send a chat completion request.
    pub async fn chat(&self, body: &ChatRequest) -> Result<ChatCompletion, String> {
        let msg_count = body.messages.len();
        let tool_count = body.tools.as_ref().map_or(0, |t| t.len());
        let model_label = body
            .model
            .as_deref()
            .or_else(|| {
                body.models
                    .as_ref()
                    .and_then(|m| m.first().map(|s| s.as_str()))
            })
            .unwrap_or("(none)");
        debug!(
            "LLM request: model={}, messages={}, tools={}, max_tokens={}, temp={}",
            model_label, msg_count, tool_count, body.max_tokens, body.temperature,
        );
        trace!(
            "Request payload size: {} bytes",
            serde_json::to_string(body).map_or(0, |s| s.len())
        );

        let start = Instant::now();

        let resp = self
            .client
            .post(OPENROUTER_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("HTTP-Referer", &self.referer)
            .header("X-Title", &self.title)
            .json(body)
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("failed to read response: {e}"))?;

        let elapsed = start.elapsed();
        debug!(
            "LLM response: HTTP {} in {:.1}s ({} bytes)",
            status,
            elapsed.as_secs_f64(),
            text.len()
        );

        if !status.is_success() {
            return Err(format!("OpenRouter API HTTP {status}: {text}"));
        }

        let parsed: RawChatResponse =
            serde_json::from_str(&text).map_err(|e| format!("failed to parse response: {e}"))?;

        if let Some(err) = parsed.error {
            return Err(format!("OpenRouter API error: {}", err.message));
        }

        if let Some(ref usage) = parsed.usage {
            debug!(
                "Token usage: prompt={}, completion={}, total={}",
                usage.prompt_tokens.unwrap_or(0),
                usage.completion_tokens.unwrap_or(0),
                usage.total_tokens.unwrap_or(0),
            );
        }

        let choice = parsed.choices.and_then(|c| c.into_iter().next());

        match choice {
            Some(ref c) => {
                let content_len = c.message.content.as_ref().map_or(0, |s| s.len());
                let tc_count = c.message.tool_calls.as_ref().map_or(0, |t| t.len());
                debug!(
                    "LLM output: {} chars text, {} tool call(s)",
                    content_len, tc_count
                );
            }
            None => debug!("LLM output: empty (no choices)"),
        }

        match choice {
            Some(c) => Ok(ChatCompletion {
                content: c.message.content,
                tool_calls: c.message.tool_calls.unwrap_or_default(),
                usage: parsed.usage,
                annotations: c.message.annotations.unwrap_or_default(),
                finish_reason: c.finish_reason,
                reasoning: c.message.reasoning,
            }),
            None => Ok(ChatCompletion {
                content: None,
                tool_calls: vec![],
                usage: parsed.usage,
                annotations: vec![],
                finish_reason: None,
                reasoning: None,
            }),
        }
    }
}

// ── Convenience ────────────────────────────────────────────────────

/// Run a quick one-shot LLM completion for data preprocessing.
///
/// Reads the API key from the `OPENROUTER_KEY` environment variable.
/// Returns `Err` if the key is not set or the API call fails.
pub async fn quick_completion(system: &str, user: &str, model: &str) -> Result<String, String> {
    let api_key =
        std::env::var("OPENROUTER_KEY").map_err(|_| "OPENROUTER_KEY not set".to_string())?;

    let client = OpenRouterClient::new(api_key)?;

    let body = ChatRequest {
        model: Some(model.to_string()),
        messages: vec![Message::system(system), Message::user(user)],
        max_tokens: PREPROCESSING_MAX_TOKENS,
        temperature: 0.3,
        ..Default::default()
    };

    let completion = client.chat(&body).await?;
    completion
        .content
        .ok_or_else(|| "Empty LLM response".to_string())
}

// ── Citation formatting ────────────────────────────────────────────

/// Format web-search URL citations as a "Sources:" footer.
pub fn format_citations(annotations: &[Annotation]) -> String {
    let citations: Vec<String> = annotations
        .iter()
        .filter(|a| a.annotation_type.as_deref() == Some("url_citation"))
        .filter_map(|a| {
            a.url_citation.as_ref().map(|c| {
                let title = c.title.as_deref().unwrap_or(&c.url);
                format!("- [{title}]({})", c.url)
            })
        })
        .collect();

    let mut seen = std::collections::HashSet::new();
    let unique: Vec<&str> = citations
        .iter()
        .filter(|c| seen.insert(c.as_str()))
        .map(|c| c.as_str())
        .collect();

    if unique.is_empty() {
        String::new()
    } else {
        format!("\n\nSources:\n{}", unique.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_constructors() {
        let sys = Message::system("hello");
        assert_eq!(sys.role, MessageRole::System);
        assert_eq!(sys.content.as_deref(), Some("hello"));

        let user = Message::user("world");
        assert_eq!(user.role, MessageRole::User);

        let assist = Message::assistant_text("prefill");
        assert_eq!(assist.role, MessageRole::Assistant);
        assert_eq!(assist.content.as_deref(), Some("prefill"));

        let tool = Message::tool_result("call-1", "result");
        assert_eq!(tool.role, MessageRole::Tool);
        assert_eq!(tool.tool_call_id.as_deref(), Some("call-1"));
    }

    #[test]
    fn chat_request_default_skips_none_fields() {
        let req = ChatRequest {
            model: Some("test-model".into()),
            messages: vec![Message::user("hi")],
            max_tokens: 100,
            temperature: 0.5,
            ..Default::default()
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("top_p").is_none());
        assert!(json.get("tools").is_none());
        assert!(json.get("plugins").is_none());
        assert!(json.get("models").is_none());
    }

    #[test]
    fn format_citations_deduplicates() {
        let anns = vec![
            Annotation {
                annotation_type: Some("url_citation".into()),
                url_citation: Some(UrlCitation {
                    url: "https://example.com".into(),
                    title: Some("Example".into()),
                }),
            },
            Annotation {
                annotation_type: Some("url_citation".into()),
                url_citation: Some(UrlCitation {
                    url: "https://example.com".into(),
                    title: Some("Example".into()),
                }),
            },
        ];
        let result = format_citations(&anns);
        assert_eq!(
            result.matches("example.com").count(),
            1,
            "should deduplicate"
        );
    }
}
