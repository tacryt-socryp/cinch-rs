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

    /// Add multiple messages in batch. Each message is pushed through the
    /// normal zone management logic.
    pub fn push_messages(&mut self, msgs: impl IntoIterator<Item = Message>) {
        for msg in msgs {
            self.push_message(msg);
        }
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
        // Replace compressed history with the new summary. The summarizer
        // already receives the existing summary as context (via
        // build_summarization_request), so the returned summary is a merged
        // result that replaces the old one entirely.
        self.compressed_history = Some(summary);

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

    /// Total number of messages across all zones (excluding compressed history).
    pub fn total_message_count(&self) -> usize {
        self.prefix.len() + self.middle.len() + self.recency_window.len()
    }

    /// Get the messages in the middle zone (alias for `compactable_messages`).
    pub fn middle_messages(&self) -> &[Message] {
        &self.middle
    }

    /// Get mutable references to all non-prefix messages (middle + recency) in order.
    ///
    /// Used by eviction to modify tool result content in-place. Each entry is
    /// a `(global_index, &mut Message)` pair where `global_index` is the
    /// position in the flat `to_messages()` output (accounting for prefix and
    /// compressed history messages).
    pub fn flat_messages_mut(&mut self) -> Vec<(usize, &mut Message)> {
        let prefix_len = self.prefix.len();
        let history_msgs = if self.compressed_history.is_some() { 2 } else { 0 };
        let offset = prefix_len + history_msgs;
        let middle_len = self.middle.len();

        let mut result: Vec<(usize, &mut Message)> = Vec::new();
        for (i, msg) in self.middle.iter_mut().enumerate() {
            result.push((offset + i, msg));
        }
        for (i, msg) in self.recency_window.iter_mut().enumerate() {
            result.push((offset + middle_len + i, msg));
        }
        result
    }

    /// Return the index that the next pushed message would occupy in
    /// `to_messages()` output. Used for tracking message positions for eviction.
    pub fn next_message_index(&self) -> usize {
        self.prefix.len()
            + if self.compressed_history.is_some() { 2 } else { 0 }
            + self.middle.len()
            + self.recency_window.len()
    }

    /// Get a mutable reference to a message by its position in the
    /// `to_messages()` output. Returns `None` for prefix and synthetic
    /// compressed history messages.
    pub fn message_at_mut(&mut self, index: usize) -> Option<&mut Message> {
        let prefix_len = self.prefix.len();
        if index < prefix_len {
            return None; // prefix is immutable
        }
        let index = index - prefix_len;

        let history_len = if self.compressed_history.is_some() { 2 } else { 0 };
        if index < history_len {
            return None; // synthetic compressed history messages
        }
        let index = index - history_len;

        if index < self.middle.len() {
            return self.middle.get_mut(index);
        }
        let index = index - self.middle.len();

        self.recency_window.get_mut(index)
    }

    /// Get the keep_recent setting.
    pub fn keep_recent(&self) -> usize {
        self.keep_recent
    }

    /// Estimate tokens for a slice of messages.
    fn estimate_tokens_for(messages: &[Message], chars_per_token: f64) -> usize {
        let total_chars: usize = messages
            .iter()
            .map(|m| m.content.as_ref().map_or(0, |c| c.len()))
            .sum();
        (total_chars as f64 / chars_per_token) as usize
    }

    /// Compute a per-zone breakdown of estimated token usage.
    pub fn breakdown(&self) -> ContextBreakdown {
        let prefix_tokens = Self::estimate_tokens_for(&self.prefix, self.chars_per_token);

        let compressed_history_tokens = self
            .compressed_history
            .as_ref()
            .map(|s| {
                // Account for the wrapping <context_summary> tags and assistant ack message
                let summary_msg_chars = format!("<context_summary>\n{s}\n</context_summary>").len();
                let ack_chars = "I've reviewed the context summary and will continue from where I left off.".len();
                ((summary_msg_chars + ack_chars) as f64 / self.chars_per_token) as usize
            })
            .unwrap_or(0);

        let middle_tokens = Self::estimate_tokens_for(&self.middle, self.chars_per_token);

        let recency_msgs: Vec<&Message> = self.recency_window.iter().collect();
        let recency_tokens = {
            let total_chars: usize = recency_msgs
                .iter()
                .map(|m| m.content.as_ref().map_or(0, |c| c.len()))
                .sum();
            (total_chars as f64 / self.chars_per_token) as usize
        };

        let total_tokens = prefix_tokens + compressed_history_tokens + middle_tokens + recency_tokens;

        ContextBreakdown {
            prefix_tokens,
            compressed_history_tokens,
            middle_tokens,
            recency_tokens,
            total_tokens,
        }
    }
}

/// Per-zone breakdown of estimated context token usage.
#[derive(Debug, Clone)]
pub struct ContextBreakdown {
    /// Estimated tokens in the pinned prefix zone.
    pub prefix_tokens: usize,
    /// Estimated tokens in the compressed history zone.
    pub compressed_history_tokens: usize,
    /// Estimated tokens in the middle zone (not yet compacted).
    pub middle_tokens: usize,
    /// Estimated tokens in the recency window.
    pub recency_tokens: usize,
    /// Total estimated tokens across all zones.
    pub total_tokens: usize,
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
    fn breakdown_reports_per_zone_tokens() {
        let mut layout = ContextLayout::new(200_000).with_keep_recent(2);
        layout.set_prefix(vec![Message::system("system prompt here")]);

        // Push messages so some end up in middle, some in recency
        for i in 0..4 {
            layout.push_message(Message::user(&format!("message {i}")));
        }

        let bd = layout.breakdown();
        assert!(bd.prefix_tokens > 0);
        assert!(bd.middle_tokens > 0);
        assert!(bd.recency_tokens > 0);
        assert_eq!(bd.compressed_history_tokens, 0);
        assert_eq!(
            bd.total_tokens,
            bd.prefix_tokens + bd.middle_tokens + bd.recency_tokens
        );
    }

    #[test]
    fn breakdown_includes_compressed_history() {
        let mut layout = ContextLayout::new(200_000).with_keep_recent(2);
        layout.set_prefix(vec![Message::system("sys")]);
        layout.apply_compaction("Summary of work done so far.".into(), 5);

        let bd = layout.breakdown();
        assert!(bd.compressed_history_tokens > 0);
    }

    #[test]
    fn incremental_compaction_replaces_summary() {
        let mut layout = ContextLayout::new(200_000).with_keep_recent(2);

        layout.apply_compaction("First summary.".into(), 5);
        assert_eq!(layout.compressed_history().unwrap(), "First summary.");

        // Second compaction replaces (not concatenates) the first.
        // The summarizer is responsible for merging old + new into the returned string.
        layout.apply_compaction("Merged summary covering both phases.".into(), 15);

        let history = layout.compressed_history().unwrap();
        assert_eq!(history, "Merged summary covering both phases.");
        // Verify the old summary is NOT present (no concatenation).
        assert!(!history.contains("First summary."));
    }
}
