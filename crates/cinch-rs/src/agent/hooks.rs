//! Typed lifecycle hooks and external hook runner.
//!
//! This module provides two complementary mechanisms for hooking into the
//! agent lifecycle:
//!
//! 1. **[`LifecycleHook`]** — A typed trait with named methods for each
//!    lifecycle point (`pre_tool_use`, `post_tool_use`, `session_start`, etc.).
//!    [`LifecycleHookAdapter`] converts any `LifecycleHook` into an
//!    [`EventHandler`] so it plugs into the existing
//!    [`CompositeEventHandler`](super::events::CompositeEventHandler) chain.
//!
//! 2. **[`ExternalHookRunner`]** — An [`EventHandler`] that runs shell
//!    commands at lifecycle points. Configured via [`HookConfig`] structs
//!    (loadable from `.cinch/hooks.json`).

use super::events::{EventHandler, EventResponse, HarnessEvent};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::warn;

// ── Enums ──────────────────────────────────────────────────────────

/// Action returned by [`LifecycleHook::pre_tool_use`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookAction {
    /// Allow the tool to execute.
    Proceed,
    /// Block the tool with the given reason (sent to the LLM).
    Block(String),
}

/// Action returned by [`LifecycleHook::on_stop`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopAction {
    /// Allow the agent to stop.
    Allow,
    /// Inject a message and continue for another round.
    Continue(String),
}

// ── LifecycleHook trait ────────────────────────────────────────────

/// Typed lifecycle hook with named methods for each lifecycle point.
///
/// All methods have default no-op implementations. Implement only the
/// hooks you need, then wrap with [`LifecycleHookAdapter`] to use as
/// an [`EventHandler`].
///
/// # Example
///
/// ```ignore
/// struct MyHook;
///
/// impl LifecycleHook for MyHook {
///     fn pre_tool_use(&self, tool: &str, _args: &str) -> HookAction {
///         if tool == "shell" {
///             HookAction::Block("Shell disabled.".into())
///         } else {
///             HookAction::Proceed
///         }
///     }
/// }
///
/// let handler = LifecycleHookAdapter::new(MyHook);
/// ```
pub trait LifecycleHook: Send + Sync {
    /// Called before a tool executes. Return `Block` to prevent execution.
    fn pre_tool_use(&self, _tool: &str, _args: &str) -> HookAction {
        HookAction::Proceed
    }

    /// Called after a tool executes. Return a message to inject into the
    /// conversation, or `None` to do nothing.
    fn post_tool_use(&self, _tool: &str, _result: &str) -> Option<String> {
        None
    }

    /// Called before context compaction. Return a message to include in
    /// the summarization input for state preservation.
    fn pre_compact(&self) -> Option<String> {
        None
    }

    /// Called when the agent would stop (no more tool calls). Return
    /// `Continue` to inject a message and keep going.
    fn on_stop(&self) -> StopAction {
        StopAction::Allow
    }

    /// Called when a session starts.
    fn session_start(&self, _trace_id: &str) {}

    /// Called when a session is finishing with a summary.
    fn session_end_summary(&self, _trace_id: &str, _finished: bool, _rounds_used: u32) {}
}

// ── LifecycleHookAdapter ───────────────────────────────────────────

/// Adapts a [`LifecycleHook`] into an [`EventHandler`].
///
/// Maps harness events to the corresponding typed hook methods:
/// - `ApprovalRequired` → `pre_tool_use()`
/// - `ToolResult` → `post_tool_use()`
/// - `PreCompaction` → `pre_compact()`
/// - `Finished` → `on_stop()`
/// - `SessionStarting` → `session_start()`
/// - `SessionFinishing` → `session_end_summary()`
pub struct LifecycleHookAdapter<H: LifecycleHook> {
    hook: H,
}

impl<H: LifecycleHook> LifecycleHookAdapter<H> {
    /// Wrap a lifecycle hook into an event handler adapter.
    pub fn new(hook: H) -> Self {
        Self { hook }
    }
}

impl<H: LifecycleHook> EventHandler for LifecycleHookAdapter<H> {
    fn on_event(&self, event: &HarnessEvent<'_>) -> Option<EventResponse> {
        match event {
            HarnessEvent::ApprovalRequired { name, arguments } => {
                match self.hook.pre_tool_use(name, arguments) {
                    HookAction::Block(reason) => Some(EventResponse::Deny(reason)),
                    HookAction::Proceed => None,
                }
            }
            HarnessEvent::ToolResult { name, result, .. } => self
                .hook
                .post_tool_use(name, result)
                .map(EventResponse::InjectMessage),
            HarnessEvent::PreCompaction => {
                self.hook.pre_compact().map(EventResponse::InjectMessage)
            }
            HarnessEvent::Finished => match self.hook.on_stop() {
                StopAction::Continue(msg) => Some(EventResponse::InjectMessage(msg)),
                StopAction::Allow => None,
            },
            HarnessEvent::SessionStarting { trace_id } => {
                self.hook.session_start(trace_id);
                None
            }
            HarnessEvent::SessionFinishing {
                trace_id,
                finished,
                rounds_used,
            } => {
                self.hook
                    .session_end_summary(trace_id, *finished, *rounds_used);
                None
            }
            _ => None,
        }
    }
}

// ── HookConfig ─────────────────────────────────────────────────────

/// Configuration for external shell hooks, loadable from JSON.
///
/// # Example JSON
///
/// ```json
/// {
///   "pre_tool_use": [
///     { "command": "check-tool.sh", "matcher": "shell" }
///   ],
///   "session_start": [
///     { "command": "notify.sh start" }
///   ]
/// }
/// ```
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct HookConfig {
    #[serde(default)]
    pub pre_tool_use: Vec<HookEntry>,
    #[serde(default)]
    pub post_tool_use: Vec<HookEntry>,
    #[serde(default)]
    pub session_start: Vec<HookEntry>,
    #[serde(default)]
    pub session_end: Vec<HookEntry>,
}

/// A single hook entry: a shell command with an optional tool name matcher.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct HookEntry {
    /// Shell command to execute.
    pub command: String,
    /// Optional tool name matcher. If set, this hook only fires for tools
    /// whose name contains the matcher string.
    #[serde(default)]
    pub matcher: Option<String>,
}

// ── ExternalHookRunner ─────────────────────────────────────────────

/// An [`EventHandler`] that runs external shell commands at lifecycle points.
///
/// Commands receive context via environment variables:
/// - `CINCH_HOOK_EVENT`: event name (e.g., `pre_tool_use`)
/// - `CINCH_TOOL_NAME`: tool name (for tool hooks)
/// - `CINCH_TOOL_ARGS`: tool arguments JSON (for `pre_tool_use`)
/// - `CINCH_TOOL_RESULT`: tool result (for `post_tool_use`, truncated to 10KB)
/// - `CINCH_TRACE_ID`: session trace ID
///
/// Exit code 0 = proceed, non-zero = block (for `pre_tool_use`).
/// Stdout is captured as the block reason or injection message.
pub struct ExternalHookRunner {
    hooks: HookConfig,
    workdir: String,
}

/// Maximum bytes of tool result passed via `CINCH_TOOL_RESULT`.
const MAX_TOOL_RESULT_ENV_BYTES: usize = 10_240;

impl ExternalHookRunner {
    /// Create a runner with the given config and working directory.
    pub fn new(hooks: HookConfig, workdir: impl Into<String>) -> Self {
        Self {
            hooks,
            workdir: workdir.into(),
        }
    }

    /// Load hook config from a JSON file. Returns a runner with default
    /// (empty) config if the file doesn't exist or can't be parsed.
    pub fn load(path: impl AsRef<Path>, workdir: impl Into<String>) -> Self {
        let hooks = match std::fs::read_to_string(path.as_ref()) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|e| {
                warn!("Failed to parse hooks config: {e}");
                HookConfig::default()
            }),
            Err(_) => HookConfig::default(),
        };
        Self {
            hooks,
            workdir: workdir.into(),
        }
    }

    /// Check if a hook entry matches the given tool name.
    fn matches_tool(entry: &HookEntry, tool_name: &str) -> bool {
        match &entry.matcher {
            Some(m) => tool_name.contains(m.as_str()),
            None => true,
        }
    }

    /// Run a single hook command synchronously (blocking).
    fn run_hook_sync(
        &self,
        command: &str,
        env_vars: &[(&str, &str)],
    ) -> Result<(i32, String), String> {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg(command).current_dir(&self.workdir);

        for (key, val) in env_vars {
            cmd.env(key, val);
        }

        let output = cmd
            .output()
            .map_err(|e| format!("Failed to execute hook command: {e}"))?;

        let exit_code = output.status.code().unwrap_or(1);
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();

        Ok((exit_code, stdout))
    }

    /// Run hooks for `pre_tool_use` and return a deny response if any hook blocks.
    fn run_pre_tool_hooks(&self, name: &str, arguments: &str) -> Option<EventResponse> {
        for entry in &self.hooks.pre_tool_use {
            if !Self::matches_tool(entry, name) {
                continue;
            }
            let env_vars = vec![
                ("CINCH_HOOK_EVENT", "pre_tool_use"),
                ("CINCH_TOOL_NAME", name),
                ("CINCH_TOOL_ARGS", arguments),
            ];
            match self.run_hook_sync(&entry.command, &env_vars) {
                Ok((0, _)) => {} // proceed
                Ok((_, reason)) => {
                    let msg = if reason.is_empty() {
                        format!("Hook blocked tool '{name}'")
                    } else {
                        reason
                    };
                    return Some(EventResponse::Deny(msg));
                }
                Err(e) => {
                    warn!("pre_tool_use hook failed: {e}");
                }
            }
        }
        None
    }

    /// Run hooks for `post_tool_use` and return an inject message if any produces output.
    fn run_post_tool_hooks(&self, name: &str, result: &str) -> Option<EventResponse> {
        let truncated_result: String = result.chars().take(MAX_TOOL_RESULT_ENV_BYTES).collect();
        for entry in &self.hooks.post_tool_use {
            if !Self::matches_tool(entry, name) {
                continue;
            }
            let env_vars = vec![
                ("CINCH_HOOK_EVENT", "post_tool_use"),
                ("CINCH_TOOL_NAME", name),
                ("CINCH_TOOL_RESULT", truncated_result.as_str()),
            ];
            match self.run_hook_sync(&entry.command, &env_vars) {
                Ok((_, output)) if !output.is_empty() => {
                    return Some(EventResponse::InjectMessage(output));
                }
                Ok(_) => {}
                Err(e) => {
                    warn!("post_tool_use hook failed: {e}");
                }
            }
        }
        None
    }

    /// Run session start hooks (fire-and-forget, no response).
    fn run_session_start_hooks(&self, trace_id: &str) {
        for entry in &self.hooks.session_start {
            let env_vars = vec![
                ("CINCH_HOOK_EVENT", "session_start"),
                ("CINCH_TRACE_ID", trace_id),
            ];
            if let Err(e) = self.run_hook_sync(&entry.command, &env_vars) {
                warn!("session_start hook failed: {e}");
            }
        }
    }

    /// Run session end hooks (fire-and-forget, no response).
    fn run_session_end_hooks(&self, trace_id: &str, finished: bool, rounds_used: u32) {
        let finished_str = finished.to_string();
        let rounds_str = rounds_used.to_string();
        for entry in &self.hooks.session_end {
            let env_vars = vec![
                ("CINCH_HOOK_EVENT", "session_end"),
                ("CINCH_TRACE_ID", trace_id),
                ("CINCH_FINISHED", finished_str.as_str()),
                ("CINCH_ROUNDS_USED", rounds_str.as_str()),
            ];
            if let Err(e) = self.run_hook_sync(&entry.command, &env_vars) {
                warn!("session_end hook failed: {e}");
            }
        }
    }
}

impl EventHandler for ExternalHookRunner {
    fn on_event(&self, event: &HarnessEvent<'_>) -> Option<EventResponse> {
        match event {
            HarnessEvent::ApprovalRequired { name, arguments } => {
                self.run_pre_tool_hooks(name, arguments)
            }
            HarnessEvent::ToolResult { name, result, .. } => self.run_post_tool_hooks(name, result),
            HarnessEvent::SessionStarting { trace_id } => {
                self.run_session_start_hooks(trace_id);
                None
            }
            HarnessEvent::SessionFinishing {
                trace_id,
                finished,
                rounds_used,
            } => {
                self.run_session_end_hooks(trace_id, *finished, *rounds_used);
                None
            }
            _ => None,
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Default trait method tests ─────────────────────────────────

    struct DefaultHook;
    impl LifecycleHook for DefaultHook {}

    #[test]
    fn hook_action_default_is_proceed() {
        let hook = DefaultHook;
        assert_eq!(hook.pre_tool_use("test", "{}"), HookAction::Proceed);
    }

    #[test]
    fn stop_action_default_is_allow() {
        let hook = DefaultHook;
        assert_eq!(hook.on_stop(), StopAction::Allow);
    }

    // ── Adapter mapping tests ──────────────────────────────────────

    struct BlockingHook;
    impl LifecycleHook for BlockingHook {
        fn pre_tool_use(&self, tool: &str, _args: &str) -> HookAction {
            if tool == "shell" {
                HookAction::Block("Shell blocked".into())
            } else {
                HookAction::Proceed
            }
        }

        fn post_tool_use(&self, _tool: &str, result: &str) -> Option<String> {
            if result.contains("error") {
                Some("Error detected in tool result".into())
            } else {
                None
            }
        }

        fn pre_compact(&self) -> Option<String> {
            Some("Preserve this state".into())
        }

        fn on_stop(&self) -> StopAction {
            StopAction::Continue("Keep going".into())
        }
    }

    #[test]
    fn adapter_maps_approval_to_pre_tool() {
        let adapter = LifecycleHookAdapter::new(DefaultHook);
        let event = HarnessEvent::ApprovalRequired {
            name: "read_file",
            arguments: "{}",
        };
        assert!(adapter.on_event(&event).is_none());
    }

    #[test]
    fn adapter_block_maps_to_deny() {
        let adapter = LifecycleHookAdapter::new(BlockingHook);
        let event = HarnessEvent::ApprovalRequired {
            name: "shell",
            arguments: "{}",
        };
        let response = adapter.on_event(&event);
        assert!(matches!(response, Some(EventResponse::Deny(ref r)) if r == "Shell blocked"));
    }

    #[test]
    fn adapter_post_tool_injects_message() {
        let adapter = LifecycleHookAdapter::new(BlockingHook);
        let event = HarnessEvent::ToolResult {
            name: "shell",
            call_id: "1",
            result: "some error occurred",
        };
        let response = adapter.on_event(&event);
        assert!(
            matches!(response, Some(EventResponse::InjectMessage(ref m)) if m.contains("Error detected"))
        );
    }

    #[test]
    fn adapter_pre_compact_injects() {
        let adapter = LifecycleHookAdapter::new(BlockingHook);
        let event = HarnessEvent::PreCompaction;
        let response = adapter.on_event(&event);
        assert!(
            matches!(response, Some(EventResponse::InjectMessage(ref m)) if m.contains("Preserve"))
        );
    }

    #[test]
    fn adapter_stop_continue_injects() {
        let adapter = LifecycleHookAdapter::new(BlockingHook);
        let event = HarnessEvent::Finished;
        let response = adapter.on_event(&event);
        assert!(
            matches!(response, Some(EventResponse::InjectMessage(ref m)) if m.contains("Keep going"))
        );
    }

    #[test]
    fn adapter_ignores_irrelevant_events() {
        let adapter = LifecycleHookAdapter::new(BlockingHook);
        assert!(adapter.on_event(&HarnessEvent::Text("hello")).is_none());
        assert!(
            adapter
                .on_event(&HarnessEvent::RoundLimitReached { max_rounds: 10 })
                .is_none()
        );
    }

    // ── Session lifecycle adapter tests ────────────────────────────

    use std::sync::{Arc, Mutex};

    struct TrackingHook {
        started: Arc<Mutex<Vec<String>>>,
        ended: Arc<Mutex<Vec<(String, bool, u32)>>>,
    }

    impl LifecycleHook for TrackingHook {
        fn session_start(&self, trace_id: &str) {
            self.started.lock().unwrap().push(trace_id.to_string());
        }

        fn session_end_summary(&self, trace_id: &str, finished: bool, rounds_used: u32) {
            self.ended
                .lock()
                .unwrap()
                .push((trace_id.to_string(), finished, rounds_used));
        }
    }

    #[test]
    fn adapter_session_start_fires() {
        let started = Arc::new(Mutex::new(Vec::new()));
        let ended = Arc::new(Mutex::new(Vec::new()));
        let adapter = LifecycleHookAdapter::new(TrackingHook {
            started: started.clone(),
            ended: ended.clone(),
        });

        let event = HarnessEvent::SessionStarting { trace_id: "tr-123" };
        let response = adapter.on_event(&event);
        assert!(response.is_none());
        assert_eq!(started.lock().unwrap().len(), 1);
        assert_eq!(started.lock().unwrap()[0], "tr-123");
    }

    #[test]
    fn adapter_session_finishing_fires() {
        let started = Arc::new(Mutex::new(Vec::new()));
        let ended = Arc::new(Mutex::new(Vec::new()));
        let adapter = LifecycleHookAdapter::new(TrackingHook {
            started: started.clone(),
            ended: ended.clone(),
        });

        let event = HarnessEvent::SessionFinishing {
            trace_id: "tr-456",
            finished: true,
            rounds_used: 5,
        };
        let response = adapter.on_event(&event);
        assert!(response.is_none());
        let e = ended.lock().unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0], ("tr-456".to_string(), true, 5));
    }

    // ── HookConfig deserialization tests ───────────────────────────

    #[test]
    fn hook_config_deserialize_empty() {
        let config: HookConfig = serde_json::from_str("{}").unwrap();
        assert!(config.pre_tool_use.is_empty());
        assert!(config.post_tool_use.is_empty());
        assert!(config.session_start.is_empty());
        assert!(config.session_end.is_empty());
    }

    #[test]
    fn hook_config_deserialize_full() {
        let json = r#"{
            "pre_tool_use": [
                { "command": "check.sh", "matcher": "shell" }
            ],
            "post_tool_use": [
                { "command": "lint.sh" }
            ],
            "session_start": [
                { "command": "notify.sh start" }
            ],
            "session_end": [
                { "command": "notify.sh end", "matcher": null }
            ]
        }"#;
        let config: HookConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.pre_tool_use.len(), 1);
        assert_eq!(config.pre_tool_use[0].command, "check.sh");
        assert_eq!(config.pre_tool_use[0].matcher.as_deref(), Some("shell"));
        assert_eq!(config.post_tool_use.len(), 1);
        assert!(config.post_tool_use[0].matcher.is_none());
        assert_eq!(config.session_start.len(), 1);
        assert_eq!(config.session_end.len(), 1);
    }

    #[test]
    fn hook_config_load_missing_file() {
        let runner = ExternalHookRunner::load("/nonexistent/hooks.json", "/tmp");
        assert!(runner.hooks.pre_tool_use.is_empty());
    }

    // ── ExternalHookRunner matcher tests ───────────────────────────

    #[test]
    fn external_runner_matches_tool_name() {
        let entry = HookEntry {
            command: "echo ok".into(),
            matcher: Some("shell".into()),
        };
        assert!(ExternalHookRunner::matches_tool(&entry, "shell"));
        assert!(ExternalHookRunner::matches_tool(&entry, "run_shell"));
        assert!(!ExternalHookRunner::matches_tool(&entry, "read_file"));
    }

    #[test]
    fn external_runner_no_matcher_matches_all() {
        let entry = HookEntry {
            command: "echo ok".into(),
            matcher: None,
        };
        assert!(ExternalHookRunner::matches_tool(&entry, "shell"));
        assert!(ExternalHookRunner::matches_tool(&entry, "read_file"));
        assert!(ExternalHookRunner::matches_tool(&entry, "anything"));
    }

    // ── ExternalHookRunner execution tests ─────────────────────────

    #[test]
    fn external_runner_pre_tool_proceed_on_exit_zero() {
        let config = HookConfig {
            pre_tool_use: vec![HookEntry {
                command: "exit 0".into(),
                matcher: None,
            }],
            ..Default::default()
        };
        let runner = ExternalHookRunner::new(config, "/tmp");
        let event = HarnessEvent::ApprovalRequired {
            name: "test_tool",
            arguments: "{}",
        };
        assert!(runner.on_event(&event).is_none());
    }

    #[test]
    fn external_runner_pre_tool_blocks_on_exit_nonzero() {
        let config = HookConfig {
            pre_tool_use: vec![HookEntry {
                command: "echo 'blocked by hook' && exit 1".into(),
                matcher: None,
            }],
            ..Default::default()
        };
        let runner = ExternalHookRunner::new(config, "/tmp");
        let event = HarnessEvent::ApprovalRequired {
            name: "test_tool",
            arguments: "{}",
        };
        let response = runner.on_event(&event);
        assert!(
            matches!(response, Some(EventResponse::Deny(ref r)) if r.contains("blocked by hook"))
        );
    }

    #[test]
    fn external_runner_post_tool_injects_stdout() {
        let config = HookConfig {
            post_tool_use: vec![HookEntry {
                command: "echo 'injected message'".into(),
                matcher: None,
            }],
            ..Default::default()
        };
        let runner = ExternalHookRunner::new(config, "/tmp");
        let event = HarnessEvent::ToolResult {
            name: "test_tool",
            call_id: "1",
            result: "ok",
        };
        let response = runner.on_event(&event);
        assert!(
            matches!(response, Some(EventResponse::InjectMessage(ref m)) if m.contains("injected message"))
        );
    }

    #[test]
    fn external_runner_ignores_unmatched_tool() {
        let config = HookConfig {
            pre_tool_use: vec![HookEntry {
                command: "exit 1".into(),
                matcher: Some("shell".into()),
            }],
            ..Default::default()
        };
        let runner = ExternalHookRunner::new(config, "/tmp");
        let event = HarnessEvent::ApprovalRequired {
            name: "read_file",
            arguments: "{}",
        };
        // Should not block because matcher doesn't match.
        assert!(runner.on_event(&event).is_none());
    }

    #[test]
    fn external_runner_ignores_irrelevant_events() {
        let config = HookConfig {
            pre_tool_use: vec![HookEntry {
                command: "exit 1".into(),
                matcher: None,
            }],
            ..Default::default()
        };
        let runner = ExternalHookRunner::new(config, "/tmp");
        assert!(runner.on_event(&HarnessEvent::Finished).is_none());
        assert!(runner.on_event(&HarnessEvent::Text("hello")).is_none());
    }
}
