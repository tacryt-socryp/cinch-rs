//! Dynamic tool filtering for context-aware tool availability.
//!
//! When an agent has many tools (50+), sending all definitions in every
//! request wastes context and increases confusion. This module provides
//! strategies for dynamically selecting which tools to include:
//! - Category-based filtering (group tools by domain)
//! - Task-based filtering (select tools relevant to the current task)
//! - Usage-based filtering (promote frequently used tools)

use crate::ToolDef;
use std::collections::{HashMap, HashSet};

/// A category of related tools.
#[derive(Debug, Clone)]
pub struct ToolCategory {
    /// Category name.
    pub name: String,
    /// Tool names in this category.
    pub tools: Vec<String>,
    /// Description of when this category is relevant.
    pub when_relevant: String,
}

impl ToolCategory {
    /// Create a new tool category from string slices.
    ///
    /// Avoids the repetitive `.into()` calls on each tool name when
    /// constructing categories from string literals.
    pub fn new(name: impl Into<String>, tools: &[&str], when_relevant: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            tools: tools.iter().map(|s| (*s).to_string()).collect(),
            when_relevant: when_relevant.into(),
        }
    }
}

/// Strategy for filtering tools.
#[derive(Debug)]
pub struct ToolFilter {
    /// Tool categories.
    categories: Vec<ToolCategory>,
    /// Tools that should always be included regardless of filtering.
    always_include: HashSet<String>,
    /// Maximum number of tools to include per request.
    max_tools: usize,
    /// Usage counts for promoting frequently-used tools.
    usage_counts: HashMap<String, u32>,
}

impl ToolFilter {
    pub fn new(max_tools: usize) -> Self {
        Self {
            categories: Vec::new(),
            always_include: HashSet::new(),
            max_tools,
            usage_counts: HashMap::new(),
        }
    }

    /// Add a tool category.
    pub fn add_category(&mut self, category: ToolCategory) {
        self.categories.push(category);
    }

    /// Add a tool category (builder pattern).
    pub fn with_category(mut self, category: ToolCategory) -> Self {
        self.categories.push(category);
        self
    }

    /// Mark a tool as always-included.
    pub fn always_include(&mut self, tool_name: impl Into<String>) {
        self.always_include.insert(tool_name.into());
    }

    /// Mark a tool as always-included (builder pattern).
    pub fn with_always_include(mut self, tool_name: impl Into<String>) -> Self {
        self.always_include.insert(tool_name.into());
        self
    }

    /// Mark multiple tools as always-included (builder pattern).
    pub fn with_always_include_all(mut self, tool_names: &[&str]) -> Self {
        for name in tool_names {
            self.always_include.insert((*name).to_string());
        }
        self
    }

    /// Record a tool usage (call this after each tool execution).
    pub fn record_usage(&mut self, tool_name: &str) {
        *self.usage_counts.entry(tool_name.to_string()).or_insert(0) += 1;
    }

    /// Filter tools based on task keywords.
    ///
    /// Selects categories whose `when_relevant` description matches any of
    /// the keywords, plus always-included tools. Falls back to most-used
    /// tools if no keywords match.
    pub fn filter_for_task(&self, task_keywords: &[&str], all_tools: &[ToolDef]) -> Vec<ToolDef> {
        let mut selected_names: HashSet<String> = self.always_include.clone();

        // Add tools from relevant categories.
        for category in &self.categories {
            let relevant = task_keywords.iter().any(|kw| {
                category
                    .when_relevant
                    .to_lowercase()
                    .contains(&kw.to_lowercase())
                    || category.name.to_lowercase().contains(&kw.to_lowercase())
            });
            if relevant {
                for tool in &category.tools {
                    selected_names.insert(tool.clone());
                }
            }
        }

        // If no categories matched, include most-used tools.
        if selected_names.len() <= self.always_include.len() {
            let mut by_usage: Vec<_> = self.usage_counts.iter().collect();
            by_usage.sort_by(|a, b| b.1.cmp(a.1));
            for (name, _) in by_usage.iter().take(self.max_tools) {
                selected_names.insert(name.to_string());
            }
        }

        // Filter the tool definitions.
        let mut filtered: Vec<ToolDef> = all_tools
            .iter()
            .filter(|t| selected_names.contains(&t.function.name))
            .cloned()
            .collect();

        // Truncate to max_tools.
        filtered.truncate(self.max_tools);
        filtered
    }

    /// Register the standard categories for tools provided by
    /// [`ToolSet::with_common_tools`](crate::tools::core::ToolSet::with_common_tools).
    ///
    /// Adds five categories (`file_ops`, `search`, `editing`, `shell`, `web`)
    /// and marks `think`, `todo`, `read_file`, `list_dir`, and `shell` as
    /// always-included. Domain-specific categories can be chained after
    /// this call.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let filter = ToolFilter::new(15)
    ///     .with_common_categories()
    ///     .with_category(ToolCategory::new("twitter", &["post_tweet"], "When posting"));
    /// ```
    pub fn with_common_categories(self) -> Self {
        use super::names::*;
        self.with_always_include_all(&[THINK, TODO, READ_FILE, LIST_DIR, SHELL])
            .with_category(ToolCategory::new(
                "file_ops",
                &[READ_FILE, LIST_DIR, FIND_FILES],
                "When reading or browsing files",
            ))
            .with_category(ToolCategory::new(
                "search",
                &[GREP],
                "When searching file contents",
            ))
            .with_category(ToolCategory::new(
                "editing",
                &[EDIT_FILE, WRITE_FILE],
                "When modifying or creating files",
            ))
            .with_category(ToolCategory::new(
                "shell",
                &[SHELL],
                "When running shell commands",
            ))
            .with_category(ToolCategory::new(
                "web",
                &[WEB_SEARCH],
                "When searching the internet",
            ))
    }

    /// Get all available categories.
    pub fn categories(&self) -> &[ToolCategory] {
        &self.categories
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool_def(name: &str) -> ToolDef {
        ToolDef::new(name, format!("{name} tool"), serde_json::json!({}))
    }

    #[test]
    fn filter_by_category() {
        let mut filter = ToolFilter::new(10);
        filter.always_include("think");
        filter.add_category(ToolCategory {
            name: "file_ops".into(),
            tools: vec!["read_file".into(), "list_dir".into()],
            when_relevant: "When reading or browsing files".into(),
        });
        filter.add_category(ToolCategory {
            name: "search".into(),
            tools: vec!["grep".into(), "find_files".into()],
            when_relevant: "When searching for content or files".into(),
        });

        let all_tools = vec![
            make_tool_def("think"),
            make_tool_def("read_file"),
            make_tool_def("list_dir"),
            make_tool_def("grep"),
            make_tool_def("find_files"),
            make_tool_def("shell"),
        ];

        let filtered = filter.filter_for_task(&["search"], &all_tools);
        let names: Vec<_> = filtered.iter().map(|t| t.function.name.as_str()).collect();
        assert!(names.contains(&"think"));
        assert!(names.contains(&"grep"));
        assert!(names.contains(&"find_files"));
        assert!(!names.contains(&"shell"));
    }

    #[test]
    fn always_include_present() {
        let mut filter = ToolFilter::new(10);
        filter.always_include("think");
        filter.always_include("todo");

        let all_tools = vec![
            make_tool_def("think"),
            make_tool_def("todo"),
            make_tool_def("shell"),
        ];

        let filtered = filter.filter_for_task(&["nonexistent"], &all_tools);
        let names: Vec<_> = filtered.iter().map(|t| t.function.name.as_str()).collect();
        assert!(names.contains(&"think"));
        assert!(names.contains(&"todo"));
    }

    #[test]
    fn usage_based_fallback() {
        let mut filter = ToolFilter::new(3);
        filter.record_usage("grep");
        filter.record_usage("grep");
        filter.record_usage("grep");
        filter.record_usage("read_file");
        filter.record_usage("read_file");
        filter.record_usage("shell");

        let all_tools = vec![
            make_tool_def("grep"),
            make_tool_def("read_file"),
            make_tool_def("shell"),
            make_tool_def("list_dir"),
        ];

        // No matching keywords → falls back to most-used.
        let filtered = filter.filter_for_task(&["nonexistent"], &all_tools);
        assert!(filtered.len() <= 3);
    }

    // ── Builder API tests ────────────────────────────────────────

    #[test]
    fn tool_category_new_convenience() {
        let cat = ToolCategory::new("file_ops", &["read_file", "list_dir"], "When reading files");
        assert_eq!(cat.name, "file_ops");
        assert_eq!(cat.tools, vec!["read_file", "list_dir"]);
        assert_eq!(cat.when_relevant, "When reading files");
    }

    #[test]
    fn filter_builder_pattern() {
        let filter = ToolFilter::new(10)
            .with_always_include("think")
            .with_always_include("todo")
            .with_category(ToolCategory::new(
                "search",
                &["grep", "find_files"],
                "When searching",
            ));

        let all_tools = vec![
            make_tool_def("think"),
            make_tool_def("todo"),
            make_tool_def("grep"),
            make_tool_def("find_files"),
            make_tool_def("shell"),
        ];

        let filtered = filter.filter_for_task(&["searching"], &all_tools);
        let names: Vec<_> = filtered.iter().map(|t| t.function.name.as_str()).collect();
        assert!(names.contains(&"think"));
        assert!(names.contains(&"todo"));
        assert!(names.contains(&"grep"));
        assert!(names.contains(&"find_files"));
    }

    #[test]
    fn filter_with_always_include_all() {
        let filter = ToolFilter::new(10).with_always_include_all(&["think", "todo", "read_file"]);

        let all_tools = vec![
            make_tool_def("think"),
            make_tool_def("todo"),
            make_tool_def("read_file"),
            make_tool_def("shell"),
        ];

        let filtered = filter.filter_for_task(&["nonexistent"], &all_tools);
        let names: Vec<_> = filtered.iter().map(|t| t.function.name.as_str()).collect();
        assert!(names.contains(&"think"));
        assert!(names.contains(&"todo"));
        assert!(names.contains(&"read_file"));
    }

    #[test]
    fn with_common_categories_registers_standard_tools() {
        let filter = ToolFilter::new(15).with_common_categories();

        let all_tools = vec![
            make_tool_def("think"),
            make_tool_def("todo"),
            make_tool_def("read_file"),
            make_tool_def("list_dir"),
            make_tool_def("find_files"),
            make_tool_def("grep"),
            make_tool_def("shell"),
            make_tool_def("custom_tool"),
        ];

        // "files" keyword should match file_ops category
        let filtered = filter.filter_for_task(&["files"], &all_tools);
        let names: Vec<_> = filtered.iter().map(|t| t.function.name.as_str()).collect();
        assert!(names.contains(&"think"), "think should always be included");
        assert!(names.contains(&"todo"), "todo should always be included");
        assert!(
            names.contains(&"read_file"),
            "read_file should be included (always + file_ops)"
        );
        assert!(
            names.contains(&"list_dir"),
            "list_dir should be included (always + file_ops)"
        );
        assert!(
            names.contains(&"find_files"),
            "find_files should be included (file_ops)"
        );
        assert!(names.contains(&"shell"), "shell should always be included");
    }

    #[test]
    fn with_common_categories_composable_with_domain_categories() {
        let filter = ToolFilter::new(15)
            .with_common_categories()
            .with_category(ToolCategory::new(
                "twitter",
                &["post_tweet", "save_draft"],
                "When posting or drafting tweets",
            ));

        assert_eq!(filter.categories().len(), 6); // file_ops, search, editing, shell, web, twitter
    }
}
