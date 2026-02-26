//! Tool result eviction: replace oldest tool results with one-line placeholders.
//!
//! Tool results are the single largest context consumer in any agent loop.
//! A `read_file` can inject 30KB; a `grep` can return hundreds of lines.
//! Most of this is irrelevant after the model has processed it. This module
//! replaces old tool result content with compact placeholders, freeing context
//! without any LLM call. The full result still exists in the environment.
//!
//! Highest-ROI context management technique: no LLM call needed, typically
//! recovers 10-100x more tokens than model reasoning occupies.

use crate::Message;
use crate::context::layout::message_tokens;
use std::cmp::Ordering;
use std::collections::HashSet;

/// Prefix used for evicted tool result placeholders.
///
/// Both the placeholder writer and the "already evicted?" check reference
/// this constant so they can't drift out of sync.
pub const EVICTED_PREFIX: &str = "[Cleared:";

/// Configuration for tool result eviction.
#[derive(Debug, Clone)]
pub struct EvictionConfig {
    /// Tools whose results should never be evicted (e.g., tools with ephemeral output).
    pub protected_tools: HashSet<String>,
    /// Minimum age (in rounds) before a tool result can be evicted.
    pub min_age_rounds: usize,
    /// Characters per token ratio for estimation.
    pub chars_per_token: f64,
}

impl Default for EvictionConfig {
    fn default() -> Self {
        Self {
            protected_tools: HashSet::new(),
            min_age_rounds: 3,
            chars_per_token: crate::context::DEFAULT_CHARS_PER_TOKEN,
        }
    }
}

impl EvictionConfig {
    /// Create a new eviction config with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a protected tool name (its results will never be evicted).
    pub fn protect_tool(mut self, name: impl Into<String>) -> Self {
        self.protected_tools.insert(name.into());
        self
    }

    /// Set the minimum age before eviction.
    pub fn with_min_age(mut self, rounds: usize) -> Self {
        self.min_age_rounds = rounds;
        self
    }
}

/// Metadata tracked alongside each tool result for eviction purposes.
#[derive(Debug, Clone)]
pub struct ToolResultMeta {
    /// The tool name that produced this result.
    pub tool_name: String,
    /// A short summary of the arguments (for the placeholder).
    pub args_summary: String,
    /// The round in which this tool was called.
    pub round: usize,
    /// Index in the message list.
    pub message_index: usize,
    /// Approximate character count of the original result.
    pub char_count: usize,
    /// Estimated token count of the original result.
    pub estimated_tokens: usize,
}

/// Compute eviction priority for a tool result. Higher score = evict first.
///
/// Considers three factors:
/// - **Age**: older results are more likely stale.
/// - **Size**: larger results free more context per eviction (log-scaled to
///   prevent huge results from dominating).
/// - **Tool type**: read-only tools (read_file, grep, find_files, list_dir)
///   get a 1.5x multiplier since their results can always be re-read from the
///   environment, unlike mutation tool results.
pub fn eviction_priority(meta: &ToolResultMeta, current_round: usize) -> f64 {
    let age = (current_round.saturating_sub(meta.round)).max(1) as f64;
    let size_factor = (meta.estimated_tokens.max(1) as f64).ln();
    let tool_factor = match meta.tool_name.as_str() {
        crate::tools::names::READ_FILE
        | crate::tools::names::GREP
        | crate::tools::names::FIND_FILES
        | crate::tools::names::LIST_DIR => 1.5,
        _ => 1.0,
    };
    age * size_factor * tool_factor
}

/// Evict tool results from a message list by priority, replacing them with placeholders.
///
/// Candidates are sorted by [`eviction_priority()`] (highest first): large,
/// old, read-only results are evicted before small, recent, or mutation results.
/// Stops when the estimated total tokens drops below `target_tokens`.
///
/// Returns the number of characters freed.
pub fn evict_tool_results(
    messages: &mut [Message],
    tool_metas: &[ToolResultMeta],
    current_round: usize,
    target_tokens: usize,
    config: &EvictionConfig,
) -> usize {
    let mut freed = 0;

    // Sort candidates by priority (highest = evict first).
    let mut candidates: Vec<&ToolResultMeta> = tool_metas
        .iter()
        .filter(|m| {
            !config.protected_tools.contains(&m.tool_name)
                && current_round.saturating_sub(m.round) >= config.min_age_rounds
        })
        .collect();
    candidates.sort_by(|a, b| {
        eviction_priority(b, current_round)
            .partial_cmp(&eviction_priority(a, current_round))
            .unwrap_or(Ordering::Equal)
    });

    for meta in candidates {
        // Check if we've freed enough.
        let current_tokens = estimate_tokens_for_messages(messages, config.chars_per_token);
        if current_tokens <= target_tokens {
            break;
        }

        if let Some(msg) = messages.get_mut(meta.message_index)
            && let Some(ref content) = msg.content
        {
            // Only evict if the content hasn't already been evicted.
            if content.starts_with(EVICTED_PREFIX) {
                continue;
            }

            let placeholder = format!(
                "[Cleared: {}({}) — {} chars, round {}]",
                meta.tool_name, meta.args_summary, meta.char_count, meta.round,
            );

            let old_len = content.len();
            let new_len = placeholder.len();
            freed += old_len.saturating_sub(new_len);

            msg.content = Some(placeholder);
        }
    }

    freed
}

/// Estimate total tokens for a slice of messages.
fn estimate_tokens_for_messages(messages: &[Message], chars_per_token: f64) -> usize {
    messages
        .iter()
        .map(|m| message_tokens(m, chars_per_token))
        .sum()
}

/// Extract a short argument summary from raw JSON arguments for use in placeholders.
#[allow(clippy::string_slice)] // all indices from floor_char_boundary
pub fn summarize_args(arguments: &str, max_len: usize) -> String {
    // Try to parse and extract key fields.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(arguments)
        && let Some(obj) = v.as_object()
    {
        let parts: Vec<String> = obj
            .iter()
            .take(3)
            .map(|(k, v)| {
                let val = match v {
                    serde_json::Value::String(s) => {
                        if s.len() > 40 {
                            let end = s.floor_char_boundary(37);
                            format!("\"{}...\"", &s[..end])
                        } else {
                            format!("\"{s}\"")
                        }
                    }
                    other => {
                        let s = other.to_string();
                        if s.len() > 40 {
                            let end = s.floor_char_boundary(37);
                            format!("{}...", &s[..end])
                        } else {
                            s
                        }
                    }
                };
                format!("{k}={val}")
            })
            .collect();
        let summary = parts.join(", ");
        if summary.len() > max_len {
            let end = summary.floor_char_boundary(max_len.saturating_sub(3));
            return format!("{}...", &summary[..end]);
        }
        return summary;
    }

    // Fallback: truncate raw arguments.
    if arguments.len() > max_len {
        let end = arguments.floor_char_boundary(max_len.saturating_sub(3));
        format!("{}...", &arguments[..end])
    } else {
        arguments.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool_msg(call_id: &str, content: &str) -> Message {
        Message::tool_result(call_id, content)
    }

    #[test]
    fn evict_oldest_tool_results() {
        let mut messages = vec![
            Message::system("system"),
            Message::user("task"),
            make_tool_msg("c1", &"a".repeat(10000)),
            make_tool_msg("c2", &"b".repeat(10000)),
            make_tool_msg("c3", &"c".repeat(10000)),
        ];

        let metas = vec![
            ToolResultMeta {
                tool_name: "read_file".into(),
                args_summary: "path=\"src/main.rs\"".into(),
                round: 1,
                message_index: 2,
                char_count: 10000,
                estimated_tokens: 2857,
            },
            ToolResultMeta {
                tool_name: "grep".into(),
                args_summary: "pattern=\"TODO\"".into(),
                round: 2,
                message_index: 3,
                char_count: 10000,
                estimated_tokens: 2857,
            },
            ToolResultMeta {
                tool_name: "read_file".into(),
                args_summary: "path=\"src/lib.rs\"".into(),
                round: 3,
                message_index: 4,
                char_count: 10000,
                estimated_tokens: 2857,
            },
        ];

        let config = EvictionConfig::new().with_min_age(1);
        let freed = evict_tool_results(&mut messages, &metas, 5, 1000, &config);

        assert!(freed > 0);
        assert!(
            messages[2]
                .content
                .as_ref()
                .unwrap()
                .starts_with(EVICTED_PREFIX)
        );
        assert!(
            messages[3]
                .content
                .as_ref()
                .unwrap()
                .starts_with(EVICTED_PREFIX)
        );
    }

    #[test]
    fn protected_tools_not_evicted() {
        let mut messages = vec![make_tool_msg("c1", &"a".repeat(10000))];

        let metas = vec![ToolResultMeta {
            tool_name: "save_draft".into(),
            args_summary: "".into(),
            round: 1,
            message_index: 0,
            char_count: 10000,
            estimated_tokens: 2857,
        }];

        let config = EvictionConfig::new()
            .with_min_age(0)
            .protect_tool("save_draft");
        let freed = evict_tool_results(&mut messages, &metas, 5, 0, &config);

        assert_eq!(freed, 0);
        assert!(
            !messages[0]
                .content
                .as_ref()
                .unwrap()
                .starts_with(EVICTED_PREFIX)
        );
    }

    #[test]
    fn recent_results_not_evicted() {
        let mut messages = vec![make_tool_msg("c1", &"a".repeat(10000))];

        let metas = vec![ToolResultMeta {
            tool_name: "read_file".into(),
            args_summary: "".into(),
            round: 4,
            message_index: 0,
            char_count: 10000,
            estimated_tokens: 2857,
        }];

        let config = EvictionConfig::new().with_min_age(3);
        let freed = evict_tool_results(&mut messages, &metas, 5, 0, &config);

        assert_eq!(freed, 0);
    }

    #[test]
    fn priority_prefers_large_read_only_over_small_recent() {
        // Large read_file from round 2 should have higher priority than
        // small shell result from round 1 (even though shell is older).
        let large_read = ToolResultMeta {
            tool_name: "read_file".into(),
            args_summary: "path=\"big.rs\"".into(),
            round: 2,
            message_index: 0,
            char_count: 30_000,
            estimated_tokens: 8572,
        };
        let small_shell = ToolResultMeta {
            tool_name: "shell".into(),
            args_summary: "cmd=\"echo ok\"".into(),
            round: 1,
            message_index: 1,
            char_count: 50,
            estimated_tokens: 15,
        };

        let current_round = 5;
        let p_read = eviction_priority(&large_read, current_round);
        let p_shell = eviction_priority(&small_shell, current_round);

        assert!(
            p_read > p_shell,
            "Large read_file (priority={p_read}) should be evicted before small shell (priority={p_shell})"
        );
    }

    #[test]
    fn eviction_order_respects_priority() {
        let mut messages = vec![
            Message::system("system"),
            Message::user("task"),
            make_tool_msg("c1", &"x".repeat(50)), // small, round 1
            make_tool_msg("c2", &"y".repeat(30_000)), // large read_file, round 2
            make_tool_msg("c3", &"z".repeat(500)), // medium, round 3
        ];

        let metas = vec![
            ToolResultMeta {
                tool_name: "shell".into(),
                args_summary: "".into(),
                round: 1,
                message_index: 2,
                char_count: 50,
                estimated_tokens: 15,
            },
            ToolResultMeta {
                tool_name: "read_file".into(),
                args_summary: "path=\"big.rs\"".into(),
                round: 2,
                message_index: 3,
                char_count: 30_000,
                estimated_tokens: 8572,
            },
            ToolResultMeta {
                tool_name: "grep".into(),
                args_summary: "pattern=\"TODO\"".into(),
                round: 3,
                message_index: 4,
                char_count: 500,
                estimated_tokens: 143,
            },
        ];

        // Set target very low so only the highest-priority result gets evicted
        // before we hit the target.
        let config = EvictionConfig::new().with_min_age(1);
        let freed = evict_tool_results(&mut messages, &metas, 5, 1000, &config);
        assert!(freed > 0);

        // The large read_file (index 3) should be evicted first due to
        // highest priority (large size + read-only tool multiplier).
        assert!(
            messages[3]
                .content
                .as_ref()
                .unwrap()
                .starts_with(EVICTED_PREFIX),
            "Large read_file should be evicted first"
        );
    }

    #[test]
    fn summarize_args_json() {
        let args = r#"{"path": "src/main.rs", "encoding": "utf-8"}"#;
        let summary = summarize_args(args, 100);
        assert!(summary.contains("path="));
        assert!(summary.contains("src/main.rs"));
    }

    #[test]
    fn summarize_args_truncates_long_values() {
        let args = format!(r#"{{"query": "{}"}}"#, "x".repeat(100));
        let summary = summarize_args(&args, 100);
        assert!(summary.contains("..."));
    }
}
