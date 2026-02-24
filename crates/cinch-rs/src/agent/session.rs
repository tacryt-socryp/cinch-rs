//! Per-session directories with manifests.
//!
//! Each session gets its own directory under `.agents/sessions/`, containing
//! a lightweight `manifest.json` and per-round checkpoint files. The
//! [`SessionManager`] subsumes all [`CheckpointManager`](super::checkpoint)
//! functionality plus manifest management.

use crate::agent::checkpoint::Checkpoint;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::warn;

// ── SessionManifest ────────────────────────────────────────────────

/// Lightweight metadata for a session, stored as `manifest.json`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SessionManifest {
    /// Trace ID for the session (also the directory name).
    pub trace_id: String,
    /// Optional human-readable title.
    pub title: Option<String>,
    /// Model used for this session.
    pub model: String,
    /// Current session status.
    pub status: SessionStatus,
    /// Unix epoch seconds when the session was created.
    pub created_at: u64,
    /// Unix epoch seconds of the last update.
    pub updated_at: u64,
    /// Last completed round number.
    pub last_round: u32,
    /// Cumulative prompt tokens.
    pub total_prompt_tokens: u32,
    /// Cumulative completion tokens.
    pub total_completion_tokens: u32,
    /// Estimated cost in USD.
    pub estimated_cost_usd: f64,
    /// First ~200 chars of the first user message.
    pub message_preview: String,
}

/// Status of a session.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Running,
    Completed,
    Interrupted,
}

// ── SessionManager ─────────────────────────────────────────────────

/// Manager for per-session directories, manifests, and round checkpoints.
///
/// Directory layout:
/// ```text
/// sessions_dir/
///   tr-abc123/
///     manifest.json
///     round-001.json
///     round-003.json
/// ```
pub struct SessionManager {
    sessions_dir: PathBuf,
}

impl SessionManager {
    /// Create a new manager, ensuring the root sessions directory exists.
    pub fn new(sessions_dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let sessions_dir = sessions_dir.into();
        std::fs::create_dir_all(&sessions_dir)?;
        Ok(Self { sessions_dir })
    }

    /// Get the sessions root directory.
    pub fn dir(&self) -> &Path {
        &self.sessions_dir
    }

    /// Path to a session's directory.
    fn session_dir(&self, trace_id: &str) -> PathBuf {
        self.sessions_dir.join(trace_id)
    }

    /// Path to a session's manifest file.
    fn manifest_path(&self, trace_id: &str) -> PathBuf {
        self.session_dir(trace_id).join("manifest.json")
    }

    /// Checkpoint filename for a given round (zero-padded).
    fn round_filename(round: u32) -> String {
        format!("round-{round:03}.json")
    }

    // ── Manifest operations ────────────────────────────────────────

    /// Atomic write: serialize to a temp file, then rename into place.
    pub fn save_manifest(&self, manifest: &SessionManifest) -> Result<(), String> {
        let dir = self.session_dir(&manifest.trace_id);
        std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create session dir: {e}"))?;

        let final_path = dir.join("manifest.json");
        let tmp_path = dir.join(".manifest.json.tmp");

        let json = serde_json::to_string_pretty(manifest)
            .map_err(|e| format!("Failed to serialize manifest: {e}"))?;
        std::fs::write(&tmp_path, json)
            .map_err(|e| format!("Failed to write temp manifest: {e}"))?;
        std::fs::rename(&tmp_path, &final_path)
            .map_err(|e| format!("Failed to rename manifest: {e}"))?;

        Ok(())
    }

    /// Load a session's manifest. Returns `None` if the session doesn't exist.
    pub fn load_manifest(&self, trace_id: &str) -> Result<Option<SessionManifest>, String> {
        let path = self.manifest_path(trace_id);
        if !path.exists() {
            return Ok(None);
        }
        let json =
            std::fs::read_to_string(&path).map_err(|e| format!("Failed to read manifest: {e}"))?;
        let manifest: SessionManifest =
            serde_json::from_str(&json).map_err(|e| format!("Failed to parse manifest: {e}"))?;
        Ok(Some(manifest))
    }

    /// List all sessions by reading each subdirectory's `manifest.json`.
    pub fn list_sessions(&self) -> Result<Vec<SessionManifest>, String> {
        let entries = std::fs::read_dir(&self.sessions_dir)
            .map_err(|e| format!("Failed to read sessions dir: {e}"))?;

        let mut manifests = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| format!("Failed to read entry: {e}"))?;
            if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                continue;
            }
            let manifest_path = entry.path().join("manifest.json");
            if !manifest_path.exists() {
                continue;
            }
            match std::fs::read_to_string(&manifest_path) {
                Ok(json) => match serde_json::from_str::<SessionManifest>(&json) {
                    Ok(m) => manifests.push(m),
                    Err(e) => {
                        warn!(
                            "Skipping malformed manifest at {}: {e}",
                            manifest_path.display()
                        );
                    }
                },
                Err(e) => {
                    warn!(
                        "Skipping unreadable manifest at {}: {e}",
                        manifest_path.display()
                    );
                }
            }
        }
        Ok(manifests)
    }

    // ── Checkpoint operations ──────────────────────────────────────

    /// Save a checkpoint to `{trace_id}/round-{NNN}.json`.
    pub fn save_checkpoint(&self, checkpoint: &Checkpoint) -> Result<PathBuf, String> {
        let dir = self.session_dir(&checkpoint.trace_id);
        std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create session dir: {e}"))?;

        let filename = Self::round_filename(checkpoint.round);
        let path = dir.join(filename);

        let json = serde_json::to_string_pretty(checkpoint)
            .map_err(|e| format!("Failed to serialize checkpoint: {e}"))?;
        std::fs::write(&path, json).map_err(|e| format!("Failed to write checkpoint: {e}"))?;

        Ok(path)
    }

    /// Load the latest (highest round) checkpoint for a session.
    pub fn load_latest_checkpoint(&self, trace_id: &str) -> Result<Option<Checkpoint>, String> {
        let dir = self.session_dir(trace_id);
        if !dir.exists() {
            return Ok(None);
        }

        let mut latest: Option<(u32, PathBuf)> = None;

        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("Failed to read session dir: {e}"))?;

        for entry in entries {
            let entry = entry.map_err(|e| format!("Failed to read entry: {e}"))?;
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(round_str) = name
                .strip_prefix("round-")
                .and_then(|s| s.strip_suffix(".json"))
                && let Ok(round) = round_str.parse::<u32>()
                && latest.as_ref().is_none_or(|(r, _)| round > *r)
            {
                latest = Some((round, entry.path()));
            }
        }

        match latest {
            Some((_, path)) => {
                let json = std::fs::read_to_string(&path)
                    .map_err(|e| format!("Failed to read checkpoint: {e}"))?;
                let checkpoint: Checkpoint = serde_json::from_str(&json)
                    .map_err(|e| format!("Failed to parse checkpoint: {e}"))?;
                Ok(Some(checkpoint))
            }
            None => Ok(None),
        }
    }

    /// Delete round checkpoint files but keep the manifest.
    /// Returns the number of files deleted.
    pub fn cleanup_checkpoints(&self, trace_id: &str) -> Result<usize, String> {
        let dir = self.session_dir(trace_id);
        if !dir.exists() {
            return Ok(0);
        }

        let mut count = 0;
        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("Failed to read session dir: {e}"))?;

        for entry in entries {
            let entry = entry.map_err(|e| format!("Failed to read entry: {e}"))?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("round-") && name.ends_with(".json") {
                std::fs::remove_file(entry.path())
                    .map_err(|e| format!("Failed to remove checkpoint: {e}"))?;
                count += 1;
            }
        }

        Ok(count)
    }

    /// Delete the entire session directory (manifest + all checkpoints).
    pub fn delete_session(&self, trace_id: &str) -> Result<(), String> {
        let dir = self.session_dir(trace_id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .map_err(|e| format!("Failed to delete session dir: {e}"))?;
        }
        Ok(())
    }
}

// ── Helper ─────────────────────────────────────────────────────────

/// Current unix epoch in seconds.
pub fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Extract the first ~200 characters of the first user message.
pub fn extract_message_preview(messages: &[crate::Message]) -> String {
    for msg in messages {
        if matches!(msg.role, crate::MessageRole::User)
            && let Some(ref content) = msg.content
        {
            let preview: String = content.chars().take(200).collect();
            return preview;
        }
    }
    String::new()
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Message;

    fn make_test_manifest(trace_id: &str) -> SessionManifest {
        SessionManifest {
            trace_id: trace_id.into(),
            title: None,
            model: "test-model".into(),
            status: SessionStatus::Running,
            created_at: 1000,
            updated_at: 1000,
            last_round: 0,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            estimated_cost_usd: 0.0,
            message_preview: "hello world".into(),
        }
    }

    fn make_test_checkpoint(trace_id: &str, round: u32) -> Checkpoint {
        Checkpoint {
            trace_id: trace_id.into(),
            messages: vec![Message::user("test")],
            text_output: vec!["output".into()],
            round,
            total_prompt_tokens: 100,
            total_completion_tokens: 50,
            estimated_cost_usd: 0.001,
            timestamp: "epoch:1000".into(),
        }
    }

    #[test]
    fn manifest_save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SessionManager::new(dir.path()).unwrap();

        let manifest = make_test_manifest("tr-abc");
        mgr.save_manifest(&manifest).unwrap();

        let loaded = mgr.load_manifest("tr-abc").unwrap().unwrap();
        assert_eq!(loaded.trace_id, "tr-abc");
        assert_eq!(loaded.model, "test-model");
        assert_eq!(loaded.status, SessionStatus::Running);
    }

    #[test]
    fn list_sessions_returns_all_manifests() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SessionManager::new(dir.path()).unwrap();

        mgr.save_manifest(&make_test_manifest("tr-aaa")).unwrap();
        mgr.save_manifest(&make_test_manifest("tr-bbb")).unwrap();

        let sessions = mgr.list_sessions().unwrap();
        assert_eq!(sessions.len(), 2);
        let ids: Vec<&str> = sessions.iter().map(|s| s.trace_id.as_str()).collect();
        assert!(ids.contains(&"tr-aaa"));
        assert!(ids.contains(&"tr-bbb"));
    }

    #[test]
    fn checkpoint_save_load_within_session() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SessionManager::new(dir.path()).unwrap();

        let cp1 = make_test_checkpoint("tr-sess", 1);
        let cp3 = make_test_checkpoint("tr-sess", 3);
        mgr.save_checkpoint(&cp1).unwrap();
        mgr.save_checkpoint(&cp3).unwrap();

        let latest = mgr.load_latest_checkpoint("tr-sess").unwrap().unwrap();
        assert_eq!(latest.round, 3);
    }

    #[test]
    fn cleanup_checkpoints_removes_rounds_preserves_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SessionManager::new(dir.path()).unwrap();

        mgr.save_manifest(&make_test_manifest("tr-clean")).unwrap();
        mgr.save_checkpoint(&make_test_checkpoint("tr-clean", 1))
            .unwrap();
        mgr.save_checkpoint(&make_test_checkpoint("tr-clean", 2))
            .unwrap();

        let count = mgr.cleanup_checkpoints("tr-clean").unwrap();
        assert_eq!(count, 2);

        // Manifest still exists.
        assert!(mgr.load_manifest("tr-clean").unwrap().is_some());
        // No more checkpoints.
        assert!(mgr.load_latest_checkpoint("tr-clean").unwrap().is_none());
    }

    #[test]
    fn delete_session_removes_entire_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SessionManager::new(dir.path()).unwrap();

        mgr.save_manifest(&make_test_manifest("tr-del")).unwrap();
        mgr.save_checkpoint(&make_test_checkpoint("tr-del", 1))
            .unwrap();

        mgr.delete_session("tr-del").unwrap();
        assert!(mgr.load_manifest("tr-del").unwrap().is_none());
        assert!(!mgr.session_dir("tr-del").exists());
    }

    #[test]
    fn atomic_manifest_write_no_temp_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SessionManager::new(dir.path()).unwrap();

        mgr.save_manifest(&make_test_manifest("tr-atomic")).unwrap();

        // The temp file should not exist after a successful write.
        let tmp = mgr.session_dir("tr-atomic").join(".manifest.json.tmp");
        assert!(!tmp.exists());
    }

    #[test]
    fn missing_session_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SessionManager::new(dir.path()).unwrap();

        assert!(mgr.load_manifest("nonexistent").unwrap().is_none());
        assert!(mgr.load_latest_checkpoint("nonexistent").unwrap().is_none());
    }

    #[test]
    fn session_status_serde_roundtrip() {
        let json = serde_json::to_string(&SessionStatus::Interrupted).unwrap();
        assert_eq!(json, "\"interrupted\"");
        let parsed: SessionStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, SessionStatus::Interrupted);
    }

    #[test]
    fn extract_message_preview_basic() {
        let msgs = vec![
            Message::system("sys"),
            Message::user("Hello, this is a test message for preview extraction."),
        ];
        let preview = extract_message_preview(&msgs);
        assert!(preview.starts_with("Hello"));
        assert!(preview.len() <= 200);
    }

    #[test]
    fn extract_message_preview_empty() {
        let msgs: Vec<Message> = vec![];
        assert!(extract_message_preview(&msgs).is_empty());
    }

    #[test]
    fn extract_message_preview_truncates_long() {
        let long_msg = "a".repeat(500);
        let msgs = vec![Message::user(&long_msg)];
        let preview = extract_message_preview(&msgs);
        assert_eq!(preview.len(), 200);
    }

    #[test]
    fn cleanup_nonexistent_session_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SessionManager::new(dir.path()).unwrap();
        assert_eq!(mgr.cleanup_checkpoints("nope").unwrap(), 0);
    }

    #[test]
    fn delete_nonexistent_session_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SessionManager::new(dir.path()).unwrap();
        mgr.delete_session("nope").unwrap(); // Should not error.
    }
}
