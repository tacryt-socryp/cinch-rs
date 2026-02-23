//! Correlation IDs and cost tracking for agent runs.
//!
//! Assigns a unique `trace_id` to each harness run and a `span_id` to each
//! round within it. Tracks cumulative token usage and estimated cost.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

/// Generate a unique trace ID for an agent run.
pub fn generate_trace_id() -> String {
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // Use a counter to handle sub-nanosecond calls.
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("tr-{ts:x}-{count:04x}")
}

/// Generate a span ID for a round within a run.
pub fn generate_span_id(trace_id: &str, round: u32) -> String {
    format!("{trace_id}:r{round}")
}

/// Per-model pricing for cost estimation (USD per 1M tokens).
#[derive(Debug, Clone)]
pub struct ModelPricing {
    /// Price per 1M input tokens.
    pub input_per_million: f64,
    /// Price per 1M output tokens.
    pub output_per_million: f64,
}

impl ModelPricing {
    /// Estimate cost for given token counts.
    pub fn estimate_cost(&self, prompt_tokens: u32, completion_tokens: u32) -> f64 {
        (prompt_tokens as f64 / 1_000_000.0) * self.input_per_million
            + (completion_tokens as f64 / 1_000_000.0) * self.output_per_million
    }
}

impl Default for ModelPricing {
    fn default() -> Self {
        // Default to a mid-range estimate.
        Self {
            input_per_million: 3.0,
            output_per_million: 15.0,
        }
    }
}

/// Lookup approximate pricing for a model by name.
///
/// Matches on the model name segment (after the last `/` in paths like
/// `"anthropic/claude-sonnet-4"`) to avoid false positives from org
/// prefixes like `"my-org/custom-sonnet-finetune"`.
pub fn pricing_for_model(model: &str) -> ModelPricing {
    // Extract the model name after the last `/` (e.g. "claude-sonnet-4"
    // from "anthropic/claude-sonnet-4"). Fall back to the full string
    // for bare model names.
    let name = model.rsplit('/').next().unwrap_or(model).to_lowercase();

    // Approximate pricing as of early 2026. These don't need to be exact —
    // cost tracking is for detecting runaway loops, not billing.
    if name.contains("opus") {
        ModelPricing {
            input_per_million: 15.0,
            output_per_million: 75.0,
        }
    } else if name.contains("sonnet") {
        ModelPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
        }
    } else if name.contains("haiku") {
        ModelPricing {
            input_per_million: 0.25,
            output_per_million: 1.25,
        }
    } else if name.contains("gpt-4o-mini") || name.contains("4o-mini") {
        ModelPricing {
            input_per_million: 0.15,
            output_per_million: 0.60,
        }
    } else if name.contains("gpt-4o") || name.contains("gpt-4") {
        ModelPricing {
            input_per_million: 2.50,
            output_per_million: 10.0,
        }
    } else if name.starts_with("o1") || name.starts_with("o3") {
        ModelPricing {
            input_per_million: 15.0,
            output_per_million: 60.0,
        }
    } else if name.contains("gemini") && name.contains("flash") {
        ModelPricing {
            input_per_million: 0.075,
            output_per_million: 0.30,
        }
    } else if name.contains("gemini") {
        ModelPricing {
            input_per_million: 1.25,
            output_per_million: 5.0,
        }
    } else if name.contains("deepseek") {
        ModelPricing {
            input_per_million: 0.27,
            output_per_million: 1.10,
        }
    } else {
        ModelPricing::default()
    }
}

/// Cumulative cost tracker for a harness run (including sub-agents).
#[derive(Debug, Default)]
pub struct CostTracker {
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub estimated_cost_usd: f64,
}

impl CostTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record token usage for a round.
    pub fn record(&mut self, prompt_tokens: u32, completion_tokens: u32, pricing: &ModelPricing) {
        self.total_prompt_tokens += prompt_tokens as u64;
        self.total_completion_tokens += completion_tokens as u64;
        self.estimated_cost_usd += pricing.estimate_cost(prompt_tokens, completion_tokens);
    }

    /// Total tokens consumed.
    pub fn total_tokens(&self) -> u64 {
        self.total_prompt_tokens + self.total_completion_tokens
    }

    /// Format as a short summary string.
    pub fn summary(&self) -> String {
        format!(
            "tokens: {} prompt + {} completion = {} total, est. cost: ${:.4}",
            self.total_prompt_tokens,
            self.total_completion_tokens,
            self.total_tokens(),
            self.estimated_cost_usd,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_id_unique() {
        let id1 = generate_trace_id();
        let id2 = generate_trace_id();
        assert_ne!(id1, id2);
        assert!(id1.starts_with("tr-"));
    }

    #[test]
    fn span_id_includes_round() {
        let trace = "tr-abc123-0000";
        let span = generate_span_id(trace, 3);
        assert!(span.contains("r3"));
        assert!(span.starts_with(trace));
    }

    #[test]
    fn cost_estimation() {
        let pricing = ModelPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
        };
        let cost = pricing.estimate_cost(1_000_000, 100_000);
        assert!((cost - 4.5).abs() < 0.01); // 3.0 + 1.5
    }

    #[test]
    fn cost_tracker_accumulates() {
        let mut tracker = CostTracker::new();
        let pricing = ModelPricing::default();
        tracker.record(1000, 500, &pricing);
        tracker.record(2000, 1000, &pricing);
        assert_eq!(tracker.total_prompt_tokens, 3000);
        assert_eq!(tracker.total_completion_tokens, 1500);
        assert!(tracker.estimated_cost_usd > 0.0);
    }

    #[test]
    fn pricing_lookup_known_models() {
        let opus = pricing_for_model("anthropic/claude-opus-4");
        assert!(opus.input_per_million > 10.0);

        let haiku = pricing_for_model("anthropic/claude-3.5-haiku");
        assert!(haiku.input_per_million < 1.0);

        let unknown = pricing_for_model("some-unknown-model");
        assert!(unknown.input_per_million > 0.0);
    }

    #[test]
    fn cost_summary_format() {
        let mut tracker = CostTracker::new();
        tracker.record(1000, 500, &ModelPricing::default());
        let summary = tracker.summary();
        assert!(summary.contains("tokens:"));
        assert!(summary.contains("cost:"));
    }
}
