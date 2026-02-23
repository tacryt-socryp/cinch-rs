//! Persistent agent identity across sessions.
//!
//! An [`AgentProfile`] accumulates cross-session state: calibrated token
//! estimator parameters, tool usage statistics, model observations, and
//! user instructions. It is loaded at the start of a
//! harness run and saved on completion, enabling compounding learning.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tracing::debug;

/// Cross-session agent state that persists between runs.
#[derive(Debug, Serialize, Deserialize)]
pub struct AgentProfile {
    /// Unique identifier for this agent profile.
    pub agent_id: String,
    /// Calibrated token estimator (chars-per-token ratio).
    pub chars_per_token: f64,
    /// Per-tool usage statistics.
    pub tool_usage: HashMap<String, ToolUsageStats>,
    /// Model performance observations.
    pub model_observations: Vec<ModelObservation>,
    /// User-provided behavioral instructions.
    pub user_instructions: Vec<String>,
    /// ISO 8601 timestamp when the profile was created.
    pub created_at: String,
    /// ISO 8601 timestamp of the last run.
    pub last_run_at: String,
    /// Total number of harness runs.
    pub total_runs: u64,
    /// Total estimated cost across all runs.
    pub total_cost_usd: f64,
}

/// Per-tool usage statistics.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ToolUsageStats {
    /// Number of times this tool has been called.
    pub call_count: u64,
    /// Number of times the tool produced an error result.
    pub error_count: u64,
    /// Average result size in bytes.
    pub avg_result_bytes: f64,
    /// Total execution time in milliseconds.
    pub total_execution_ms: u64,
}

/// An observation about model performance from a single run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelObservation {
    /// Model identifier.
    pub model: String,
    /// Rounds used in this run.
    pub rounds_used: u32,
    /// Prompt tokens consumed.
    pub prompt_tokens: u32,
    /// Completion tokens consumed.
    pub completion_tokens: u32,
    /// Whether the run finished naturally.
    pub finished: bool,
    /// Estimated cost in USD for this run.
    pub cost_usd: f64,
    /// ISO 8601 timestamp.
    pub timestamp: String,
}

impl AgentProfile {
    /// Create a new profile with default settings.
    pub fn new(agent_id: impl Into<String>) -> Self {
        let now = now_iso8601();
        Self {
            agent_id: agent_id.into(),
            chars_per_token: crate::context::DEFAULT_CHARS_PER_TOKEN,
            tool_usage: HashMap::new(),
            model_observations: Vec::new(),
            user_instructions: Vec::new(),
            created_at: now.clone(),
            last_run_at: now,
            total_runs: 0,
            total_cost_usd: 0.0,
        }
    }

    /// Load a profile from a JSON file, or create a new one if it doesn't exist.
    pub fn load_or_create(path: &Path, agent_id: &str) -> Result<Self, String> {
        if path.exists() {
            let data = std::fs::read_to_string(path)
                .map_err(|e| format!("failed to read profile: {e}"))?;
            let profile: AgentProfile =
                serde_json::from_str(&data).map_err(|e| format!("failed to parse profile: {e}"))?;
            debug!(
                "Loaded agent profile '{}' ({} runs)",
                profile.agent_id, profile.total_runs
            );
            Ok(profile)
        } else {
            debug!(
                "Creating new agent profile '{agent_id}' at {}",
                path.display()
            );
            Ok(Self::new(agent_id))
        }
    }

    /// Save the profile to a JSON file.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let data = serde_json::to_string_pretty(self)
            .map_err(|e| format!("failed to serialize profile: {e}"))?;
        std::fs::write(path, data).map_err(|e| format!("failed to write profile: {e}"))?;
        debug!(
            "Saved agent profile '{}' ({} runs)",
            self.agent_id, self.total_runs
        );
        Ok(())
    }

    /// Record the outcome of a harness run.
    pub fn record_run(
        &mut self,
        model: &str,
        rounds_used: u32,
        prompt_tokens: u32,
        completion_tokens: u32,
        finished: bool,
        cost_usd: f64,
    ) {
        self.total_runs += 1;
        self.total_cost_usd += cost_usd;
        self.last_run_at = now_iso8601();

        self.model_observations.push(ModelObservation {
            model: model.into(),
            rounds_used,
            prompt_tokens,
            completion_tokens,
            finished,
            cost_usd,
            timestamp: self.last_run_at.clone(),
        });

        // Keep observations bounded (last 100).
        if self.model_observations.len() > 100 {
            let excess = self.model_observations.len() - 100;
            self.model_observations.drain(..excess);
        }
    }

    /// Record a tool execution.
    pub fn record_tool_call(
        &mut self,
        tool_name: &str,
        result_bytes: usize,
        execution_ms: u64,
        is_error: bool,
    ) {
        let stats = self.tool_usage.entry(tool_name.to_string()).or_default();
        stats.call_count += 1;
        if is_error {
            stats.error_count += 1;
        }
        // Running average for result bytes.
        let n = stats.call_count as f64;
        stats.avg_result_bytes = stats.avg_result_bytes * ((n - 1.0) / n) + result_bytes as f64 / n;
        stats.total_execution_ms += execution_ms;
    }

    /// Add a user instruction. Deduplicates exact matches.
    pub fn add_instruction(&mut self, instruction: impl Into<String>) {
        let instruction = instruction.into();
        if !self.user_instructions.contains(&instruction) {
            self.user_instructions.push(instruction);
        }
    }

    /// Generate the user instructions section to inject into the system prompt.
    pub fn instructions_prompt_section(&self) -> Option<String> {
        if self.user_instructions.is_empty() {
            return None;
        }
        let mut section = String::from(
            "\n\n## User Instructions\n\nThe user has provided the following behavioral instructions from prior sessions:\n",
        );
        for (i, instruction) in self.user_instructions.iter().enumerate() {
            section.push_str(&format!("{}. {}\n", i + 1, instruction));
        }
        Some(section)
    }

    /// Get the most frequently used tools (sorted by call count, descending).
    pub fn top_tools(&self, n: usize) -> Vec<(&str, &ToolUsageStats)> {
        let mut tools: Vec<_> = self
            .tool_usage
            .iter()
            .map(|(name, stats)| (name.as_str(), stats))
            .collect();
        tools.sort_by(|a, b| b.1.call_count.cmp(&a.1.call_count));
        tools.truncate(n);
        tools
    }

    /// Average success rate for a specific model across observations.
    pub fn model_success_rate(&self, model: &str) -> Option<f64> {
        let relevant: Vec<_> = self
            .model_observations
            .iter()
            .filter(|o| o.model == model)
            .collect();
        if relevant.is_empty() {
            return None;
        }
        let finished = relevant.iter().filter(|o| o.finished).count();
        Some(finished as f64 / relevant.len() as f64)
    }
}

/// Get the current time as ISO 8601 string (best-effort, no chrono dependency).
fn now_iso8601() -> String {
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("epoch:{epoch}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_profile_has_defaults() {
        let profile = AgentProfile::new("test-agent");
        assert_eq!(profile.agent_id, "test-agent");
        assert_eq!(profile.total_runs, 0);
        assert_eq!(profile.total_cost_usd, 0.0);
        assert!(profile.tool_usage.is_empty());
        assert!(profile.model_observations.is_empty());
        assert!(profile.user_instructions.is_empty());
    }

    #[test]
    fn record_run_accumulates() {
        let mut profile = AgentProfile::new("test");
        profile.record_run("model-a", 5, 1000, 500, true, 0.01);
        profile.record_run("model-a", 10, 2000, 1000, false, 0.02);

        assert_eq!(profile.total_runs, 2);
        assert!((profile.total_cost_usd - 0.03).abs() < 0.001);
        assert_eq!(profile.model_observations.len(), 2);
    }

    #[test]
    fn record_tool_call_tracks_stats() {
        let mut profile = AgentProfile::new("test");
        profile.record_tool_call("read_file", 1000, 50, false);
        profile.record_tool_call("read_file", 2000, 100, false);
        profile.record_tool_call("read_file", 500, 30, true);

        let stats = profile.tool_usage.get("read_file").unwrap();
        assert_eq!(stats.call_count, 3);
        assert_eq!(stats.error_count, 1);
        assert_eq!(stats.total_execution_ms, 180);
    }

    #[test]
    fn add_instruction_deduplicates() {
        let mut profile = AgentProfile::new("test");
        profile.add_instruction("be concise");
        profile.add_instruction("be concise");
        profile.add_instruction("use web search first");

        assert_eq!(profile.user_instructions.len(), 2);
    }

    #[test]
    fn instructions_prompt_section_none_when_empty() {
        let profile = AgentProfile::new("test");
        assert!(profile.instructions_prompt_section().is_none());
    }

    #[test]
    fn instructions_prompt_section_with_instructions() {
        let mut profile = AgentProfile::new("test");
        profile.add_instruction("be concise");
        let section = profile.instructions_prompt_section().unwrap();
        assert!(section.contains("be concise"));
        assert!(section.contains("User Instructions"));
    }

    #[test]
    fn top_tools_sorted() {
        let mut profile = AgentProfile::new("test");
        for _ in 0..5 {
            profile.record_tool_call("grep", 100, 10, false);
        }
        for _ in 0..10 {
            profile.record_tool_call("read_file", 200, 20, false);
        }
        for _ in 0..2 {
            profile.record_tool_call("shell", 50, 5, false);
        }

        let top = profile.top_tools(2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].0, "read_file");
        assert_eq!(top[1].0, "grep");
    }

    #[test]
    fn model_success_rate_computation() {
        let mut profile = AgentProfile::new("test");
        profile.record_run("model-a", 5, 100, 50, true, 0.01);
        profile.record_run("model-a", 10, 200, 100, false, 0.02);
        profile.record_run("model-a", 3, 100, 50, true, 0.01);

        let rate = profile.model_success_rate("model-a").unwrap();
        assert!((rate - 2.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn model_success_rate_none_for_unknown() {
        let profile = AgentProfile::new("test");
        assert!(profile.model_success_rate("unknown").is_none());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.json");

        let mut profile = AgentProfile::new("test-agent");
        profile.record_run("model-a", 5, 1000, 500, true, 0.01);
        profile.add_instruction("be helpful");
        profile.save(&path).unwrap();

        let loaded = AgentProfile::load_or_create(&path, "test-agent").unwrap();
        assert_eq!(loaded.agent_id, "test-agent");
        assert_eq!(loaded.total_runs, 1);
        assert_eq!(loaded.user_instructions.len(), 1);
    }

    #[test]
    fn load_or_create_creates_new() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");

        let profile = AgentProfile::load_or_create(&path, "new-agent").unwrap();
        assert_eq!(profile.agent_id, "new-agent");
        assert_eq!(profile.total_runs, 0);
    }

    #[test]
    fn observations_bounded_to_100() {
        let mut profile = AgentProfile::new("test");
        for i in 0..110 {
            profile.record_run("model", i as u32, 100, 50, true, 0.001);
        }
        assert_eq!(profile.model_observations.len(), 100);
    }
}
