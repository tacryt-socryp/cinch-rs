//! Three-zone context layout for managing LLM conversation context.
//!
//! Structures every API request as three zones:
//! 1. **Pinned prefix** — system prompt, tool definitions, persistent rules, original task.
//!    Never modified. Serves as attention sink and prompt cache anchor.
//! 2. **Compressed history** — running summary of completed work. Updated incrementally
//!    during compaction cycles. Information-dense but lossy.
//! 3. **Raw recency window** — last N messages, unmodified. Full fidelity. Exploits
//!    the recency bias of LLMs.
//!
//! Based on convergent architecture across Claude Code, Manus, OpenHands, and SWE-agent.
//! See: StreamingLLM (attention sinks), "Lost in the Middle" (TACL 2024).

use crate::Message;
use std::collections::VecDeque;

/// Default number of recent messages to keep in the raw recency window.
const DEFAULT_KEEP_RECENT: usize = 10;

/// Default token threshold to trigger compaction (80% of context window).
const DEFAULT_T_MAX_FRACTION: f64 = 0.80;

/// Default target tokens after compaction (60% of context window).
const DEFAULT_T_RETAINED_FRACTION: f64 = 0.60;

/// Three-zone context layout manager.
///
/// Manages the assembly of messages for API requests, maintaining three
/// distinct zones with different retention policies.
#[derive(Debug)]
pub struct ContextLayout {
    /// Pinned prefix: system prompt + persistent rules + original user task.
    /// Assembled once, never modified.
    prefix: Vec<Message>,

    /// Compressed history: running summary of completed work.
    /// Replaced atomically on each compaction cycle.
    compressed_history: Option<String>,

    /// Raw recency window: most recent messages, unmodified.
    recency_window: VecDeque<Message>,

    /// Middle zone: messages between prefix and recency window that haven't
    /// been compacted yet.
    middle: Vec<Message>,

    /// Number of recent messages to keep in the raw recency window.
    keep_recent: usize,

    /// Token threshold to trigger compaction.
    t_max: usize,

    /// Target tokens after compaction.
    t_retained: usize,

    /// Characters per token ratio for estimation.
    chars_per_token: f64,

    /// Number of compaction cycles performed (for cache-aware spacing).
    compaction_count: usize,

    /// Minimum rounds between compaction events (cache-aware: compact
    /// infrequently to minimize cache invalidation).
    min_rounds_between_compaction: usize,

    /// Round at which last compaction occurred.
    last_compaction_round: usize,
}

impl ContextLayout {
    /// Create a new context layout with the given context window size.
    pub fn new(context_window_tokens: usize) -> Self {
        Self {
            prefix: Vec::new(),
            compressed_history: None,
            compaction_count: 0,
            min_rounds_between_compaction: 10,
            last_compaction_round: 0,
            recency_window: VecDeque::new(),
            middle: Vec::new(),
            keep_recent: DEFAULT_KEEP_RECENT,
            t_max: (context_window_tokens as f64 * DEFAULT_T_MAX_FRACTION) as usize,
            t_retained: (context_window_tokens as f64 * DEFAULT_T_RETAINED_FRACTION) as usize,
            chars_per_token: crate::context::DEFAULT_CHARS_PER_TOKEN,
        }
    }

    /// Set the number of recent messages to keep in the raw recency window.
    pub fn with_keep_recent(mut self, n: usize) -> Self {
        self.keep_recent = n;
        self
    }

    /// Set custom compaction thresholds.
    pub fn with_thresholds(mut self, t_max: usize, t_retained: usize) -> Self {
        self.t_max = t_max;
        self.t_retained = t_retained;
        self
    }

    /// Set the pinned prefix messages (system prompt, persistent rules, etc.).
    pub fn set_prefix(&mut self, messages: Vec<Message>) {
        self.prefix = messages;
    }

    /// Add a message to the conversation. The layout automatically manages
    /// which zone the message ends up in.
    pub fn push_message(&mut self, msg: Message) {
        self.recency_window.push_back(msg);

        // If the recency window exceeds the keep_recent limit, move the
        // oldest message to the middle zone.
        while self.recency_window.len() > self.keep_recent {
            if let Some(old) = self.recency_window.pop_front() {
                self.middle.push(old);
            }
        }
    }

    /// Build the complete message list for an API request.
    pub fn to_messages(&self) -> Vec<Message> {
        let mut msgs = self.prefix.clone();

        // Insert compressed history as a system message if present.
        if let Some(ref summary) = self.compressed_history {
            msgs.push(Message::user(format!(
                "<context_summary>\n{summary}\n</context_summary>"
            )));
            msgs.push(Message::assistant_text(
                "I've reviewed the context summary and will continue from where I left off.",
            ));
        }

        // Add middle zone messages (not yet compacted).
        msgs.extend(self.middle.iter().cloned());

        // Add recency window messages.
        msgs.extend(self.recency_window.iter().cloned());

        msgs
    }

    /// Estimate total tokens across all zones.
    pub fn estimate_tokens(&self) -> usize {
        let total_chars: usize = self
            .to_messages()
            .iter()
            .map(|m| m.content.as_ref().map_or(0, |c| c.len()))
            .sum();
        (total_chars as f64 / self.chars_per_token) as usize
    }

    /// Set minimum rounds between compaction events (cache-aware: compact
    /// infrequently to amortize cache rebuild cost over many cache-hit rounds).
    pub fn with_min_rounds_between_compaction(mut self, rounds: usize) -> Self {
        self.min_rounds_between_compaction = rounds;
        self
    }

    /// Check if compaction is needed (total tokens exceed T_max) and
    /// cache-aware spacing is respected.
    pub fn needs_compaction(&self) -> bool {
        self.estimate_tokens() > self.t_max
    }

    /// Check if compaction should proceed, respecting cache-aware spacing.
    /// Even if needs_compaction() is true, we may want to defer if a recent
    /// compaction just occurred.
    pub fn should_compact(&self, current_round: usize) -> bool {
        self.needs_compaction()
            && (current_round - self.last_compaction_round) >= self.min_rounds_between_compaction
    }

    /// Get the compaction thresholds.
    pub fn thresholds(&self) -> (usize, usize) {
        (self.t_max, self.t_retained)
    }

    /// Get the messages in the middle zone that should be compacted.
    /// These are messages between the prefix and the recency window.
    pub fn compactable_messages(&self) -> &[Message] {
        &self.middle
    }

    /// Replace the middle zone with a compressed summary.
    /// Called after an external summarization step produces a summary string.
    /// `current_round` is used for cache-aware compaction spacing.
    pub fn apply_compaction(&mut self, summary: String, current_round: usize) {
        // Merge with existing compressed history if present.
        self.compressed_history = Some(match self.compressed_history.take() {
            Some(existing) => format!("{existing}\n\n{summary}"),
            None => summary,
        });

        // Clear the middle zone — it's been compressed into the summary.
        self.middle.clear();
        self.compaction_count += 1;
        self.last_compaction_round = current_round;
    }

    /// Number of compaction cycles performed.
    pub fn compaction_count(&self) -> usize {
        self.compaction_count
    }

    /// Get the current compressed history summary.
    pub fn compressed_history(&self) -> Option<&str> {
        self.compressed_history.as_deref()
    }

    /// Number of messages in the recency window.
    pub fn recency_window_len(&self) -> usize {
        self.recency_window.len()
    }

    /// Number of messages in the middle zone.
    pub fn middle_len(&self) -> usize {
        self.middle.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_layout_empty() {
        let layout = ContextLayout::new(200_000);
        assert_eq!(layout.recency_window_len(), 0);
        assert_eq!(layout.middle_len(), 0);
        assert!(layout.compressed_history().is_none());
    }

    #[test]
    fn messages_stay_in_recency_window() {
        let mut layout = ContextLayout::new(200_000).with_keep_recent(5);
        for i in 0..5 {
            layout.push_message(Message::user(&format!("msg {i}")));
        }
        assert_eq!(layout.recency_window_len(), 5);
        assert_eq!(layout.middle_len(), 0);
    }

    #[test]
    fn overflow_moves_to_middle() {
        let mut layout = ContextLayout::new(200_000).with_keep_recent(3);
        for i in 0..6 {
            layout.push_message(Message::user(&format!("msg {i}")));
        }
        assert_eq!(layout.recency_window_len(), 3);
        assert_eq!(layout.middle_len(), 3);
    }

    #[test]
    fn to_messages_includes_all_zones() {
        let mut layout = ContextLayout::new(200_000).with_keep_recent(2);
        layout.set_prefix(vec![Message::system("system prompt")]);

        layout.push_message(Message::user("old 1"));
        layout.push_message(Message::user("old 2"));
        layout.push_message(Message::user("recent 1"));
        layout.push_message(Message::user("recent 2"));

        let msgs = layout.to_messages();
        // prefix (1) + middle (2) + recency (2) = 5
        assert_eq!(msgs.len(), 5);
        assert_eq!(msgs[0].content.as_deref(), Some("system prompt"));
    }

    #[test]
    fn apply_compaction_replaces_middle() {
        let mut layout = ContextLayout::new(200_000).with_keep_recent(2);
        layout.set_prefix(vec![Message::system("sys")]);

        for i in 0..6 {
            layout.push_message(Message::user(&format!("msg {i}")));
        }

        assert_eq!(layout.middle_len(), 4);
        layout.apply_compaction("Summary of messages 0-3.".into(), 5);
        assert_eq!(layout.middle_len(), 0);
        assert!(layout.compressed_history().unwrap().contains("Summary"));

        // to_messages now includes: prefix + summary pair + recency
        let msgs = layout.to_messages();
        // prefix (1) + summary user msg + assistant ack + recency (2) = 5
        assert_eq!(msgs.len(), 5);
    }

    #[test]
    fn incremental_compaction_merges_summaries() {
        let mut layout = ContextLayout::new(200_000).with_keep_recent(2);

        layout.apply_compaction("First summary.".into(), 5);
        layout.apply_compaction("Second summary.".into(), 15);

        let history = layout.compressed_history().unwrap();
        assert!(history.contains("First summary."));
        assert!(history.contains("Second summary."));
    }
}
