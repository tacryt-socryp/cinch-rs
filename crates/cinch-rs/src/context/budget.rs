//! Context budget tracking: monitors cumulative message size during an
//! agent loop and injects warnings when approaching model limits.
//!
//! Helps prevent context overflow by tracking the approximate token budget
//! consumed by system prompt, user messages, and tool results. When usage
//! exceeds configurable thresholds, advisory notices are injected into tool
//! results to nudge the LLM toward completing its task rather than gathering
//! more data.

use crate::Message;

/// Default characters per token (conservative estimate for English text).
/// Most tokenizers average 3-4 chars per token; we use 3.5 as a middle ground.
pub const DEFAULT_CHARS_PER_TOKEN: f64 = 3.5;

/// Default context window size in tokens (Claude models via OpenRouter).
const DEFAULT_CONTEXT_WINDOW: usize = 200_000;

/// Threshold percentages at which to inject context notices.
const WARNING_THRESHOLD: f64 = 0.60;
const CRITICAL_THRESHOLD: f64 = 0.80;

/// Tracks context budget consumption across the agent loop.
///
/// Estimates total token usage from message character counts and injects
/// advisory notices when thresholds are crossed: a warning at 60% and a
/// critical notice at 80%. The [`Harness`](crate::agent::harness::Harness)
/// creates and manages a `ContextBudget` automatically; you only need to
/// construct one manually for standalone use.
///
/// # Example
///
/// ```ignore
/// let budget = ContextBudget::with_calibration("You are a helpful agent.", None)
///     .with_max_tokens(128_000)
///     .with_output_reserve(4096)
///     .with_warning_message("Wrap up your research soon.")
///     .with_critical_message("Save your output NOW.");
///
/// let usage = budget.estimate_usage(&messages);
/// println!("{}", usage.to_log_string());
///
/// if let Some(advisory) = budget.advisory(&messages) {
///     println!("Advisory: {advisory}");
/// }
/// ```
#[derive(Debug)]
pub struct ContextBudget {
    /// Maximum context window in tokens.
    max_tokens: usize,
    /// Tokens reserved for model output (per-response token limit).
    output_reserve: usize,
    /// Tokens reserved for system prompt overhead.
    system_reserve: usize,
    /// Size of the system prompt in characters.
    system_prompt_chars: usize,
    /// Characters per token ratio (calibrated or default).
    chars_per_token: f64,
    /// Warning message injected at the warning threshold.
    warning_message: Option<String>,
    /// Critical message injected at the critical threshold.
    critical_message: Option<String>,
}

impl ContextBudget {
    /// Create a new context budget tracker with a calibrated chars-per-token
    /// ratio from historical API usage data. Pass `None` to use the default.
    pub fn with_calibration(system_prompt: &str, calibrated_cpt: Option<f64>) -> Self {
        let cpt = calibrated_cpt.unwrap_or(DEFAULT_CHARS_PER_TOKEN);
        Self {
            max_tokens: DEFAULT_CONTEXT_WINDOW,
            output_reserve: 0,
            system_reserve: 0,
            system_prompt_chars: system_prompt.len(),
            chars_per_token: cpt,
            warning_message: None,
            critical_message: None,
        }
    }

    /// Override the context window size (in tokens).
    pub fn with_max_tokens(mut self, max: usize) -> Self {
        self.max_tokens = max;
        self
    }

    /// Set a custom warning message (injected at 60% usage).
    pub fn with_warning_message(mut self, msg: impl Into<String>) -> Self {
        self.warning_message = Some(msg.into());
        self
    }

    /// Set a custom critical message (injected at 80% usage).
    pub fn with_critical_message(mut self, msg: impl Into<String>) -> Self {
        self.critical_message = Some(msg.into());
        self
    }

    /// Set tokens reserved for model output (the per-response max_tokens limit).
    pub fn with_output_reserve(mut self, tokens: usize) -> Self {
        self.output_reserve = tokens;
        self
    }

    /// Set tokens reserved for system prompt overhead.
    pub fn with_system_reserve(mut self, tokens: usize) -> Self {
        self.system_reserve = tokens;
        self
    }

    /// Return the maximum context window size in tokens.
    pub fn max_tokens(&self) -> usize {
        self.max_tokens
    }

    /// Effective context window: max_tokens minus reserves for output and system prompt.
    ///
    /// All threshold calculations use this value instead of raw `max_tokens`
    /// to ensure the model always has room for its response.
    pub fn effective_max_tokens(&self) -> usize {
        self.max_tokens
            .saturating_sub(self.output_reserve)
            .saturating_sub(self.system_reserve)
    }

    /// Estimate the total tokens consumed by all messages.
    ///
    /// Usage percentage is computed against [`effective_max_tokens()`](Self::effective_max_tokens)
    /// rather than raw `max_tokens`, so thresholds account for output and system reserves.
    pub fn estimate_usage(&self, messages: &[Message]) -> ContextUsage {
        let mut total_chars = self.system_prompt_chars;

        for msg in messages {
            if let Some(ref content) = msg.content {
                total_chars += content.len();
            }
        }

        let estimated_tokens = (total_chars as f64 / self.chars_per_token) as usize;
        let effective = self.effective_max_tokens();
        let usage_pct = if effective > 0 {
            estimated_tokens as f64 / effective as f64
        } else {
            1.0
        };

        ContextUsage {
            estimated_tokens,
            max_tokens: self.max_tokens,
            usage_pct,
        }
    }

    /// Generate a context advisory notice if usage exceeds thresholds.
    ///
    /// Returns `None` if usage is within normal bounds.
    pub fn advisory(&self, messages: &[Message]) -> Option<String> {
        let usage = self.estimate_usage(messages);

        if usage.usage_pct >= CRITICAL_THRESHOLD {
            Some(self.critical_message.clone().unwrap_or_else(|| {
                format!(
                    "[Context notice: ~{:.0}% of context budget used ({} est. tokens / {} max). \
                     Prioritize saving drafts NOW. Do not call additional research tools.]",
                    usage.usage_pct * 100.0,
                    usage.estimated_tokens,
                    usage.max_tokens,
                )
            }))
        } else if usage.usage_pct >= WARNING_THRESHOLD {
            Some(self.warning_message.clone().unwrap_or_else(|| {
                format!(
                    "[Context notice: ~{:.0}% of context budget used. \
                     Prioritize drafting over additional research. \
                     Wrap up tool calls and save your draft soon.]",
                    usage.usage_pct * 100.0,
                )
            }))
        } else {
            None
        }
    }
}

/// Snapshot of context usage at a point in time.
#[derive(Debug)]
pub struct ContextUsage {
    /// Estimated tokens consumed.
    pub estimated_tokens: usize,
    /// Maximum context window.
    pub max_tokens: usize,
    /// Usage as a fraction (0.0 to 1.0+).
    pub usage_pct: f64,
}

impl ContextUsage {
    /// Format as a short log-friendly string.
    pub fn to_log_string(&self) -> String {
        format!(
            "context: ~{} tokens ({:.0}% of {})",
            self.estimated_tokens,
            self.usage_pct * 100.0,
            self.max_tokens,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_message(content: &str) -> Message {
        Message {
            role: crate::MessageRole::User,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    #[test]
    fn low_usage_no_advisory() {
        let budget = ContextBudget::with_calibration("short system prompt", None);
        let messages = vec![make_message("hello")];
        assert!(budget.advisory(&messages).is_none());
    }

    #[test]
    fn warning_threshold_triggers() {
        let large_prompt = "x".repeat(455_000);
        let budget = ContextBudget::with_calibration(&large_prompt, None);
        let messages = vec![];
        let advisory = budget.advisory(&messages);
        assert!(advisory.is_some());
        assert!(advisory.unwrap().contains("Prioritize drafting"));
    }

    #[test]
    fn critical_threshold_triggers() {
        let large_prompt = "x".repeat(595_000);
        let budget = ContextBudget::with_calibration(&large_prompt, None);
        let messages = vec![];
        let advisory = budget.advisory(&messages);
        assert!(advisory.is_some());
        assert!(advisory.unwrap().contains("Prioritize saving drafts NOW"));
    }

    #[test]
    fn custom_messages() {
        let large_prompt = "x".repeat(595_000);
        let budget =
            ContextBudget::with_calibration(&large_prompt, None).with_critical_message("STOP NOW");
        let advisory = budget.advisory(&[]);
        assert_eq!(advisory, Some("STOP NOW".into()));
    }

    #[test]
    fn usage_accumulates_across_messages() {
        let budget = ContextBudget::with_calibration("prompt", None);
        let messages = vec![
            make_message(&"a".repeat(100_000)),
            make_message(&"b".repeat(100_000)),
        ];
        let usage = budget.estimate_usage(&messages);
        assert!(usage.estimated_tokens > 50_000);
    }

    #[test]
    fn usage_log_string_format() {
        let budget = ContextBudget::with_calibration("test prompt", None);
        let messages = vec![make_message("hello world")];
        let usage = budget.estimate_usage(&messages);
        let log = usage.to_log_string();
        assert!(log.contains("context:"));
        assert!(log.contains("tokens"));
    }

    #[test]
    fn calibrated_budget_uses_custom_ratio() {
        let budget_default = ContextBudget::with_calibration("test", None);
        let budget_calibrated = ContextBudget::with_calibration("test", Some(4.0));
        let messages = vec![make_message(&"a".repeat(40_000))];

        let usage_default = budget_default.estimate_usage(&messages);
        let usage_calibrated = budget_calibrated.estimate_usage(&messages);

        assert!(usage_calibrated.estimated_tokens < usage_default.estimated_tokens);
    }

    #[test]
    fn with_max_tokens_override() {
        let budget = ContextBudget::with_calibration("test", None).with_max_tokens(100_000);
        assert_eq!(budget.max_tokens(), 100_000);
    }

    #[test]
    fn effective_max_tokens_subtracts_reserves() {
        let budget = ContextBudget::with_calibration("test", None)
            .with_max_tokens(200_000)
            .with_output_reserve(4096)
            .with_system_reserve(1000);
        assert_eq!(budget.effective_max_tokens(), 200_000 - 4096 - 1000);
    }

    #[test]
    fn effective_max_tokens_saturates_at_zero() {
        let budget = ContextBudget::with_calibration("test", None)
            .with_max_tokens(1000)
            .with_output_reserve(800)
            .with_system_reserve(500);
        assert_eq!(budget.effective_max_tokens(), 0);
    }

    #[test]
    fn output_reserve_makes_thresholds_trigger_earlier() {
        // With output_reserve, effective window is smaller, so same content
        // hits thresholds at lower absolute token counts.
        let budget_no_reserve = ContextBudget::with_calibration("sys", None)
            .with_max_tokens(100_000);
        let budget_with_reserve = ContextBudget::with_calibration("sys", None)
            .with_max_tokens(100_000)
            .with_output_reserve(20_000);

        let messages = vec![make_message(&"a".repeat(200_000))];
        let usage_no = budget_no_reserve.estimate_usage(&messages);
        let usage_with = budget_with_reserve.estimate_usage(&messages);

        // Same estimated tokens, but higher usage_pct with reserve.
        assert_eq!(usage_no.estimated_tokens, usage_with.estimated_tokens);
        assert!(usage_with.usage_pct > usage_no.usage_pct);
    }
}
