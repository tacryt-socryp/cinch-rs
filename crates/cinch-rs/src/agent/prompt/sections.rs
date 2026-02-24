//! Named prompt sections with conditions and stability tags.
//!
//! The [`PromptRegistry`] stores sections that are conditionally included in the
//! system prompt based on a [`TurnContext`]. Sections are tagged as [`Stable`]
//! (cache-friendly, rarely changes) or [`Dynamic`] (varies per turn). The
//! registry assembles all active sections with stable sections first, maximizing
//! prompt cache hits.

use super::builder::SystemPromptBuilder;
use std::collections::HashMap;

/// Context available when evaluating section conditions and content.
///
/// Passed to condition and content functions so they can decide whether to
/// include a section and what content to produce.
#[derive(Debug, Clone)]
pub struct TurnContext {
    /// The model being used for this turn.
    pub model: String,
    /// Current round number (0-indexed).
    pub round: u32,
    /// Maximum rounds configured.
    pub max_rounds: u32,
    /// Estimated context usage as a fraction (0.0–1.0+).
    pub context_usage_pct: f64,
    /// Whether plan-execute mode is enabled.
    pub plan_execute_enabled: bool,
    /// Current phase (planning or executing).
    pub is_planning_phase: bool,
    /// Arbitrary key-value metadata for custom conditions.
    pub metadata: HashMap<String, String>,
}

impl Default for TurnContext {
    fn default() -> Self {
        Self {
            model: String::new(),
            round: 0,
            max_rounds: 10,
            context_usage_pct: 0.0,
            plan_execute_enabled: false,
            is_planning_phase: false,
            metadata: HashMap::new(),
        }
    }
}

/// Whether a section is stable (cache-friendly) or dynamic (per-turn).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stability {
    /// Stable sections rarely change and are placed first in the prompt
    /// to maximize prompt cache hits.
    Stable,
    /// Dynamic sections may change every turn and are placed after stable
    /// sections.
    Dynamic,
}

/// A named prompt section with a condition and content generator.
pub struct PromptSection {
    /// Unique name for this section (used for ordering and dedup).
    pub name: String,
    /// Section heading (e.g., "Tool Guidance"). Empty string = raw content.
    pub heading: String,
    /// Whether this section is stable (cache-friendly) or dynamic.
    pub stability: Stability,
    /// Priority within its stability group (lower = earlier). Default: 100.
    pub priority: u32,
    /// Condition: returns true if this section should be included.
    pub condition: Box<dyn Fn(&TurnContext) -> bool + Send + Sync>,
    /// Content generator: returns the section body.
    pub content: Box<dyn Fn(&TurnContext) -> String + Send + Sync>,
}

impl std::fmt::Debug for PromptSection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PromptSection")
            .field("name", &self.name)
            .field("heading", &self.heading)
            .field("stability", &self.stability)
            .field("priority", &self.priority)
            .finish()
    }
}

/// Registry of named prompt sections with conditional loading.
///
/// Separates stable (cache-friendly) and dynamic content. When assembled,
/// stable sections come first to maximize prompt cache hits.
///
/// # Example
///
/// ```ignore
/// let mut registry = PromptRegistry::new("You are a helpful coding agent.");
///
/// registry.register_stable("Tool Guidance", 10, |_ctx| true, |_ctx| {
///     "Use grep for searching, read_file for reading.".into()
/// });
///
/// registry.register_dynamic("Context Warning", 50,
///     |ctx| ctx.context_usage_pct > 0.6,
///     |ctx| format!("Context at {:.0}%. Wrap up soon.", ctx.context_usage_pct * 100.0),
/// );
///
/// let prompt = registry.assemble(&TurnContext::default());
/// ```
#[derive(Debug)]
pub struct PromptRegistry {
    /// The preamble (always included first, before any sections).
    preamble: String,
    /// Registered sections, keyed by name.
    sections: Vec<PromptSection>,
}

impl PromptRegistry {
    /// Create a new registry with a preamble.
    pub fn new(preamble: impl Into<String>) -> Self {
        Self {
            preamble: preamble.into(),
            sections: Vec::new(),
        }
    }

    /// Register a stable (cache-friendly) section.
    ///
    /// Stable sections are placed before dynamic sections in the assembled
    /// prompt. They should produce the same output across turns to maximize
    /// prompt cache hits.
    pub fn register_stable(
        &mut self,
        heading: impl Into<String>,
        priority: u32,
        condition: impl Fn(&TurnContext) -> bool + Send + Sync + 'static,
        content: impl Fn(&TurnContext) -> String + Send + Sync + 'static,
    ) {
        let heading = heading.into();
        let name = heading.clone();
        self.sections.push(PromptSection {
            name,
            heading,
            stability: Stability::Stable,
            priority,
            condition: Box::new(condition),
            content: Box::new(content),
        });
    }

    /// Register a dynamic (per-turn) section.
    ///
    /// Dynamic sections are placed after all stable sections. Their content
    /// may change every turn.
    pub fn register_dynamic(
        &mut self,
        heading: impl Into<String>,
        priority: u32,
        condition: impl Fn(&TurnContext) -> bool + Send + Sync + 'static,
        content: impl Fn(&TurnContext) -> String + Send + Sync + 'static,
    ) {
        let heading = heading.into();
        let name = heading.clone();
        self.sections.push(PromptSection {
            name,
            heading,
            stability: Stability::Dynamic,
            priority,
            condition: Box::new(condition),
            content: Box::new(content),
        });
    }

    /// Register a section with a custom name different from the heading.
    pub fn register(
        &mut self,
        name: impl Into<String>,
        heading: impl Into<String>,
        stability: Stability,
        priority: u32,
        condition: impl Fn(&TurnContext) -> bool + Send + Sync + 'static,
        content: impl Fn(&TurnContext) -> String + Send + Sync + 'static,
    ) {
        self.sections.push(PromptSection {
            name: name.into(),
            heading: heading.into(),
            stability,
            priority,
            condition: Box::new(condition),
            content: Box::new(content),
        });
    }

    /// Remove a section by name.
    pub fn remove(&mut self, name: &str) {
        self.sections.retain(|s| s.name != name);
    }

    /// Assemble the full system prompt for the given turn context.
    ///
    /// Sections are ordered: stable (by priority) then dynamic (by priority).
    /// Sections whose condition returns `false` are skipped.
    pub fn assemble(&self, ctx: &TurnContext) -> String {
        let mut builder = SystemPromptBuilder::new(&self.preamble);

        // Collect and sort active sections: stable first, then dynamic.
        let mut active: Vec<&PromptSection> = self
            .sections
            .iter()
            .filter(|s| (s.condition)(ctx))
            .collect();

        active.sort_by(|a, b| {
            a.stability
                .cmp_order()
                .cmp(&b.stability.cmp_order())
                .then(a.priority.cmp(&b.priority))
        });

        for section in active {
            let content = (section.content)(ctx);
            if content.is_empty() {
                continue;
            }
            if section.heading.is_empty() {
                builder = builder.raw(content);
            } else {
                builder = builder.section(&section.heading, content);
            }
        }

        builder.build()
    }

    /// Number of registered sections.
    pub fn section_count(&self) -> usize {
        self.sections.len()
    }

    /// Check if a section with the given name exists.
    pub fn has_section(&self, name: &str) -> bool {
        self.sections.iter().any(|s| s.name == name)
    }
}

impl Stability {
    /// Sort order: stable (0) before dynamic (1).
    fn cmp_order(self) -> u8 {
        match self {
            Self::Stable => 0,
            Self::Dynamic => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_returns_preamble_only() {
        let registry = PromptRegistry::new("You are a helper.");
        let prompt = registry.assemble(&TurnContext::default());
        assert_eq!(prompt, "You are a helper.");
    }

    #[test]
    fn stable_sections_come_before_dynamic() {
        let mut registry = PromptRegistry::new("Preamble");

        registry.register_dynamic("Dynamic First", 10, |_| true, |_| "dynamic".into());
        registry.register_stable("Stable Second", 10, |_| true, |_| "stable".into());

        let prompt = registry.assemble(&TurnContext::default());
        let stable_pos = prompt.find("stable").unwrap();
        let dynamic_pos = prompt.find("dynamic").unwrap();
        assert!(
            stable_pos < dynamic_pos,
            "Stable sections should come before dynamic"
        );
    }

    #[test]
    fn priority_ordering_within_group() {
        let mut registry = PromptRegistry::new("P");

        registry.register_stable("Second", 20, |_| true, |_| "second".into());
        registry.register_stable("First", 10, |_| true, |_| "first".into());

        let prompt = registry.assemble(&TurnContext::default());
        let first_pos = prompt.find("first").unwrap();
        let second_pos = prompt.find("second").unwrap();
        assert!(first_pos < second_pos);
    }

    #[test]
    fn condition_false_excludes_section() {
        let mut registry = PromptRegistry::new("P");

        registry.register_stable("Visible", 10, |_| true, |_| "visible".into());
        registry.register_stable("Hidden", 20, |_| false, |_| "hidden".into());

        let prompt = registry.assemble(&TurnContext::default());
        assert!(prompt.contains("visible"));
        assert!(!prompt.contains("hidden"));
    }

    #[test]
    fn condition_uses_turn_context() {
        let mut registry = PromptRegistry::new("P");

        registry.register_dynamic(
            "Context Warning",
            10,
            |ctx| ctx.context_usage_pct > 0.6,
            |ctx| format!("Usage at {:.0}%", ctx.context_usage_pct * 100.0),
        );

        let low_ctx = TurnContext {
            context_usage_pct: 0.3,
            ..Default::default()
        };
        let high_ctx = TurnContext {
            context_usage_pct: 0.8,
            ..Default::default()
        };

        assert!(!registry.assemble(&low_ctx).contains("Usage"));
        assert!(registry.assemble(&high_ctx).contains("Usage at 80%"));
    }

    #[test]
    fn remove_section_by_name() {
        let mut registry = PromptRegistry::new("P");
        registry.register_stable("ToRemove", 10, |_| true, |_| "content".into());
        assert!(registry.has_section("ToRemove"));

        registry.remove("ToRemove");
        assert!(!registry.has_section("ToRemove"));
    }

    #[test]
    fn empty_content_skipped() {
        let mut registry = PromptRegistry::new("P");
        registry.register_stable("Empty", 10, |_| true, |_| String::new());
        let prompt = registry.assemble(&TurnContext::default());
        assert!(!prompt.contains("Empty"));
    }

    #[test]
    fn raw_section_no_heading() {
        let mut registry = PromptRegistry::new("P");
        registry.register(
            "raw-block",
            "",
            Stability::Stable,
            10,
            |_| true,
            |_| "---\nRaw content here".into(),
        );
        let prompt = registry.assemble(&TurnContext::default());
        assert!(prompt.contains("---\nRaw content here"));
        assert!(!prompt.contains("## ")); // no heading added for raw
    }

    #[test]
    fn section_count() {
        let mut registry = PromptRegistry::new("P");
        assert_eq!(registry.section_count(), 0);
        registry.register_stable("A", 10, |_| true, |_| "a".into());
        registry.register_dynamic("B", 10, |_| true, |_| "b".into());
        assert_eq!(registry.section_count(), 2);
    }
}
