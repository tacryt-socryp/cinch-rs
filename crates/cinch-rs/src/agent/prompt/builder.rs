//! Structured system prompt builder.
//!
//! [`SystemPromptBuilder`] provides a builder-pattern API for assembling
//! multi-section system prompts with conditional sections, optional sections,
//! and raw text blocks. This replaces manual string concatenation with a
//! structured, composable approach.

/// Builder for multi-section system prompts.
///
/// Sections are joined with double newlines. Empty sections (from `section_if`
/// with a false condition, or `section_opt` with `None`) are silently skipped.
///
/// # Example
///
/// ```
/// use cinch_rs::agent::prompt::SystemPromptBuilder;
///
/// let prompt = SystemPromptBuilder::new("You are a helpful agent.")
///     .section("Context", "Today is Monday.")
///     .section_if(true, "Active Feature", || "Feature X is enabled.".into())
///     .section_opt("Analytics", Some("Top tweets: ..."))
///     .section_opt("Missing", None::<String>)
///     .build();
///
/// assert!(prompt.contains("## Context"));
/// assert!(prompt.contains("## Active Feature"));
/// assert!(prompt.contains("## Analytics"));
/// assert!(!prompt.contains("## Missing"));
/// ```
pub struct SystemPromptBuilder {
    sections: Vec<String>,
    heading_prefix: String,
}

impl SystemPromptBuilder {
    /// Create a new builder with an initial preamble section.
    ///
    /// The preamble is included as-is (no heading prefix). Subsequent
    /// sections added via `section()` get `## ` prefixed headings by default.
    pub fn new(preamble: impl Into<String>) -> Self {
        Self {
            sections: vec![preamble.into()],
            heading_prefix: "##".to_string(),
        }
    }

    /// Set the heading level for subsequent `section()` calls.
    ///
    /// Level 2 produces `## Heading`, level 3 produces `### Heading`, etc.
    /// The default is 2.
    pub fn heading_level(mut self, level: u8) -> Self {
        self.heading_prefix = "#".repeat(level as usize);
        self
    }

    /// Append a named section with a markdown heading.
    ///
    /// Skipped if `content` is empty. Uses the builder's current heading level
    /// (default `##`). For a one-off heading level override, use [`section_at`](Self::section_at).
    pub fn section(mut self, heading: &str, content: impl Into<String>) -> Self {
        let content = content.into();
        if !content.is_empty() {
            self.sections
                .push(format!("{} {heading}\n\n{content}", self.heading_prefix));
        }
        self
    }

    /// Append a named section with a specific heading level, ignoring the
    /// builder's current `heading_level` setting.
    ///
    /// This is useful when a single section needs a different depth than the
    /// surrounding sections, without changing the builder-wide default:
    ///
    /// ```
    /// use cinch_rs::agent::prompt::SystemPromptBuilder;
    ///
    /// let prompt = SystemPromptBuilder::new("Preamble")
    ///     .section("Normal", "level 2 by default")
    ///     .section_at(3, "Nested", "level 3 just for this one")
    ///     .section("Back to Normal", "level 2 again")
    ///     .build();
    ///
    /// assert!(prompt.contains("## Normal"));
    /// assert!(prompt.contains("### Nested"));
    /// assert!(prompt.contains("## Back to Normal"));
    /// ```
    ///
    /// Skipped if `content` is empty.
    pub fn section_at(mut self, level: u8, heading: &str, content: impl Into<String>) -> Self {
        let content = content.into();
        if !content.is_empty() {
            let prefix = "#".repeat(level as usize);
            self.sections
                .push(format!("{prefix} {heading}\n\n{content}"));
        }
        self
    }

    /// Conditionally append a section with a specific heading level.
    ///
    /// Combines [`section_at`](Self::section_at) with [`section_if`](Self::section_if).
    pub fn section_at_if(
        self,
        level: u8,
        condition: bool,
        heading: &str,
        content_fn: impl FnOnce() -> String,
    ) -> Self {
        if condition {
            self.section_at(level, heading, content_fn())
        } else {
            self
        }
    }

    /// Append a section with a specific heading level only if the content is `Some`.
    ///
    /// Combines [`section_at`](Self::section_at) with [`section_opt`](Self::section_opt).
    pub fn section_at_opt(
        self,
        level: u8,
        heading: &str,
        content: Option<impl Into<String>>,
    ) -> Self {
        match content {
            Some(c) => self.section_at(level, heading, c),
            None => self,
        }
    }

    /// Conditionally append a section.
    ///
    /// The `content_fn` is only called when `condition` is true.
    pub fn section_if(
        self,
        condition: bool,
        heading: &str,
        content_fn: impl FnOnce() -> String,
    ) -> Self {
        if condition {
            self.section(heading, content_fn())
        } else {
            self
        }
    }

    /// Append a section only if the content is `Some`.
    pub fn section_opt(self, heading: &str, content: Option<impl Into<String>>) -> Self {
        match content {
            Some(c) => self.section(heading, c),
            None => self,
        }
    }

    /// Append raw text without a heading.
    ///
    /// Skipped if `content` is empty.
    pub fn raw(mut self, content: impl Into<String>) -> Self {
        let content = content.into();
        if !content.is_empty() {
            self.sections.push(content);
        }
        self
    }

    /// Conditionally append raw text.
    ///
    /// The `content_fn` is only called when `condition` is true.
    pub fn raw_if(self, condition: bool, content_fn: impl FnOnce() -> String) -> Self {
        if condition {
            self.raw(content_fn())
        } else {
            self
        }
    }

    /// Append raw text only if the content is `Some`.
    pub fn raw_opt(self, content: Option<impl Into<String>>) -> Self {
        match content {
            Some(c) => self.raw(c),
            None => self,
        }
    }

    /// Build the final system prompt by joining all sections with double newlines.
    pub fn build(self) -> String {
        self.sections.join("\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_preamble_only() {
        let prompt = SystemPromptBuilder::new("You are an agent.").build();
        assert_eq!(prompt, "You are an agent.");
    }

    #[test]
    fn sections_use_heading_prefix() {
        let prompt = SystemPromptBuilder::new("Preamble")
            .section("Context", "Some context")
            .build();
        assert_eq!(prompt, "Preamble\n\n## Context\n\nSome context");
    }

    #[test]
    fn custom_heading_level() {
        let prompt = SystemPromptBuilder::new("Preamble")
            .heading_level(3)
            .section("Sub", "Details")
            .build();
        assert!(prompt.contains("### Sub\n\nDetails"));
    }

    #[test]
    fn empty_section_skipped() {
        let prompt = SystemPromptBuilder::new("Preamble")
            .section("Empty", "")
            .section("Present", "content")
            .build();
        assert!(!prompt.contains("Empty"));
        assert!(prompt.contains("## Present"));
    }

    #[test]
    fn section_if_true_included() {
        let prompt = SystemPromptBuilder::new("Preamble")
            .section_if(true, "Active", || "active content".into())
            .build();
        assert!(prompt.contains("## Active"));
    }

    #[test]
    fn section_if_false_excluded() {
        let prompt = SystemPromptBuilder::new("Preamble")
            .section_if(false, "Inactive", || "should not appear".into())
            .build();
        assert!(!prompt.contains("Inactive"));
    }

    #[test]
    fn section_opt_some_included() {
        let prompt = SystemPromptBuilder::new("Preamble")
            .section_opt("Optional", Some("present"))
            .build();
        assert!(prompt.contains("## Optional\n\npresent"));
    }

    #[test]
    fn section_opt_none_excluded() {
        let prompt = SystemPromptBuilder::new("Preamble")
            .section_opt("Missing", None::<String>)
            .build();
        assert!(!prompt.contains("Missing"));
    }

    #[test]
    fn raw_appended_without_heading() {
        let prompt = SystemPromptBuilder::new("Preamble")
            .raw("---\nRaw block")
            .build();
        assert_eq!(prompt, "Preamble\n\n---\nRaw block");
    }

    #[test]
    fn raw_if_true() {
        let prompt = SystemPromptBuilder::new("P")
            .raw_if(true, || "raw content".into())
            .build();
        assert!(prompt.contains("raw content"));
    }

    #[test]
    fn raw_if_false() {
        let prompt = SystemPromptBuilder::new("P")
            .raw_if(false, || "hidden".into())
            .build();
        assert!(!prompt.contains("hidden"));
    }

    #[test]
    fn raw_opt_some() {
        let prompt = SystemPromptBuilder::new("P")
            .raw_opt(Some("visible"))
            .build();
        assert!(prompt.contains("visible"));
    }

    #[test]
    fn raw_opt_none() {
        let prompt = SystemPromptBuilder::new("P")
            .raw_opt(None::<String>)
            .build();
        assert_eq!(prompt, "P");
    }

    #[test]
    fn empty_raw_skipped() {
        let prompt = SystemPromptBuilder::new("P").raw("").build();
        assert_eq!(prompt, "P");
    }

    #[test]
    fn complex_prompt_assembly() {
        let has_analytics = true;
        let perf_section: Option<String> = None;
        let learnings = "Note: prefer short tweets.";

        let prompt = SystemPromptBuilder::new("You are the social media agent.")
            .section("Context", "Cycle 5 of 10.")
            .section_if(has_analytics, "Analytics", || "Top tweet: 42 likes".into())
            .section_opt("Performance", perf_section)
            .section_if(!learnings.is_empty(), "Learnings", || learnings.into())
            .build();

        assert!(prompt.contains("## Context\n\nCycle 5 of 10."));
        assert!(prompt.contains("## Analytics\n\nTop tweet: 42 likes"));
        assert!(!prompt.contains("Performance"));
        assert!(prompt.contains("## Learnings\n\nNote: prefer short tweets."));
    }

    #[test]
    fn section_at_overrides_heading_level() {
        let prompt = SystemPromptBuilder::new("Preamble")
            .section("Level2", "content A")
            .section_at(3, "Level3", "content B")
            .section("BackTo2", "content C")
            .build();
        assert!(prompt.contains("## Level2\n\ncontent A"));
        assert!(prompt.contains("### Level3\n\ncontent B"));
        assert!(prompt.contains("## BackTo2\n\ncontent C"));
    }

    #[test]
    fn section_at_empty_skipped() {
        let prompt = SystemPromptBuilder::new("P")
            .section_at(3, "Empty", "")
            .build();
        assert_eq!(prompt, "P");
    }

    #[test]
    fn section_at_if_true() {
        let prompt = SystemPromptBuilder::new("P")
            .section_at_if(4, true, "Deep", || "deep content".into())
            .build();
        assert!(prompt.contains("#### Deep\n\ndeep content"));
    }

    #[test]
    fn section_at_if_false() {
        let prompt = SystemPromptBuilder::new("P")
            .section_at_if(4, false, "Hidden", || "should not appear".into())
            .build();
        assert!(!prompt.contains("Hidden"));
    }

    #[test]
    fn section_at_opt_some() {
        let prompt = SystemPromptBuilder::new("P")
            .section_at_opt(3, "Present", Some("here"))
            .build();
        assert!(prompt.contains("### Present\n\nhere"));
    }

    #[test]
    fn section_at_opt_none() {
        let prompt = SystemPromptBuilder::new("P")
            .section_at_opt(3, "Missing", None::<String>)
            .build();
        assert!(!prompt.contains("Missing"));
    }
}
