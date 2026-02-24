//! Git and coding-specific tools for the coding agent.
//!
//! Provides git-aware tools and the [`GitToolsExt`] trait for easy
//! registration on a [`ToolSet`](cinch_rs::tools::core::ToolSet).

pub mod git;

pub use git::{GitBranch, GitCheckout, GitCommit, GitDiff, GitLog, GitStatus};

// ── Tool name constants ─────────────────────────────────────────────

pub const GIT_STATUS: &str = "git_status";
pub const GIT_DIFF: &str = "git_diff";
pub const GIT_LOG: &str = "git_log";
pub const GIT_COMMIT: &str = "git_commit";
pub const GIT_BRANCH: &str = "git_branch";
pub const GIT_CHECKOUT: &str = "git_checkout";

// ── Extension trait ─────────────────────────────────────────────────

/// Extension trait for registering git tools on a
/// [`ToolSet`](cinch_rs::tools::core::ToolSet).
///
/// # Example
///
/// ```ignore
/// use cinch_rs::tools::core::ToolSet;
/// use cinch_code::tools::GitToolsExt;
///
/// let tools = ToolSet::new()
///     .with_common_tools(".")
///     .with_git_tools(".");
/// ```
pub trait GitToolsExt {
    fn with_git_tools(self, workdir: impl Into<String>) -> Self;
}

impl GitToolsExt for cinch_rs::tools::core::ToolSet {
    fn with_git_tools(self, workdir: impl Into<String>) -> Self {
        let wd = workdir.into();
        self.with(GitStatus::new(wd.clone()))
            .with(GitDiff::new(wd.clone()))
            .with(GitLog::new(wd.clone()))
            .with(GitCommit::new(wd.clone()))
            .with(GitBranch::new(wd.clone()))
            .with(GitCheckout::new(wd))
    }
}
