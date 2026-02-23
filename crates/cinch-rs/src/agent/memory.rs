//! File-based memory instructions and MEMORY.md index loading.
//!
//! Agents use a `memory/` directory for persistent cross-session knowledge.
//! The primary file is `memory/MEMORY.md` — a concise, semantically organized
//! index kept under ~200 lines. Topic files (`memory/*.md`) hold detailed
//! notes and are loaded on demand, not at startup.
//!
//! The default prompt teaches agents this convention. At runtime,
//! [`read_memory_index()`] loads the MEMORY.md content for injection into the
//! system message.

use std::path::Path;

/// Returns the default file-based memory prompt for injection into system messages.
///
/// Teaches the agent the MEMORY.md convention:
/// - `memory/MEMORY.md` as the primary index (concise, under 200 lines)
/// - Topic files (`memory/*.md`) for detailed notes, loaded on demand
/// - Semantic organization by topic, not chronological
pub fn default_memory_prompt() -> String {
    r#"## File-Based Memory

You have a persistent memory directory at `memory/`. Its contents persist across sessions.

As you work, consult your memory files to build on previous experience.

### How to save memories

- Organize memory semantically by topic, not chronologically
- `memory/MEMORY.md` is the primary index — keep it concise and under 200 lines
- Create separate topic files (e.g., `memory/debugging.md`, `memory/patterns.md`) for detailed notes and link to them from MEMORY.md
- Update or remove memories that turn out to be wrong or outdated
- Do not write duplicate memories — check if there is an existing entry to update first

### What to save

- Stable patterns and conventions confirmed across multiple interactions
- Key architectural decisions, important file paths, and project structure
- User preferences for workflow, tools, and communication style
- Solutions to recurring problems and debugging insights

### What NOT to save

- Session-specific context (current task details, in-progress work)
- Information that might be incomplete — verify before writing
- Speculative or unverified conclusions from reading a single file

### Explicit user requests

- When the user asks you to remember something across sessions, save it immediately
- When the user asks to forget something, find and remove the relevant entries
"#
    .to_string()
}

/// Read the MEMORY.md index file from the given path.
///
/// Returns `None` if the file doesn't exist or can't be read. If the file
/// exceeds `max_lines`, truncates and appends a note indicating truncation.
pub fn read_memory_index(path: &Path, max_lines: usize) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;

    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();

    if total <= max_lines {
        Some(content)
    } else {
        let truncated: String = lines[..max_lines].join("\n");
        Some(format!(
            "{truncated}\n\n[MEMORY.md truncated at {max_lines} of {total} lines. Read the full file for more.]"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_prompt_is_nonempty() {
        let prompt = default_memory_prompt();
        assert!(!prompt.is_empty());
        assert!(prompt.contains("MEMORY.md"));
    }

    #[test]
    fn default_prompt_contains_key_sections() {
        let prompt = default_memory_prompt();
        assert!(prompt.contains("## File-Based Memory"));
        assert!(prompt.contains("### How to save memories"));
        assert!(prompt.contains("### What to save"));
        assert!(prompt.contains("### What NOT to save"));
    }

    #[test]
    fn default_prompt_no_old_conventions() {
        let prompt = default_memory_prompt();
        assert!(!prompt.contains("scratchpad.md"));
        assert!(!prompt.contains("learnings.md"));
        assert!(!prompt.contains("sources-used.md"));
    }

    #[test]
    fn read_memory_index_missing_file() {
        let result = read_memory_index(Path::new("/nonexistent/MEMORY.md"), 200);
        assert!(result.is_none());
    }

    #[test]
    fn read_memory_index_under_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("MEMORY.md");
        std::fs::write(&path, "# Memory\n\nSome notes here.\n").unwrap();

        let result = read_memory_index(&path, 200);
        assert!(result.is_some());
        let content = result.unwrap();
        assert!(content.contains("Some notes here"));
        assert!(!content.contains("truncated"));
    }

    #[test]
    fn read_memory_index_over_limit_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("MEMORY.md");
        let lines: Vec<String> = (0..50).map(|i| format!("Line {i}")).collect();
        std::fs::write(&path, lines.join("\n")).unwrap();

        let result = read_memory_index(&path, 10);
        assert!(result.is_some());
        let content = result.unwrap();
        assert!(content.contains("Line 0"));
        assert!(content.contains("Line 9"));
        assert!(!content.contains("Line 10"));
        assert!(content.contains("[MEMORY.md truncated at 10 of 50 lines."));
    }

    #[test]
    fn read_memory_index_exact_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("MEMORY.md");
        let lines: Vec<String> = (0..10).map(|i| format!("Line {i}")).collect();
        std::fs::write(&path, lines.join("\n")).unwrap();

        let result = read_memory_index(&path, 10);
        assert!(result.is_some());
        let content = result.unwrap();
        assert!(!content.contains("truncated"));
    }
}
