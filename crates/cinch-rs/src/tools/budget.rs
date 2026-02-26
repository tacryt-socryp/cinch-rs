//! Tool definition budget management.
//!
//! Tool definitions are serialized into every API request but can consume
//! thousands of tokens — especially with many MCP tools. [`ToolBudget`]
//! estimates the token cost of tool definitions and [`enforce_budget`]
//! trims descriptions to fit within a budget.

use std::collections::HashSet;

/// Budget configuration for tool definitions.
#[derive(Debug, Clone)]
pub struct ToolBudget {
    /// Maximum tokens for all tool definitions combined. Default: 4000.
    pub max_tokens: usize,
    /// Tools whose descriptions should never be truncated.
    pub protected_tools: HashSet<String>,
    /// Chars-per-token ratio (reuses the same 3.5 default from ContextBudget).
    pub chars_per_token: f64,
}

impl Default for ToolBudget {
    fn default() -> Self {
        Self {
            max_tokens: 4000,
            protected_tools: HashSet::new(),
            chars_per_token: 3.5,
        }
    }
}

/// Report produced when tool definitions exceed the budget and are trimmed.
#[derive(Debug, Clone)]
pub struct BudgetReport {
    /// Estimated token count before trimming.
    pub original_tokens: usize,
    /// Estimated token count after trimming.
    pub trimmed_tokens: usize,
    /// Number of tool definitions whose descriptions were truncated.
    pub truncated_count: usize,
}

/// Structural overhead per tool definition (JSON wrapping, field names, etc.).
const STRUCTURAL_OVERHEAD_CHARS: usize = 20;

/// Minimum description length (in chars) below which we stop halving.
const MIN_DESC_CHARS: usize = 40;

/// Truncation marker appended to shortened descriptions.
const TRUNCATION_MARKER: &str = " [description truncated]";

/// Estimate the token cost of a single tool definition.
pub fn estimate_tokens(def: &crate::ToolDef, chars_per_token: f64) -> usize {
    let name_len = def.function.name.len();
    let desc_len = def.function.description.len();
    let params_len = serde_json::to_string(&def.function.parameters)
        .map(|s| s.len())
        .unwrap_or(0);
    let total_chars = name_len + desc_len + params_len + STRUCTURAL_OVERHEAD_CHARS;
    (total_chars as f64 / chars_per_token).ceil() as usize
}

/// Estimate the total token cost of all tool definitions.
pub fn estimate_total_tokens(defs: &[crate::ToolDef], chars_per_token: f64) -> usize {
    defs.iter()
        .map(|d| estimate_tokens(d, chars_per_token))
        .sum()
}

/// Enforce a token budget on tool definitions.
///
/// If the total estimated tokens are within `budget.max_tokens`, returns the
/// definitions unchanged with `None` report. Otherwise, iteratively truncates
/// unprotected tool descriptions (longest first) until the budget fits.
///
/// Protected tools (listed in `budget.protected_tools`) keep their full
/// descriptions regardless of budget pressure.
pub fn enforce_budget(
    defs: &[crate::ToolDef],
    budget: &ToolBudget,
) -> (Vec<crate::ToolDef>, Option<BudgetReport>) {
    let original_tokens = estimate_total_tokens(defs, budget.chars_per_token);

    if original_tokens <= budget.max_tokens {
        return (defs.to_vec(), None);
    }

    let mut result: Vec<crate::ToolDef> = defs.to_vec();

    // Collect indices of unprotected tools, sorted by description length (longest first).
    let mut unprotected: Vec<usize> = result
        .iter()
        .enumerate()
        .filter(|(_, d)| !budget.protected_tools.contains(&d.function.name))
        .map(|(i, _)| i)
        .collect();
    unprotected.sort_by(|&a, &b| {
        result[b]
            .function
            .description
            .len()
            .cmp(&result[a].function.description.len())
    });

    // Iteratively halve the max description length until budget fits or minimum reached.
    let max_unprotected_desc = unprotected
        .iter()
        .map(|&i| result[i].function.description.len())
        .max()
        .unwrap_or(0);

    let mut max_desc_chars = max_unprotected_desc;

    loop {
        max_desc_chars = (max_desc_chars / 2).max(MIN_DESC_CHARS);
        let mut truncated_count = 0;

        for &idx in &unprotected {
            let desc = &result[idx].function.description;
            if desc.len() > max_desc_chars && !desc.ends_with(TRUNCATION_MARKER) {
                let truncated = format!(
                    "{}{}",
                    &desc[..max_desc_chars.saturating_sub(TRUNCATION_MARKER.len())],
                    TRUNCATION_MARKER
                );
                result[idx].function.description = truncated;
                truncated_count += 1;
            } else if desc.ends_with(TRUNCATION_MARKER) && desc.len() > max_desc_chars {
                // Re-truncate an already-truncated description further.
                let content_len = max_desc_chars.saturating_sub(TRUNCATION_MARKER.len());
                let base = &result[idx].function.description
                    [..result[idx].function.description.floor_char_boundary(content_len)];
                result[idx].function.description = format!("{base}{TRUNCATION_MARKER}");
                truncated_count += 1;
            }
        }

        let current_tokens = estimate_total_tokens(&result, budget.chars_per_token);
        if current_tokens <= budget.max_tokens || max_desc_chars <= MIN_DESC_CHARS {
            let trimmed_tokens = current_tokens;
            return (
                result,
                Some(BudgetReport {
                    original_tokens,
                    trimmed_tokens,
                    truncated_count,
                }),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolDef;
    use serde_json::json;

    fn make_tool(name: &str, desc: &str) -> ToolDef {
        ToolDef::new(
            name,
            desc,
            json!({
                "type": "object",
                "properties": {
                    "input": { "type": "string" }
                }
            }),
        )
    }

    #[test]
    fn estimate_tokens_nonempty() {
        let tool = make_tool("read_file", "Read a file from disk.");
        let tokens = estimate_tokens(&tool, 3.5);
        assert!(tokens > 0, "token estimate should be positive");
    }

    #[test]
    fn estimate_total_matches_sum() {
        let tools = vec![
            make_tool("read_file", "Read a file from disk."),
            make_tool("write_file", "Write content to a file."),
            make_tool("grep", "Search file contents with regex."),
        ];
        let total = estimate_total_tokens(&tools, 3.5);
        let sum: usize = tools.iter().map(|t| estimate_tokens(t, 3.5)).sum();
        assert_eq!(total, sum);
    }

    #[test]
    fn enforce_budget_under_limit() {
        let tools = vec![make_tool("a", "short"), make_tool("b", "also short")];
        let budget = ToolBudget {
            max_tokens: 10_000,
            ..Default::default()
        };
        let (result, report) = enforce_budget(&tools, &budget);
        assert!(
            report.is_none(),
            "should not produce a report when under budget"
        );
        assert_eq!(result.len(), tools.len());
        assert_eq!(result[0].function.description, "short");
        assert_eq!(result[1].function.description, "also short");
    }

    #[test]
    fn enforce_budget_over_limit_truncates() {
        let long_desc = "A".repeat(2000);
        let tools = vec![
            make_tool("big_tool", &long_desc),
            make_tool("small_tool", "tiny"),
        ];
        let budget = ToolBudget {
            max_tokens: 50,
            protected_tools: HashSet::from(["small_tool".to_string()]),
            ..Default::default()
        };
        let (result, report) = enforce_budget(&tools, &budget);
        let report = report.expect("should produce a report when over budget");
        assert!(report.trimmed_tokens <= report.original_tokens);
        assert!(report.truncated_count > 0);
        // Small tool keeps its description.
        assert_eq!(result[1].function.description, "tiny");
        // Big tool was truncated.
        assert!(result[0].function.description.len() < long_desc.len());
    }

    #[test]
    fn protected_tools_not_truncated() {
        let long_desc = "B".repeat(2000);
        let tools = vec![make_tool("important", &long_desc)];
        let budget = ToolBudget {
            max_tokens: 10,
            protected_tools: HashSet::from(["important".to_string()]),
            ..Default::default()
        };
        let (result, report) = enforce_budget(&tools, &budget);
        // Even though over budget, the protected tool keeps its full description.
        assert_eq!(result[0].function.description, long_desc);
        // A report is still produced since original > budget.
        let report = report.expect("should produce a report");
        assert_eq!(report.truncated_count, 0);
    }

    #[test]
    fn truncation_marker_appended() {
        let long_desc = "C".repeat(2000);
        let tools = vec![
            make_tool("tool_a", &long_desc),
            make_tool("tool_b", "small"),
        ];
        let budget = ToolBudget {
            max_tokens: 50,
            ..Default::default()
        };
        let (result, _) = enforce_budget(&tools, &budget);
        assert!(
            result[0]
                .function
                .description
                .ends_with("[description truncated]"),
            "truncated description should end with marker, got: {}",
            result[0].function.description,
        );
    }
}
