//! Read-before-write enforcement tracker for `edit_file` and `write_file`.
//!
//! Records a content hash for every successfully-read file path during a
//! session. The `edit_file` and `write_file` tools consult this tracker
//! before allowing mutations: if a file exists on disk but has no entry
//! here, the write is rejected with a read-first error.

use std::collections::HashMap;
use std::sync::Mutex;

/// Session-scoped read-before-write enforcement tracker.
///
/// Shared via `Arc<ReadTracker>` between `ReadFile`, `EditFile`, and
/// `WriteFile`. Every successful `read_file` call registers the file;
/// mutation tools check registration before proceeding.
pub struct ReadTracker {
    /// Map from absolute path → FNV-1a hash of last-known content.
    entries: Mutex<HashMap<String, u64>>,
}

impl ReadTracker {
    /// Create a new, empty tracker.
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Record a successful read of `abs_path` with the given `content`.
    pub fn record_read(&self, abs_path: &str, content: &str) {
        let hash = fnv1a(content);
        self.entries
            .lock()
            .unwrap()
            .insert(abs_path.to_string(), hash);
    }

    /// Record a successful write to `abs_path` with new `content`.
    ///
    /// Updates the stored hash so subsequent edits don't require re-reading.
    pub fn record_write(&self, abs_path: &str, content: &str) {
        let hash = fnv1a(content);
        self.entries
            .lock()
            .unwrap()
            .insert(abs_path.to_string(), hash);
    }

    /// Check whether `abs_path` has been read (or written) this session.
    pub fn has_been_read(&self, abs_path: &str) -> bool {
        self.entries.lock().unwrap().contains_key(abs_path)
    }

    /// Number of tracked files.
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    /// Whether the tracker is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for ReadTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// FNV-1a 64-bit hash.
pub(crate) fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in s.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_tracker_is_empty() {
        let t = ReadTracker::new();
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn record_read_registers_file() {
        let t = ReadTracker::new();
        assert!(!t.has_been_read("/tmp/foo.rs"));
        t.record_read("/tmp/foo.rs", "fn main() {}");
        assert!(t.has_been_read("/tmp/foo.rs"));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn record_write_registers_file() {
        let t = ReadTracker::new();
        t.record_write("/tmp/bar.rs", "pub mod bar;");
        assert!(t.has_been_read("/tmp/bar.rs"));
    }

    #[test]
    fn has_been_read_returns_false_for_unknown() {
        let t = ReadTracker::new();
        t.record_read("/tmp/known.rs", "x");
        assert!(!t.has_been_read("/tmp/unknown.rs"));
    }

    #[test]
    fn record_write_updates_existing_entry() {
        let t = ReadTracker::new();
        t.record_read("/tmp/f.rs", "old content");
        t.record_write("/tmp/f.rs", "new content");
        assert!(t.has_been_read("/tmp/f.rs"));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn fnv1a_deterministic() {
        let a = fnv1a("hello world");
        let b = fnv1a("hello world");
        assert_eq!(a, b);
        assert_ne!(a, fnv1a("different"));
    }
}
