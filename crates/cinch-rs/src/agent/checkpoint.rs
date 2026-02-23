//! Checkpoint and resume for long-running agent loops.
//!
//! Serializes harness state to disk after each round, enabling recovery
//! from crashes or interruptions. On resume, loads the checkpoint and
//! continues from the last completed round.

use crate::Message;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Serializable checkpoint of harness state.
#[derive(Serialize, Deserialize, Debug)]
pub struct Checkpoint {
    /// Trace ID for the run.
    pub trace_id: String,
    /// All messages up to the checkpoint.
    pub messages: Vec<Message>,
    /// Text output accumulated so far.
    pub text_output: Vec<String>,
    /// Current round number.
    pub round: u32,
    /// Total prompt tokens consumed.
    pub total_prompt_tokens: u32,
    /// Total completion tokens consumed.
    pub total_completion_tokens: u32,
    /// Estimated cost so far.
    pub estimated_cost_usd: f64,
    /// Timestamp of the checkpoint.
    pub timestamp: String,
}

/// Configuration for checkpoint behavior.
#[derive(Debug, Clone)]
pub struct CheckpointConfig {
    /// Directory to store checkpoint files.
    pub checkpoint_dir: PathBuf,
    /// Whether to checkpoint after every round.
    pub checkpoint_every_round: bool,
    /// Whether to clean up checkpoints on successful completion.
    pub cleanup_on_success: bool,
}

impl CheckpointConfig {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            checkpoint_dir: dir.into(),
            checkpoint_every_round: true,
            cleanup_on_success: true,
        }
    }
}

/// Manager for checkpoint operations.
pub struct CheckpointManager {
    config: CheckpointConfig,
}

impl CheckpointManager {
    pub fn new(config: CheckpointConfig) -> std::io::Result<Self> {
        std::fs::create_dir_all(&config.checkpoint_dir)?;
        Ok(Self { config })
    }

    /// Save a checkpoint to disk.
    pub fn save(&self, checkpoint: &Checkpoint) -> Result<PathBuf, String> {
        let filename = format!(
            "checkpoint-{}-r{}.json",
            checkpoint.trace_id, checkpoint.round
        );
        let path = self.config.checkpoint_dir.join(filename);

        let json = serde_json::to_string_pretty(checkpoint)
            .map_err(|e| format!("Failed to serialize checkpoint: {e}"))?;
        std::fs::write(&path, json).map_err(|e| format!("Failed to write checkpoint: {e}"))?;

        Ok(path)
    }

    /// Load the latest checkpoint for a given trace ID.
    pub fn load_latest(&self, trace_id: &str) -> Result<Option<Checkpoint>, String> {
        let mut latest: Option<(u32, PathBuf)> = None;

        let entries = std::fs::read_dir(&self.config.checkpoint_dir)
            .map_err(|e| format!("Failed to read checkpoint dir: {e}"))?;

        for entry in entries {
            let entry = entry.map_err(|e| format!("Failed to read entry: {e}"))?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&format!("checkpoint-{trace_id}-r")) && name.ends_with(".json") {
                // Extract round number.
                if let Some(round_str) = name
                    .strip_prefix(&format!("checkpoint-{trace_id}-r"))
                    .and_then(|s| s.strip_suffix(".json"))
                    && let Ok(round) = round_str.parse::<u32>()
                    && latest.as_ref().is_none_or(|(r, _)| round > *r)
                {
                    latest = Some((round, entry.path()));
                }
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

    /// Clean up all checkpoints for a trace ID.
    pub fn cleanup(&self, trace_id: &str) -> Result<usize, String> {
        let mut count = 0;
        let entries = std::fs::read_dir(&self.config.checkpoint_dir)
            .map_err(|e| format!("Failed to read checkpoint dir: {e}"))?;

        for entry in entries {
            let entry = entry.map_err(|e| format!("Failed to read entry: {e}"))?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&format!("checkpoint-{trace_id}")) {
                std::fs::remove_file(entry.path())
                    .map_err(|e| format!("Failed to remove checkpoint: {e}"))?;
                count += 1;
            }
        }

        Ok(count)
    }

    /// Get the checkpoint directory path.
    pub fn dir(&self) -> &Path {
        &self.config.checkpoint_dir
    }
}

/// Adaptive round limits: dynamically adjust max_rounds based on progress.
#[derive(Debug, Clone)]
pub struct AdaptiveRoundLimit {
    /// Initial maximum rounds.
    pub initial_max: u32,
    /// Current maximum (may be adjusted).
    pub current_max: u32,
    /// Maximum absolute limit (never exceed this).
    pub absolute_max: u32,
    /// Minimum rounds of progress before considering extension.
    pub min_progress_rounds: u32,
}

impl AdaptiveRoundLimit {
    pub fn new(initial_max: u32, absolute_max: u32) -> Self {
        Self {
            initial_max,
            current_max: initial_max,
            absolute_max,
            min_progress_rounds: 3,
        }
    }

    /// Request more rounds. Returns the new limit if approved.
    /// Requires evidence of progress (tool calls, drafts saved, etc.).
    pub fn request_extension(&mut self, rounds_used: u32, has_progress: bool) -> Option<u32> {
        if !has_progress || rounds_used < self.min_progress_rounds {
            return None;
        }

        // Grant 50% more rounds, up to absolute max.
        let extension = (self.current_max as f64 * 0.5).ceil() as u32;
        let new_max = (self.current_max + extension).min(self.absolute_max);

        if new_max > self.current_max {
            self.current_max = new_max;
            Some(new_max)
        } else {
            None
        }
    }

    /// Check if the current round is within limits.
    pub fn is_within_limit(&self, round: u32) -> bool {
        round < self.current_max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let config = CheckpointConfig::new(dir.path());
        let manager = CheckpointManager::new(config).unwrap();

        let checkpoint = Checkpoint {
            trace_id: "tr-test".into(),
            messages: vec![Message::user("hello")],
            text_output: vec!["world".into()],
            round: 3,
            total_prompt_tokens: 100,
            total_completion_tokens: 50,
            estimated_cost_usd: 0.001,
            timestamp: "2026-01-01T00:00:00Z".into(),
        };

        manager.save(&checkpoint).unwrap();
        let loaded = manager.load_latest("tr-test").unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().round, 3);
    }

    #[test]
    fn checkpoint_load_latest_picks_highest_round() {
        let dir = tempfile::tempdir().unwrap();
        let config = CheckpointConfig::new(dir.path());
        let manager = CheckpointManager::new(config).unwrap();

        for round in [1, 5, 3] {
            let checkpoint = Checkpoint {
                trace_id: "tr-multi".into(),
                messages: vec![],
                text_output: vec![],
                round,
                total_prompt_tokens: 0,
                total_completion_tokens: 0,
                estimated_cost_usd: 0.0,
                timestamp: "2026-01-01".into(),
            };
            manager.save(&checkpoint).unwrap();
        }

        let latest = manager.load_latest("tr-multi").unwrap().unwrap();
        assert_eq!(latest.round, 5);
    }

    #[test]
    fn checkpoint_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let config = CheckpointConfig::new(dir.path());
        let manager = CheckpointManager::new(config).unwrap();

        let checkpoint = Checkpoint {
            trace_id: "tr-clean".into(),
            messages: vec![],
            text_output: vec![],
            round: 1,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            estimated_cost_usd: 0.0,
            timestamp: "2026-01-01".into(),
        };
        manager.save(&checkpoint).unwrap();

        let count = manager.cleanup("tr-clean").unwrap();
        assert_eq!(count, 1);
        assert!(manager.load_latest("tr-clean").unwrap().is_none());
    }

    #[test]
    fn adaptive_round_limit() {
        let mut limit = AdaptiveRoundLimit::new(10, 30);
        assert!(limit.is_within_limit(5));
        assert!(limit.is_within_limit(9));
        assert!(!limit.is_within_limit(10));

        // Extend with progress.
        let new = limit.request_extension(5, true);
        assert!(new.is_some());
        assert_eq!(limit.current_max, 15);

        // Can extend again.
        let new2 = limit.request_extension(12, true);
        assert!(new2.is_some());

        // No extension without progress.
        let no_ext = limit.request_extension(20, false);
        assert!(no_ext.is_none());
    }

    #[test]
    fn adaptive_limit_respects_absolute_max() {
        let mut limit = AdaptiveRoundLimit::new(25, 30);
        limit.request_extension(20, true);
        // Should be capped at 30.
        assert!(limit.current_max <= 30);
    }
}
