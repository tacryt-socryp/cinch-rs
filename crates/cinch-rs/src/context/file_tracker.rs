//! Recently-accessed file preservation for context compaction.
//!
//! When compaction runs, the summarizer compresses middle-zone messages into a
//! short summary, losing details about files the agent was recently working
//! with. The [`FileAccessTracker`] records file paths extracted from tool call
//! arguments and builds a preservation note that gets injected into the
//! compaction input so the summarizer retains awareness of recent files.

use std::collections::VecDeque;

/// Tracks recently-accessed files for preservation through compaction.
///
/// File accesses are recorded from tool call arguments (e.g. `read_file`,
/// `grep`, `list_files`, `find_files`). When compaction runs, the tracker
/// builds a human-readable note listing the most recently accessed files
/// so the summarizer can include them in its output.
pub struct FileAccessTracker {
    recent_files: VecDeque<FileAccess>,
    max_preserved: usize,
}

struct FileAccess {
    path: String,
    round: usize,
    access_type: FileAccessType,
}

/// The type of file access recorded by the tracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAccessType {
    Read,
    Write,
    Search,
}

impl std::fmt::Display for FileAccessType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileAccessType::Read => write!(f, "read"),
            FileAccessType::Write => write!(f, "write"),
            FileAccessType::Search => write!(f, "search"),
        }
    }
}

impl FileAccessTracker {
    /// Create a new tracker that preserves up to `max_preserved` recent files.
    pub fn new(max_preserved: usize) -> Self {
        Self {
            recent_files: VecDeque::with_capacity(max_preserved),
            max_preserved,
        }
    }

    /// Record a file access from a tool call.
    ///
    /// Parses JSON arguments to extract file paths. Recognized tools:
    /// - `read_file` → [`FileAccessType::Read`]
    /// - `write_file`, `edit_file` → [`FileAccessType::Write`]
    /// - `list_files`, `grep`, `find_files` → [`FileAccessType::Search`]
    ///
    /// Deduplicates by path: if the file was already tracked, its entry is
    /// moved to the end with the updated round and access type.
    pub fn record_tool_access(&mut self, tool_name: &str, arguments: &str, round: usize) {
        let access_type = match tool_name {
            "read_file" => FileAccessType::Read,
            "write_file" | "edit_file" => FileAccessType::Write,
            "list_files" | "grep" | "find_files" => FileAccessType::Search,
            _ => return,
        };

        let path = match extract_path(arguments) {
            Some(p) => p,
            None => return,
        };

        // Deduplicate: remove existing entry for this path.
        self.recent_files.retain(|f| f.path != path);

        // Push new entry.
        self.recent_files.push_back(FileAccess {
            path,
            round,
            access_type,
        });

        // Cap at max_preserved.
        while self.recent_files.len() > self.max_preserved {
            self.recent_files.pop_front();
        }
    }

    /// Build a preservation note for injection into compaction input.
    ///
    /// Returns an empty string if no files are tracked.
    pub fn build_preservation_note(&self) -> String {
        if self.recent_files.is_empty() {
            return String::new();
        }

        let mut note = String::from("Recently accessed files (preserve awareness of these):\n");
        for access in &self.recent_files {
            note.push_str(&format!(
                "- {} ({}, round {})\n",
                access.path, access.access_type, access.round
            ));
        }
        note
    }
}

/// Extract a file path from JSON tool arguments.
///
/// Tries common keys: `path`, `file_path`, `file`, `pattern` (for grep/search).
fn extract_path(arguments: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(arguments).ok()?;
    let obj = value.as_object()?;

    for key in &["path", "file_path", "file", "pattern"] {
        if let Some(v) = obj.get(*key).and_then(|v| v.as_str())
            && !v.is_empty()
        {
            return Some(v.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_tracker_is_empty() {
        let tracker = FileAccessTracker::new(5);
        assert_eq!(tracker.build_preservation_note(), "");
    }

    #[test]
    fn record_read_file() {
        let mut tracker = FileAccessTracker::new(5);
        tracker.record_tool_access("read_file", r#"{"path":"src/main.rs"}"#, 1);

        let note = tracker.build_preservation_note();
        assert!(note.contains("src/main.rs"));
        assert!(note.contains("read"));
    }

    #[test]
    fn record_write_file() {
        let mut tracker = FileAccessTracker::new(5);
        tracker.record_tool_access("write_file", r#"{"path":"src/lib.rs"}"#, 2);

        let note = tracker.build_preservation_note();
        assert!(note.contains("src/lib.rs"));
        assert!(note.contains("write"));
    }

    #[test]
    fn record_search_tools() {
        let mut tracker = FileAccessTracker::new(5);
        tracker.record_tool_access("grep", r#"{"pattern":"TODO"}"#, 1);
        tracker.record_tool_access("list_files", r#"{"path":"src/"}"#, 2);
        tracker.record_tool_access("find_files", r#"{"path":"tests/"}"#, 3);

        let note = tracker.build_preservation_note();
        assert!(note.contains("TODO"));
        assert!(note.contains("src/"));
        assert!(note.contains("tests/"));
    }

    #[test]
    fn deduplication_moves_to_end() {
        let mut tracker = FileAccessTracker::new(5);
        tracker.record_tool_access("read_file", r#"{"path":"a.rs"}"#, 1);
        tracker.record_tool_access("read_file", r#"{"path":"b.rs"}"#, 2);
        tracker.record_tool_access("read_file", r#"{"path":"a.rs"}"#, 3);

        // a.rs should appear after b.rs (moved to end).
        let note = tracker.build_preservation_note();
        let a_pos = note.find("a.rs").unwrap();
        let b_pos = note.find("b.rs").unwrap();
        assert!(
            b_pos < a_pos,
            "a.rs should be after b.rs (dedup moves to end)"
        );
    }

    #[test]
    fn deduplication_updates_round_and_type() {
        let mut tracker = FileAccessTracker::new(5);
        tracker.record_tool_access("read_file", r#"{"path":"a.rs"}"#, 1);
        tracker.record_tool_access("write_file", r#"{"path":"a.rs"}"#, 5);

        let note = tracker.build_preservation_note();
        assert!(note.contains("write"));
        assert!(note.contains("round 5"));
        // Should only appear once.
        assert_eq!(note.matches("a.rs").count(), 1);
    }

    #[test]
    fn max_capacity_evicts_oldest() {
        let mut tracker = FileAccessTracker::new(3);
        tracker.record_tool_access("read_file", r#"{"path":"a.rs"}"#, 1);
        tracker.record_tool_access("read_file", r#"{"path":"b.rs"}"#, 2);
        tracker.record_tool_access("read_file", r#"{"path":"c.rs"}"#, 3);
        tracker.record_tool_access("read_file", r#"{"path":"d.rs"}"#, 4);

        let note = tracker.build_preservation_note();
        // a.rs should have been evicted.
        assert!(!note.contains("a.rs"));
        assert!(note.contains("b.rs"));
        assert!(note.contains("c.rs"));
        assert!(note.contains("d.rs"));
    }

    #[test]
    fn unknown_tools_ignored() {
        let mut tracker = FileAccessTracker::new(5);
        tracker.record_tool_access("unknown_tool", r#"{"path":"secret.rs"}"#, 1);
        tracker.record_tool_access("execute_command", r#"{"command":"ls"}"#, 2);

        assert_eq!(tracker.build_preservation_note(), "");
    }

    #[test]
    fn invalid_json_ignored() {
        let mut tracker = FileAccessTracker::new(5);
        tracker.record_tool_access("read_file", "not json at all", 1);
        tracker.record_tool_access("read_file", r#"{"no_path_key": true}"#, 2);

        assert_eq!(tracker.build_preservation_note(), "");
    }

    #[test]
    fn file_path_key_variant() {
        let mut tracker = FileAccessTracker::new(5);
        tracker.record_tool_access("read_file", r#"{"file_path":"config.toml"}"#, 1);

        let note = tracker.build_preservation_note();
        assert!(note.contains("config.toml"));
    }

    #[test]
    fn edit_file_is_write() {
        let mut tracker = FileAccessTracker::new(5);
        tracker.record_tool_access("edit_file", r#"{"path":"src/mod.rs"}"#, 1);

        let note = tracker.build_preservation_note();
        assert!(note.contains("write"));
    }

    #[test]
    fn note_format() {
        let mut tracker = FileAccessTracker::new(5);
        tracker.record_tool_access("read_file", r#"{"path":"main.rs"}"#, 3);

        let note = tracker.build_preservation_note();
        assert!(note.starts_with("Recently accessed files"));
        assert!(note.contains("- main.rs (read, round 3)"));
    }
}
