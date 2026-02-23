//! Anchored incremental summarization for context compaction.
//!
//! Maintains a running summary updated incrementally — never re-summarizes
//! the whole history. When tool result eviction alone isn't enough, summarizes
//! the evicted span and merges it with the existing running summary in a single
//! cheap LLM call. Based on Factory.ai's dual-threshold mechanism.

use crate::Message;

/// The prompt used for summarization. Instructs the model to produce a concise,
/// factual summary suitable for injecting into a conversation as context.
const SUMMARIZATION_PROMPT: &str = "\
Summarize the following conversation messages concisely. Focus on:
- What was accomplished (completed subtasks, files modified)
- Key findings and decisions made
- Failed approaches (what was tried and why it failed)
- File paths and function names mentioned
- Current plan state and what remains to be done

Rules:
- Only include facts explicitly stated in the messages. Do not infer or extrapolate.
- Preserve file paths, function names, and error messages verbatim.
- Be concise — every token must earn its place.
- If there is an existing summary, merge the new information into it to produce a single \
  cohesive summary. Do not simply append — integrate, deduplicate, and update. The result \
  must be a standalone summary that replaces the existing one entirely.";

/// Configuration for incremental summarization.
#[derive(Debug, Clone)]
pub struct SummarizerConfig {
    /// Model to use for summarization (cheaper than the main model).
    pub model: Option<String>,
    /// Maximum tokens for the summarization response.
    pub max_summary_tokens: u32,
    /// Minimum token reduction required for compaction to be considered
    /// successful. If compaction doesn't reduce by at least this fraction,
    /// trigger a fallback (hard truncation).
    pub min_reduction_fraction: f64,
}

impl Default for SummarizerConfig {
    fn default() -> Self {
        Self {
            model: None, // Use main model if not specified.
            max_summary_tokens: 2048,
            min_reduction_fraction: 0.20,
        }
    }
}

/// State for the incremental summarizer.
#[derive(Debug)]
pub struct Summarizer {
    /// The running summary of all completed work.
    pub summary: Option<String>,
    /// Index of the last compaction boundary in the message list.
    pub boundary_index: usize,
    /// Configuration.
    pub config: SummarizerConfig,
}

impl Summarizer {
    pub fn new(config: SummarizerConfig) -> Self {
        Self {
            summary: None,
            boundary_index: 0,
            config,
        }
    }

    /// Build the summarization prompt for a span of messages.
    ///
    /// Returns a (system, user) message pair suitable for a one-shot LLM call.
    pub fn build_summarization_request(&self, span: &[Message]) -> (String, String) {
        let mut content = String::new();

        // Include existing summary for merge context.
        if let Some(ref existing) = self.summary {
            content.push_str("=== EXISTING SUMMARY ===\n");
            content.push_str(existing);
            content.push_str("\n\n=== NEW MESSAGES TO SUMMARIZE ===\n");
        }

        // Format the span of messages — include full content so the
        // summarizer has maximum context to work with.
        for msg in span {
            let role = &msg.role;
            let text = msg.content.as_deref().unwrap_or("[no content]");
            content.push_str(&format!("[{role}]: {text}\n\n"));
        }

        (SUMMARIZATION_PROMPT.to_string(), content)
    }

    /// Record a new summary and advance the boundary.
    pub fn apply_summary(&mut self, new_summary: String, new_boundary: usize) {
        self.summary = Some(new_summary);
        self.boundary_index = new_boundary;
    }

    /// Get the model to use for summarization.
    pub fn summary_model<'a>(&'a self, main_model: &'a str) -> &'a str {
        self.config.model.as_deref().unwrap_or(main_model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_request_without_existing_summary() {
        let summarizer = Summarizer::new(SummarizerConfig::default());
        let messages = vec![
            Message::user("Read file src/main.rs"),
            Message::tool_result("c1", "fn main() { ... }"),
        ];

        let (system, user) = summarizer.build_summarization_request(&messages);
        assert!(system.contains("Summarize"));
        assert!(user.contains("Read file src/main.rs"));
        assert!(!user.contains("EXISTING SUMMARY"));
    }

    #[test]
    fn build_request_with_existing_summary() {
        let mut summarizer = Summarizer::new(SummarizerConfig::default());
        summarizer.summary = Some("Previously: read main.rs and found entry point.".into());

        let messages = vec![Message::user("Now read lib.rs")];
        let (_, user) = summarizer.build_summarization_request(&messages);
        assert!(user.contains("EXISTING SUMMARY"));
        assert!(user.contains("Previously:"));
    }

    #[test]
    fn apply_summary_updates_state() {
        let mut summarizer = Summarizer::new(SummarizerConfig::default());
        assert!(summarizer.summary.is_none());
        assert_eq!(summarizer.boundary_index, 0);

        summarizer.apply_summary("First summary.".into(), 5);
        assert_eq!(summarizer.summary.as_deref(), Some("First summary."));
        assert_eq!(summarizer.boundary_index, 5);
    }

    #[test]
    fn preserves_full_content_in_request() {
        let summarizer = Summarizer::new(SummarizerConfig::default());
        let long_content = "x".repeat(5000);
        let messages = vec![Message::tool_result("c1", long_content.clone())];

        let (_, user) = summarizer.build_summarization_request(&messages);
        assert!(user.contains(&long_content));
        assert!(!user.contains("[truncated"));
    }
}
