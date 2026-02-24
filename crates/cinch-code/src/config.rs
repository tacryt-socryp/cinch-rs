//! Coding-agent configuration with sensible defaults.
//!
//! [`CodeConfig`] captures the settings a coding agent typically needs and
//! converts them into cinch-rs types via [`build_harness_config`](CodeConfig::build_harness_config)
//! and [`build_tool_set`](CodeConfig::build_tool_set).

use cinch_rs::agent::config::HarnessConfig;
use cinch_rs::tools::core::ToolSet;

use crate::prompt::coding_system_prompt;
use crate::tools::GitToolsExt;

/// Configuration for a coding agent session.
///
/// Provides coding-tuned defaults (higher round limit, lower temperature,
/// streaming enabled) and convenience methods to produce a [`HarnessConfig`]
/// and [`ToolSet`] ready for use with the cinch-rs harness.
#[derive(Debug, Clone)]
pub struct CodeConfig {
    /// Model identifier. Default: `"anthropic/claude-sonnet-4"`.
    pub model: String,
    /// Maximum tool-use round-trips. Default: `50`.
    pub max_rounds: u32,
    /// Maximum tokens per LLM response. Default: `16384`.
    pub max_tokens: u32,
    /// Sampling temperature. Default: `0.3`.
    pub temperature: f32,
    /// Working directory for file/git tools. Default: `"."`.
    pub workdir: String,
    /// Enable streaming for LLM responses. Default: `true`.
    pub streaming: bool,
}

impl Default for CodeConfig {
    fn default() -> Self {
        Self {
            model: "anthropic/claude-sonnet-4".to_string(),
            max_rounds: 50,
            max_tokens: 16384,
            temperature: 0.3,
            workdir: ".".to_string(),
            streaming: true,
        }
    }
}

impl CodeConfig {
    /// Build a [`HarnessConfig`] from this coding config.
    ///
    /// Sets sequential tool execution, the coding system prompt, and
    /// coding-tuned defaults for rounds, tokens, and temperature.
    pub fn build_harness_config(&self) -> HarnessConfig {
        HarnessConfig::new(self.model.clone(), coding_system_prompt())
            .with_max_rounds(self.max_rounds)
            .with_max_tokens(self.max_tokens)
            .with_temperature(self.temperature)
            .with_streaming(self.streaming)
    }

    /// Build a [`ToolSet`] with common filesystem tools and git tools.
    pub fn build_tool_set(&self) -> ToolSet {
        ToolSet::new()
            .with_common_tools(&self.workdir)
            .with_git_tools(&self.workdir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_coding_tuned() {
        let config = CodeConfig::default();
        assert_eq!(config.max_rounds, 50);
        assert_eq!(config.max_tokens, 16384);
        assert!((config.temperature - 0.3).abs() < f32::EPSILON);
        assert!(config.streaming);
    }

    #[test]
    fn build_harness_config_uses_coding_defaults() {
        let config = CodeConfig::default();
        let harness = config.build_harness_config();
        assert_eq!(harness.max_rounds, 50);
        assert_eq!(harness.max_tokens, 16384);
        assert!(harness.streaming);
        assert!(harness.system_prompt.is_some());
    }

    #[test]
    fn build_tool_set_includes_git_tools() {
        let config = CodeConfig::default();
        let tools = config.build_tool_set();
        let defs = tools.definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
        assert!(names.contains(&"git_status"));
        assert!(names.contains(&"git_diff"));
        assert!(names.contains(&"git_log"));
        assert!(names.contains(&"git_commit"));
        // Also has common tools
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"shell"));
    }
}
