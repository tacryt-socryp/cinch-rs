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

use crate::{ChatRequest, Message, OpenRouterClient};

/// System prompt for LLM-based memory consolidation.
///
/// Instructs the model to merge duplicates, remove stale content, and preserve
/// technical details verbatim — producing only consolidated Markdown output.
const CONSOLIDATION_PROMPT: &str = "\
You are a memory consolidation assistant. Your job is to take a MEMORY.md file \
that has grown too long and produce a shorter, consolidated version.

Rules:
- Merge duplicate or overlapping entries into single entries
- Remove stale or superseded information (keep only the latest/correct version)
- Preserve file paths, function names, and technical details verbatim
- Keep semantic organization by topic (use ## headings)
- Output ONLY the consolidated Markdown — no commentary, no explanation
- Stay within the target line count provided by the user
- Prioritize the most useful and actionable information";

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

/// Consolidate a MEMORY.md file that exceeds the line limit.
///
/// Sends the file content through a cheap LLM call that merges redundant
/// entries, removes stale content, and produces a trimmed version. The result
/// is atomically written back (temp file + rename).
///
/// Returns `Ok(Some((lines_before, lines_after)))` when consolidation occurred,
/// `Ok(None)` when the file is missing or already within the limit, or `Err`
/// on failure.
pub async fn consolidate_memory(
    client: &OpenRouterClient,
    memory_path: &Path,
    max_lines: usize,
    model: &str,
) -> Result<Option<(usize, usize)>, String> {
    // 1. Read file — if missing, nothing to consolidate.
    let content = match std::fs::read_to_string(memory_path) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };

    // 2. Count lines — if within limit, nothing to do.
    let lines_before = content.lines().count();
    if lines_before <= max_lines {
        return Ok(None);
    }

    // 3. Build consolidation request.
    let user_prompt = format!(
        "Consolidate the following MEMORY.md file to at most {max_lines} lines.\n\n\
         Current content ({lines_before} lines):\n\n{content}"
    );
    let request = ChatRequest {
        model: Some(model.to_string()),
        messages: vec![
            Message::system(CONSOLIDATION_PROMPT),
            Message::user(&user_prompt),
        ],
        max_tokens: 4096,
        temperature: 0.3,
        ..Default::default()
    };

    // 4. Call the LLM.
    let completion = client
        .chat(&request)
        .await
        .map_err(|e| format!("Memory consolidation LLM call failed: {e}"))?;
    let consolidated = completion
        .content
        .ok_or_else(|| "Memory consolidation returned empty content".to_string())?;

    // 5. Atomic write: temp file + rename.
    let tmp_path = memory_path.with_extension("md.tmp");
    std::fs::write(&tmp_path, &consolidated)
        .map_err(|e| format!("Failed to write temp consolidation file: {e}"))?;
    std::fs::rename(&tmp_path, memory_path)
        .map_err(|e| format!("Failed to rename consolidated memory file: {e}"))?;

    let lines_after = consolidated.lines().count();
    Ok(Some((lines_before, lines_after)))
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

    // ── Consolidation tests ─────────────────────────────────────────

    #[test]
    fn consolidation_prompt_is_nonempty() {
        assert!(!CONSOLIDATION_PROMPT.is_empty());
        assert!(CONSOLIDATION_PROMPT.contains("consolidat"));
        assert!(CONSOLIDATION_PROMPT.contains("duplicate"));
        assert!(CONSOLIDATION_PROMPT.contains("Markdown"));
    }

    #[tokio::test]
    async fn consolidate_memory_missing_file() {
        // A dummy client — consolidate_memory returns before using it.
        let client = OpenRouterClient::new("fake-key").unwrap();
        let result = consolidate_memory(
            &client,
            Path::new("/nonexistent/MEMORY.md"),
            200,
            "test-model",
        )
        .await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn consolidate_memory_under_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("MEMORY.md");
        std::fs::write(&path, "# Memory\n\nSome notes.\n").unwrap();

        let client = OpenRouterClient::new("fake-key").unwrap();
        let result = consolidate_memory(&client, &path, 200, "test-model").await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }
}
