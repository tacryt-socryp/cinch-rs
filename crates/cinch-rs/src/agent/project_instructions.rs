//! Project-level instruction loading (AGENTS.md).
//!
//! Loads instructions from a hierarchy of files — user-global, project root,
//! `.cinch/` directory, and local overrides — and presents them as a single
//! prompt string for injection into the system message.
//!
//! Files in `.cinch/rules/*.md` may contain YAML frontmatter with `paths:`
//! globs, making them conditional on which files the agent accesses during a
//! session. A `## Compaction Instructions` section in any file is extracted
//! and forwarded to the summarizer for project-specific compaction guidance.

use std::fs;
use std::path::{Path, PathBuf};

/// Combined project instructions loaded from the file hierarchy.
#[derive(Debug, Clone, Default)]
pub struct ProjectInstructions {
    /// Combined prompt text from all unconditional instruction files.
    pub prompt: String,
    /// Extracted `## Compaction Instructions` section (for the summarizer).
    pub compaction_instructions: Option<String>,
    /// Conditional rules with path glob filters.
    pub conditional_rules: Vec<ConditionalRule>,
}

/// A conditional rule that only applies when accessed files match its globs.
#[derive(Debug, Clone)]
pub struct ConditionalRule {
    /// Glob patterns that trigger this rule (e.g. `crates/cinch-web/**`).
    pub globs: Vec<String>,
    /// The instruction content to include when triggered.
    pub content: String,
}

impl ProjectInstructions {
    /// Load project instructions from the standard file hierarchy.
    ///
    /// Search order:
    /// 1. `~/.config/cinch/AGENTS.md` (user-global)
    /// 2. `{root}/AGENTS.md`
    /// 3. `{root}/.cinch/AGENTS.md`
    /// 4. `{root}/.cinch/rules/*.md` (sorted; YAML frontmatter `paths` → conditional)
    /// 5. `{root}/AGENTS.local.md`
    ///
    /// Unconditional content is concatenated into `prompt`. `## Compaction
    /// Instructions` sections are extracted into `compaction_instructions`.
    /// Files with `paths` frontmatter go into `conditional_rules`.
    pub fn load(project_root: Option<&Path>) -> Self {
        let mut prompt_parts: Vec<String> = Vec::new();
        let mut compaction_parts: Vec<String> = Vec::new();
        let mut conditional_rules: Vec<ConditionalRule> = Vec::new();

        // 1. User-global config.
        if let Some(home) = dirs_path() {
            let global = home.join(".config/cinch/AGENTS.md");
            if let Some(content) = read_optional(&global) {
                let (main, compaction) = extract_compaction_section(&content);
                prompt_parts.push(main);
                if let Some(c) = compaction {
                    compaction_parts.push(c);
                }
            }
        }

        let Some(root) = project_root else {
            return Self::from_parts(prompt_parts, compaction_parts, conditional_rules);
        };

        // 2. {root}/AGENTS.md
        load_unconditional(&root.join("AGENTS.md"), &mut prompt_parts, &mut compaction_parts);

        // 3. {root}/.cinch/AGENTS.md
        load_unconditional(
            &root.join(".cinch/AGENTS.md"),
            &mut prompt_parts,
            &mut compaction_parts,
        );

        // 4. {root}/.cinch/rules/*.md (sorted, may have frontmatter)
        let rules_dir = root.join(".cinch/rules");
        if rules_dir.is_dir()
            && let Ok(entries) = fs::read_dir(&rules_dir)
        {
            let mut paths: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
                .collect();
            paths.sort();

            for path in paths {
                if let Some(content) = read_optional(&path) {
                    let (globs, body) = parse_frontmatter(&content);
                    if globs.is_empty() {
                        // Unconditional rule file.
                        let (main, compaction) = extract_compaction_section(body);
                        prompt_parts.push(main);
                        if let Some(c) = compaction {
                            compaction_parts.push(c);
                        }
                    } else {
                        // Conditional — store for later filtering.
                        conditional_rules.push(ConditionalRule {
                            globs,
                            content: body.to_string(),
                        });
                    }
                }
            }
        }

        // 5. {root}/AGENTS.local.md
        load_unconditional(
            &root.join("AGENTS.local.md"),
            &mut prompt_parts,
            &mut compaction_parts,
        );

        Self::from_parts(prompt_parts, compaction_parts, conditional_rules)
    }

    /// Returns content from conditional rules where any accessed path matches
    /// any rule glob.
    pub fn rules_for_accessed_files(&self, accessed_paths: &[&str]) -> String {
        let mut matched: Vec<&str> = Vec::new();
        for rule in &self.conditional_rules {
            let matches = rule
                .globs
                .iter()
                .any(|glob| accessed_paths.iter().any(|path| glob_matches(glob, path)));
            if matches {
                matched.push(&rule.content);
            }
        }
        matched.join("\n\n")
    }

    fn from_parts(
        prompt_parts: Vec<String>,
        compaction_parts: Vec<String>,
        conditional_rules: Vec<ConditionalRule>,
    ) -> Self {
        let prompt = prompt_parts
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        let compaction_instructions = if compaction_parts.is_empty() {
            None
        } else {
            Some(compaction_parts.join("\n\n"))
        };
        Self {
            prompt,
            compaction_instructions,
            conditional_rules,
        }
    }
}

// ── Private helpers ──────────────────────────────────────────────────

/// Get the user's home directory.
fn dirs_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Read a file if it exists and is readable.
fn read_optional(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// Load an unconditional instruction file, splitting out compaction sections.
fn load_unconditional(
    path: &Path,
    prompt_parts: &mut Vec<String>,
    compaction_parts: &mut Vec<String>,
) {
    if let Some(content) = read_optional(path) {
        let (main, compaction) = extract_compaction_section(&content);
        prompt_parts.push(main);
        if let Some(c) = compaction {
            compaction_parts.push(c);
        }
    }
}

/// Extract a `## Compaction Instructions` section from content.
///
/// Returns `(main_content, Option<compaction_section>)`. The compaction
/// section includes everything from the `## Compaction Instructions` heading
/// to the next `## ` heading or end of content.
fn extract_compaction_section(content: &str) -> (String, Option<String>) {
    const HEADER: &str = "## Compaction Instructions";
    let Some(start) = content.find(HEADER) else {
        return (content.to_string(), None);
    };

    let after_header = start + HEADER.len();
    // Find the next ## heading after our section, or end of content.
    let section_end = content[after_header..]
        .find("\n## ")
        .map(|pos| after_header + pos)
        .unwrap_or(content.len());

    let compaction = content[after_header..section_end].trim().to_string();
    let main = format!(
        "{}{}",
        content[..start].trim_end(),
        if section_end < content.len() {
            format!("\n\n{}", content[section_end..].trim_start())
        } else {
            String::new()
        }
    );

    let compaction = if compaction.is_empty() {
        None
    } else {
        Some(compaction)
    };
    (main.trim().to_string(), compaction)
}

/// Parse YAML frontmatter for `paths:` list.
///
/// Expects content starting with `---\n`, containing `paths:` followed by
/// `  - "pattern"` items, and ending with `---\n`. Minimal parsing — no
/// serde_yaml dependency.
///
/// Returns `(glob_patterns, body_after_frontmatter)`.
fn parse_frontmatter(content: &str) -> (Vec<String>, &str) {
    if !content.starts_with("---") {
        return (Vec::new(), content);
    }

    // Find closing ---
    let after_first = &content[3..];
    let close = after_first.find("\n---");
    let Some(close_pos) = close else {
        return (Vec::new(), content);
    };

    let frontmatter = &after_first[..close_pos];
    let body_start = 3 + close_pos + 4; // skip past "\n---"
    let body = if body_start < content.len() {
        content[body_start..].trim_start_matches('\n')
    } else {
        ""
    };

    let mut globs = Vec::new();
    let mut in_paths = false;
    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("paths:") {
            in_paths = true;
            continue;
        }
        if in_paths {
            if let Some(item) = trimmed.strip_prefix("- ") {
                let item = item.trim().trim_matches('"').trim_matches('\'');
                if !item.is_empty() {
                    globs.push(item.to_string());
                }
            } else if !trimmed.is_empty() {
                // Non-list line means paths section ended.
                in_paths = false;
            }
        }
    }

    (globs, body)
}

/// Simple glob matching supporting `**`, `*`, and literal characters.
///
/// `**` matches any number of path segments (including zero).
/// `*` matches anything except `/`.
/// Everything else is literal.
fn glob_matches(pattern: &str, path: &str) -> bool {
    glob_matches_inner(pattern.as_bytes(), path.as_bytes())
}

fn glob_matches_inner(pattern: &[u8], path: &[u8]) -> bool {
    let mut pi = 0; // pattern index
    let mut si = 0; // string (path) index

    // For backtracking on `*`
    let mut star_pi = usize::MAX;
    let mut star_si = 0;

    // For backtracking on `**`
    let mut dstar_pi = usize::MAX;
    let mut dstar_si = 0;

    while si < path.len() {
        if pi < pattern.len() && pattern[pi] == b'*' {
            if pi + 1 < pattern.len() && pattern[pi + 1] == b'*' {
                // `**` — match any number of segments
                dstar_pi = pi;
                dstar_si = si;
                pi += 2;
                // Skip trailing `/` after `**`
                if pi < pattern.len() && pattern[pi] == b'/' {
                    pi += 1;
                }
                continue;
            } else {
                // Single `*` — match non-slash characters
                star_pi = pi;
                star_si = si;
                pi += 1;
                continue;
            }
        }

        if pi < pattern.len() && (pattern[pi] == path[si] || pattern[pi] == b'?') {
            pi += 1;
            si += 1;
            continue;
        }

        // Try backtracking on single `*` (non-slash only).
        if star_pi != usize::MAX && path[star_si] != b'/' {
            star_si += 1;
            si = star_si;
            pi = star_pi + 1;
            continue;
        }

        // Try backtracking on `**` (any character including `/`).
        if dstar_pi != usize::MAX {
            dstar_si += 1;
            si = dstar_si;
            pi = dstar_pi + 2;
            if pi < pattern.len() && pattern[pi] == b'/' {
                pi += 1;
            }
            // Reset single-star tracking since we're re-entering from dstar.
            star_pi = usize::MAX;
            continue;
        }

        return false;
    }

    // Consume trailing `*` or `**` patterns.
    while pi < pattern.len() && pattern[pi] == b'*' {
        pi += 1;
    }
    // Also consume trailing `/` after `**`.
    if pi < pattern.len() && pattern[pi] == b'/' {
        pi += 1;
    }

    pi == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn load_with_none_root() {
        let instructions = ProjectInstructions::load(None);
        // Should not panic; prompt may be empty or contain global config.
        let _ = instructions.prompt;
    }

    #[test]
    fn load_with_nonexistent_root() {
        let instructions = ProjectInstructions::load(Some(Path::new("/nonexistent/path/xyz")));
        // No files found — prompt should be empty (ignoring global config).
        // At least verify it doesn't panic.
        let _ = instructions.prompt;
    }

    #[test]
    fn load_from_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("AGENTS.md"), "# Project Rules\nBe concise.").unwrap();
        fs::create_dir_all(root.join(".cinch")).unwrap();
        fs::write(root.join(".cinch/AGENTS.md"), "Use Rust idioms.").unwrap();

        let instructions = ProjectInstructions::load(Some(root));
        assert!(instructions.prompt.contains("Be concise"));
        assert!(instructions.prompt.contains("Use Rust idioms"));
    }

    #[test]
    fn load_with_local_override() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("AGENTS.md"), "Base rules.").unwrap();
        fs::write(root.join("AGENTS.local.md"), "My local rules.").unwrap();

        let instructions = ProjectInstructions::load(Some(root));
        assert!(instructions.prompt.contains("Base rules"));
        assert!(instructions.prompt.contains("My local rules"));
    }

    #[test]
    fn extract_compaction_section_present() {
        let content = "# Rules\nBe good.\n\n## Compaction Instructions\nPreserve file paths.\n\n## Other\nStuff.";
        let (main, compaction) = extract_compaction_section(content);
        assert!(main.contains("Be good"));
        assert!(main.contains("## Other"));
        assert!(!main.contains("Compaction Instructions"));
        assert_eq!(compaction.unwrap(), "Preserve file paths.");
    }

    #[test]
    fn extract_compaction_section_absent() {
        let content = "# Rules\nBe good.";
        let (main, compaction) = extract_compaction_section(content);
        assert_eq!(main, content);
        assert!(compaction.is_none());
    }

    #[test]
    fn extract_compaction_section_at_end() {
        let content = "# Rules\n\n## Compaction Instructions\nKeep file paths.";
        let (main, compaction) = extract_compaction_section(content);
        assert_eq!(main, "# Rules");
        assert_eq!(compaction.unwrap(), "Keep file paths.");
    }

    #[test]
    fn parse_frontmatter_with_paths() {
        let content = r#"---
paths:
  - "crates/cinch-web/**"
  - "**/*.html"
---
Web-specific rules here.
"#;
        let (globs, body) = parse_frontmatter(content);
        assert_eq!(globs, vec!["crates/cinch-web/**", "**/*.html"]);
        assert!(body.contains("Web-specific rules"));
    }

    #[test]
    fn parse_frontmatter_no_frontmatter() {
        let content = "Just regular content.";
        let (globs, body) = parse_frontmatter(content);
        assert!(globs.is_empty());
        assert_eq!(body, content);
    }

    #[test]
    fn parse_frontmatter_no_paths() {
        let content = "---\ntitle: Something\n---\nBody here.";
        let (globs, body) = parse_frontmatter(content);
        assert!(globs.is_empty());
        assert!(body.contains("Body here"));
    }

    #[test]
    fn glob_matches_double_star() {
        assert!(glob_matches("**/*.rs", "src/main.rs"));
        assert!(glob_matches("**/*.rs", "crates/foo/src/lib.rs"));
        assert!(!glob_matches("**/*.rs", "src/main.py"));
    }

    #[test]
    fn glob_matches_single_star() {
        assert!(glob_matches("src/*.rs", "src/main.rs"));
        assert!(!glob_matches("src/*.rs", "src/deep/main.rs"));
    }

    #[test]
    fn glob_matches_directory_double_star() {
        assert!(glob_matches("crates/cinch-web/**", "crates/cinch-web/src/lib.rs"));
        assert!(glob_matches("crates/cinch-web/**", "crates/cinch-web/Cargo.toml"));
        assert!(!glob_matches("crates/cinch-web/**", "crates/cinch-rs/src/lib.rs"));
    }

    #[test]
    fn glob_matches_exact() {
        assert!(glob_matches("README.md", "README.md"));
        assert!(!glob_matches("README.md", "docs/README.md"));
    }

    #[test]
    fn glob_no_match() {
        assert!(!glob_matches("src/**/*.ts", "crates/foo/src/lib.rs"));
    }

    #[test]
    fn rules_for_accessed_files_basic() {
        let instructions = ProjectInstructions {
            prompt: String::new(),
            compaction_instructions: None,
            conditional_rules: vec![
                ConditionalRule {
                    globs: vec!["crates/cinch-web/**".into()],
                    content: "Web rules apply.".into(),
                },
                ConditionalRule {
                    globs: vec!["**/*.py".into()],
                    content: "Python rules apply.".into(),
                },
            ],
        };

        let result = instructions.rules_for_accessed_files(&["crates/cinch-web/src/lib.rs"]);
        assert!(result.contains("Web rules apply"));
        assert!(!result.contains("Python rules"));

        let result = instructions.rules_for_accessed_files(&["scripts/build.py"]);
        assert!(result.contains("Python rules"));
        assert!(!result.contains("Web rules"));

        let result = instructions.rules_for_accessed_files(&["README.md"]);
        assert!(result.is_empty());
    }

    #[test]
    fn conditional_rules_from_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let rules_dir = root.join(".cinch/rules");
        fs::create_dir_all(&rules_dir).unwrap();

        fs::write(
            rules_dir.join("web.md"),
            "---\npaths:\n  - \"crates/cinch-web/**\"\n---\nUse axum patterns.",
        )
        .unwrap();

        fs::write(rules_dir.join("general.md"), "Always run clippy.").unwrap();

        let instructions = ProjectInstructions::load(Some(root));
        assert!(instructions.prompt.contains("Always run clippy"));
        assert_eq!(instructions.conditional_rules.len(), 1);
        assert!(instructions.conditional_rules[0]
            .content
            .contains("axum patterns"));
    }

    #[test]
    fn compaction_instructions_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("AGENTS.md"),
            "# Rules\n\n## Compaction Instructions\nAlways preserve test file paths.",
        )
        .unwrap();

        let instructions = ProjectInstructions::load(Some(root));
        assert!(instructions
            .compaction_instructions
            .as_ref()
            .unwrap()
            .contains("preserve test file paths"));
    }
}
