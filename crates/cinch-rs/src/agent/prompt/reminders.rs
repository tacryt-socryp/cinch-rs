//! System reminders injected mid-conversation before each API call.
//!
//! Unlike static prompt sections, reminders are injected as transient user
//! messages at specific points during the conversation. They don't persist
//! across compaction and are not part of the pinned prefix.
//!
//! Use cases:
//! - Context usage warnings ("Context at 75%, consider wrapping up")
//! - Memory nudges ("Check MEMORY.md for past observations")
//! - Task tracking ("You have 3 pending TODO items")
//! - Tool guidance ("Prefer grep over shell('grep ...') for file search")

/// Context available when evaluating reminder conditions.
#[derive(Debug, Clone)]
pub struct RoundContext {
    /// Current round number (1-indexed).
    pub round: u32,
    /// Maximum rounds configured.
    pub max_rounds: u32,
    /// Estimated context usage as a fraction (0.0–1.0+).
    pub context_usage_pct: f64,
    /// Number of tool calls executed so far.
    pub total_tool_calls: usize,
    /// The model being used for this round.
    pub model: String,
}

/// How often a reminder should fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReminderFrequency {
    /// Fire every round.
    EveryRound,
    /// Fire every N rounds.
    EveryNRounds(u32),
    /// Fire only once (on the first round where the condition is true).
    Once,
}

/// A system reminder that can be injected before API calls.
pub struct SystemReminder {
    /// Human-readable name for logging/debugging.
    pub name: String,
    /// When to check this reminder.
    pub frequency: ReminderFrequency,
    /// Condition: returns true if the reminder should fire this round.
    pub condition: Box<dyn Fn(&RoundContext) -> bool + Send + Sync>,
    /// Content generator: returns the reminder text.
    pub content: Box<dyn Fn(&RoundContext) -> String + Send + Sync>,
    /// Whether this reminder has already fired (for `Once` frequency).
    fired: bool,
}

impl std::fmt::Debug for SystemReminder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemReminder")
            .field("name", &self.name)
            .field("frequency", &self.frequency)
            .field("fired", &self.fired)
            .finish()
    }
}

impl SystemReminder {
    /// Create a new reminder.
    pub fn new(
        name: impl Into<String>,
        frequency: ReminderFrequency,
        condition: impl Fn(&RoundContext) -> bool + Send + Sync + 'static,
        content: impl Fn(&RoundContext) -> String + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            frequency,
            condition: Box::new(condition),
            content: Box::new(content),
            fired: false,
        }
    }

    /// Check if this reminder should fire for the given round context.
    fn should_fire(&self, ctx: &RoundContext) -> bool {
        if self.fired && self.frequency == ReminderFrequency::Once {
            return false;
        }

        match self.frequency {
            ReminderFrequency::EveryRound => (self.condition)(ctx),
            ReminderFrequency::EveryNRounds(n) => {
                ctx.round.is_multiple_of(n) && (self.condition)(ctx)
            }
            ReminderFrequency::Once => (self.condition)(ctx),
        }
    }

    /// Mark this reminder as having fired.
    fn mark_fired(&mut self) {
        self.fired = true;
    }
}

/// Registry of system reminders.
///
/// Manages a collection of reminders and evaluates them each round to produce
/// messages that should be injected before the API call.
///
/// # Example
///
/// ```ignore
/// let mut reminders = ReminderRegistry::new();
///
/// reminders.add(SystemReminder::new(
///     "context-warning",
///     ReminderFrequency::EveryRound,
///     |ctx| ctx.context_usage_pct > 0.75,
///     |ctx| format!(
///         "[System reminder: context at {:.0}%. Prioritize completing your current task.]",
///         ctx.context_usage_pct * 100.0
///     ),
/// ));
///
/// let messages = reminders.collect_reminders(&round_ctx);
/// ```
#[derive(Debug)]
pub struct ReminderRegistry {
    reminders: Vec<SystemReminder>,
}

impl ReminderRegistry {
    /// Create an empty reminder registry.
    pub fn new() -> Self {
        Self {
            reminders: Vec::new(),
        }
    }

    /// Create a registry with default reminders for common use cases.
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        register_default_reminders(&mut registry);
        registry
    }

    /// Add a reminder to the registry.
    pub fn add(&mut self, reminder: SystemReminder) {
        self.reminders.push(reminder);
    }

    /// Remove a reminder by name.
    pub fn remove(&mut self, name: &str) {
        self.reminders.retain(|r| r.name != name);
    }

    /// Evaluate all reminders for the current round and return the messages
    /// that should be injected.
    ///
    /// Returns a `Vec<String>` of reminder texts. Each should be injected as
    /// a system-level user message before the API call.
    pub fn collect_reminders(&mut self, ctx: &RoundContext) -> Vec<String> {
        let mut messages = Vec::new();

        for reminder in &mut self.reminders {
            if reminder.should_fire(ctx) {
                let content = (reminder.content)(ctx);
                if !content.is_empty() {
                    messages.push(content);
                }
                reminder.mark_fired();
            }
        }

        messages
    }

    /// Number of registered reminders.
    pub fn len(&self) -> usize {
        self.reminders.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.reminders.is_empty()
    }
}

impl Default for ReminderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Register the built-in default reminders.
fn register_default_reminders(registry: &mut ReminderRegistry) {
    // Context usage warning at 60%.
    registry.add(SystemReminder::new(
        "context-warning-60",
        ReminderFrequency::EveryNRounds(3),
        |ctx| ctx.context_usage_pct >= 0.60 && ctx.context_usage_pct < 0.80,
        |ctx| {
            format!(
                "[System reminder: context usage at ~{:.0}%. \
                 Prioritize drafting over additional research. \
                 Wrap up tool calls and save your progress soon.]",
                ctx.context_usage_pct * 100.0
            )
        },
    ));

    // Context usage critical at 80%.
    registry.add(SystemReminder::new(
        "context-warning-80",
        ReminderFrequency::EveryRound,
        |ctx| ctx.context_usage_pct >= 0.80,
        |ctx| {
            format!(
                "[System reminder: context usage CRITICAL at ~{:.0}%. \
                 Save drafts NOW. Do not call additional research tools.]",
                ctx.context_usage_pct * 100.0
            )
        },
    ));

    // Approaching round limit.
    registry.add(SystemReminder::new(
        "round-limit-warning",
        ReminderFrequency::Once,
        |ctx| {
            ctx.max_rounds > 0
                && ctx.round >= ctx.max_rounds.saturating_sub(2)
                && ctx.round < ctx.max_rounds
        },
        |ctx| {
            format!(
                "[System reminder: approaching round limit ({}/{}). \
                 Wrap up your current task and produce final output.]",
                ctx.round, ctx.max_rounds
            )
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ctx(round: u32, usage: f64) -> RoundContext {
        RoundContext {
            round,
            max_rounds: 10,
            context_usage_pct: usage,
            total_tool_calls: 0,
            model: "test-model".into(),
        }
    }

    #[test]
    fn empty_registry_no_reminders() {
        let mut registry = ReminderRegistry::new();
        let msgs = registry.collect_reminders(&make_ctx(1, 0.1));
        assert!(msgs.is_empty());
    }

    #[test]
    fn reminder_fires_when_condition_met() {
        let mut registry = ReminderRegistry::new();
        registry.add(SystemReminder::new(
            "test",
            ReminderFrequency::EveryRound,
            |ctx| ctx.context_usage_pct > 0.5,
            |_| "Warning!".into(),
        ));

        let msgs = registry.collect_reminders(&make_ctx(1, 0.3));
        assert!(msgs.is_empty());

        let msgs = registry.collect_reminders(&make_ctx(2, 0.7));
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0], "Warning!");
    }

    #[test]
    fn once_reminder_fires_only_once() {
        let mut registry = ReminderRegistry::new();
        registry.add(SystemReminder::new(
            "once",
            ReminderFrequency::Once,
            |_| true,
            |_| "First time!".into(),
        ));

        let msgs = registry.collect_reminders(&make_ctx(1, 0.0));
        assert_eq!(msgs.len(), 1);

        let msgs = registry.collect_reminders(&make_ctx(2, 0.0));
        assert!(msgs.is_empty());
    }

    #[test]
    fn every_n_rounds_fires_at_intervals() {
        let mut registry = ReminderRegistry::new();
        registry.add(SystemReminder::new(
            "periodic",
            ReminderFrequency::EveryNRounds(3),
            |_| true,
            |ctx| format!("Round {}", ctx.round),
        ));

        // Round 1, 2: no fire (not divisible by 3)
        assert!(registry.collect_reminders(&make_ctx(1, 0.0)).is_empty());
        assert!(registry.collect_reminders(&make_ctx(2, 0.0)).is_empty());

        // Round 3: fires
        let msgs = registry.collect_reminders(&make_ctx(3, 0.0));
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0], "Round 3");

        // Round 4, 5: no fire
        assert!(registry.collect_reminders(&make_ctx(4, 0.0)).is_empty());
        assert!(registry.collect_reminders(&make_ctx(5, 0.0)).is_empty());

        // Round 6: fires
        assert_eq!(registry.collect_reminders(&make_ctx(6, 0.0)).len(), 1);
    }

    #[test]
    fn default_reminders_include_context_warnings() {
        let mut registry = ReminderRegistry::with_defaults();
        assert!(registry.len() >= 2);

        // Low usage: no reminders
        let msgs = registry.collect_reminders(&make_ctx(3, 0.1));
        assert!(msgs.is_empty());

        // High usage: context warning fires
        let msgs = registry.collect_reminders(&make_ctx(3, 0.65));
        assert!(!msgs.is_empty());
        assert!(msgs[0].contains("context usage"));
    }

    #[test]
    fn remove_by_name() {
        let mut registry = ReminderRegistry::new();
        registry.add(SystemReminder::new(
            "removable",
            ReminderFrequency::EveryRound,
            |_| true,
            |_| "content".into(),
        ));
        assert_eq!(registry.len(), 1);

        registry.remove("removable");
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn empty_content_not_returned() {
        let mut registry = ReminderRegistry::new();
        registry.add(SystemReminder::new(
            "empty",
            ReminderFrequency::EveryRound,
            |_| true,
            |_| String::new(),
        ));

        let msgs = registry.collect_reminders(&make_ctx(1, 0.0));
        assert!(msgs.is_empty());
    }
}
