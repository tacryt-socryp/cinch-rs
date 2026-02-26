//! Git tool implementations for the coding agent.
//!
//! Provides six git-aware tools that follow the cinch-rs
//! [`Tool`] trait pattern:
//!
//! | Tool | Name | Purpose |
//! |------|------|---------|
//! | [`GitStatus`] | `git_status` | Show working tree status |
//! | [`GitDiff`] | `git_diff` | Show changes between commits, index, and working tree |
//! | [`GitLog`] | `git_log` | Show commit history |
//! | [`GitCommit`] | `git_commit` | Stage files and create a commit |
//! | [`GitBranch`] | `git_branch` | List, create, or delete branches |
//! | [`GitCheckout`] | `git_checkout` | Switch branches or restore files |

use cinch_rs::ToolDef;
use cinch_rs::tools::core::{DEFAULT_MAX_RESULT_BYTES, Tool, ToolFuture, truncate_result};
use cinch_rs::tools::spec::ToolSpec;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::process::Command;

// ── Helper ──────────────────────────────────────────────────────────

/// Run a git command in the given directory and return formatted output.
async fn run_git(workdir: &str, args: &[&str]) -> String {
    let result = Command::new("git")
        .args(args)
        .current_dir(workdir)
        .output()
        .await;

    match result {
        Ok(output) => {
            let code = output.status.code().unwrap_or(-1);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            if stderr.is_empty() || output.status.success() {
                format!("[exit: {code}]\n{stdout}")
            } else {
                format!("[exit: {code}]\n{stdout}\n[stderr]\n{stderr}")
            }
        }
        Err(e) => format!("Error: failed to run git: {e}"),
    }
}

// ── GitStatus ───────────────────────────────────────────────────────

/// Arguments for `git_status`.
#[derive(Deserialize, JsonSchema)]
pub struct GitStatusArgs {
    /// Show short-format output.
    #[serde(default)]
    pub short: Option<bool>,
}

/// Show the working tree status (`git status`).
pub struct GitStatus {
    workdir: String,
}

impl GitStatus {
    pub fn new(workdir: impl Into<String>) -> Self {
        Self {
            workdir: workdir.into(),
        }
    }
}

impl Tool for GitStatus {
    fn definition(&self) -> ToolDef {
        ToolSpec::builder(super::GIT_STATUS)
            .purpose("Show the working tree status")
            .when_to_use(
                "When you need to see which files are modified, staged, or untracked \
                 in the git repository",
            )
            .when_not_to_use(
                "When you need to see the actual content of changes — use git_diff instead",
            )
            .parameters_for::<GitStatusArgs>()
            .example(
                "git_status()",
                "[exit: 0]\nOn branch main\nnothing to commit",
            )
            .example(
                "git_status(short=true)",
                "[exit: 0]\n M src/main.rs\n?? new_file.txt",
            )
            .build()
            .to_tool_def()
    }

    fn cacheable(&self) -> bool {
        true
    }

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let workdir = self.workdir.clone();
        let arguments = arguments.to_string();
        Box::pin(async move {
            let args: GitStatusArgs =
                serde_json::from_str(&arguments).unwrap_or(GitStatusArgs { short: None });

            let mut cmd_args = vec!["status"];
            if args.short.unwrap_or(false) {
                cmd_args.push("--short");
            }

            truncate_result(run_git(&workdir, &cmd_args).await, DEFAULT_MAX_RESULT_BYTES)
        })
    }
}

// ── GitDiff ─────────────────────────────────────────────────────────

/// Arguments for `git_diff`.
#[derive(Deserialize, JsonSchema)]
pub struct GitDiffArgs {
    /// Show staged changes instead of unstaged.
    #[serde(default)]
    pub staged: Option<bool>,
    /// Limit diff to a specific file or directory path.
    #[serde(default)]
    pub path: Option<String>,
}

/// Show changes between commits, index, and working tree (`git diff`).
pub struct GitDiff {
    workdir: String,
}

impl GitDiff {
    pub fn new(workdir: impl Into<String>) -> Self {
        Self {
            workdir: workdir.into(),
        }
    }
}

impl Tool for GitDiff {
    fn definition(&self) -> ToolDef {
        ToolSpec::builder(super::GIT_DIFF)
            .purpose("Show file changes (unstaged by default, or staged with --staged)")
            .when_to_use(
                "When you need to see what has changed in the working tree or staging area. \
                 Use staged=true to see what will be committed",
            )
            .when_not_to_use(
                "When you only need to know which files changed — use git_status instead",
            )
            .parameters_for::<GitDiffArgs>()
            .example("git_diff()", "[exit: 0]\ndiff --git a/file.rs ...")
            .example(
                "git_diff(staged=true)",
                "[exit: 0]\ndiff --git a/file.rs ...",
            )
            .build()
            .to_tool_def()
    }

    fn cacheable(&self) -> bool {
        true
    }

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let workdir = self.workdir.clone();
        let arguments = arguments.to_string();
        Box::pin(async move {
            let args: GitDiffArgs = serde_json::from_str(&arguments).unwrap_or(GitDiffArgs {
                staged: None,
                path: None,
            });

            let mut cmd_args = vec!["diff"];
            if args.staged.unwrap_or(false) {
                cmd_args.push("--staged");
            }

            let path_string;
            if let Some(ref p) = args.path {
                if p.contains("..") {
                    return "Error: path traversal not allowed".to_string();
                }
                cmd_args.push("--");
                path_string = p.clone();
                cmd_args.push(&path_string);
            }

            truncate_result(run_git(&workdir, &cmd_args).await, DEFAULT_MAX_RESULT_BYTES)
        })
    }
}

// ── GitLog ──────────────────────────────────────────────────────────

/// Arguments for `git_log`.
#[derive(Deserialize, JsonSchema)]
pub struct GitLogArgs {
    /// Number of commits to show. Default: 10.
    #[serde(default)]
    pub count: Option<u32>,
    /// Use one-line format.
    #[serde(default)]
    pub oneline: Option<bool>,
}

/// Show commit history (`git log`).
pub struct GitLog {
    workdir: String,
}

impl GitLog {
    pub fn new(workdir: impl Into<String>) -> Self {
        Self {
            workdir: workdir.into(),
        }
    }
}

impl Tool for GitLog {
    fn definition(&self) -> ToolDef {
        ToolSpec::builder(super::GIT_LOG)
            .purpose("Show recent commit history")
            .when_to_use(
                "When you need to see recent commits, understand the project history, \
                 or find a specific commit",
            )
            .when_not_to_use("When you need to see the content of changes — use git_diff instead")
            .parameters_for::<GitLogArgs>()
            .example(
                "git_log(count=5, oneline=true)",
                "[exit: 0]\nabc1234 Fix bug in parser\ndef5678 Add new feature",
            )
            .build()
            .to_tool_def()
    }

    fn cacheable(&self) -> bool {
        true
    }

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let workdir = self.workdir.clone();
        let arguments = arguments.to_string();
        Box::pin(async move {
            let args: GitLogArgs = serde_json::from_str(&arguments).unwrap_or(GitLogArgs {
                count: None,
                oneline: None,
            });

            let count = args.count.unwrap_or(10).min(100);
            let count_str = format!("-{count}");

            let mut cmd_args = vec!["log", &count_str];
            if args.oneline.unwrap_or(false) {
                cmd_args.push("--oneline");
            }

            truncate_result(run_git(&workdir, &cmd_args).await, DEFAULT_MAX_RESULT_BYTES)
        })
    }
}

// ── GitCommit ───────────────────────────────────────────────────────

/// Arguments for `git_commit`.
#[derive(Deserialize, JsonSchema)]
pub struct GitCommitArgs {
    /// Commit message.
    pub message: String,
    /// Files to stage before committing. If empty, commits whatever is already staged.
    #[serde(default)]
    pub paths: Option<Vec<String>>,
}

/// Stage files and create a commit (`git add` + `git commit`).
pub struct GitCommit {
    workdir: String,
}

impl GitCommit {
    pub fn new(workdir: impl Into<String>) -> Self {
        Self {
            workdir: workdir.into(),
        }
    }
}

impl Tool for GitCommit {
    fn definition(&self) -> ToolDef {
        ToolSpec::builder(super::GIT_COMMIT)
            .purpose("Stage files and create a git commit")
            .when_to_use(
                "When you need to commit changes. Provide specific file paths to stage, \
                 or omit paths to commit whatever is already staged",
            )
            .when_not_to_use(
                "Do not commit unless the user has asked you to. Never force-push or \
                 amend commits without explicit permission",
            )
            .parameters_for::<GitCommitArgs>()
            .example(
                "git_commit(message='Fix typo in README', paths=['README.md'])",
                "[exit: 0]\n[main abc1234] Fix typo in README\n 1 file changed",
            )
            .build()
            .to_tool_def()
    }

    fn is_mutation(&self) -> bool {
        true
    }

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let workdir = self.workdir.clone();
        let arguments = arguments.to_string();
        Box::pin(async move {
            let args: GitCommitArgs = match serde_json::from_str(&arguments) {
                Ok(a) => a,
                Err(_) => return "Error: 'message' argument is required".to_string(),
            };

            if args.message.is_empty() {
                return "Error: commit message must not be empty".to_string();
            }

            // Validate paths.
            if let Some(ref paths) = args.paths {
                for p in paths {
                    if p.contains("..") {
                        return "Error: path traversal not allowed".to_string();
                    }
                }
            }

            // Stage files if paths are provided.
            if let Some(ref paths) = args.paths
                && !paths.is_empty()
            {
                let path_refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
                let mut add_args = vec!["add"];
                add_args.extend(path_refs);

                let add_result = run_git(&workdir, &add_args).await;
                if add_result.contains("[exit: 1]") || add_result.starts_with("Error:") {
                    return format!("Error staging files: {add_result}");
                }
            }

            // Commit.
            let commit_result = run_git(&workdir, &["commit", "-m", &args.message]).await;
            truncate_result(commit_result, DEFAULT_MAX_RESULT_BYTES)
        })
    }
}

// ── GitBranch ───────────────────────────────────────────────────────

/// Arguments for `git_branch`.
#[derive(Deserialize, JsonSchema)]
pub struct GitBranchArgs {
    /// Create a new branch with this name (from HEAD or `start_point`).
    #[serde(default)]
    pub create: Option<String>,
    /// Starting point for the new branch (commit, tag, or branch name).
    /// Only used when `create` is set.
    #[serde(default)]
    pub start_point: Option<String>,
    /// Delete this branch.
    #[serde(default)]
    pub delete: Option<String>,
}

/// List, create, or delete branches (`git branch`).
pub struct GitBranch {
    workdir: String,
}

impl GitBranch {
    pub fn new(workdir: impl Into<String>) -> Self {
        Self {
            workdir: workdir.into(),
        }
    }
}

impl Tool for GitBranch {
    fn definition(&self) -> ToolDef {
        ToolSpec::builder(super::GIT_BRANCH)
            .purpose("List, create, or delete git branches")
            .when_to_use(
                "When you need to see existing branches, create a feature branch, \
                 or delete a merged branch",
            )
            .when_not_to_use("When you need to switch branches — use git_checkout instead")
            .parameters_for::<GitBranchArgs>()
            .example("git_branch()", "[exit: 0]\n* main\n  feature-x")
            .example("git_branch(create='feature-y')", "[exit: 0]\n")
            .example(
                "git_branch(delete='feature-x')",
                "[exit: 0]\nDeleted branch feature-x",
            )
            .build()
            .to_tool_def()
    }

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let workdir = self.workdir.clone();
        let arguments = arguments.to_string();
        Box::pin(async move {
            let args: GitBranchArgs = serde_json::from_str(&arguments).unwrap_or(GitBranchArgs {
                create: None,
                start_point: None,
                delete: None,
            });

            if let Some(ref name) = args.create {
                if name.is_empty() {
                    return "Error: branch name must not be empty".to_string();
                }
                let mut cmd_args = vec!["branch", name.as_str()];
                let sp;
                if let Some(ref start) = args.start_point {
                    sp = start.clone();
                    cmd_args.push(&sp);
                }
                return truncate_result(
                    run_git(&workdir, &cmd_args).await,
                    DEFAULT_MAX_RESULT_BYTES,
                );
            }

            if let Some(ref name) = args.delete {
                if name.is_empty() {
                    return "Error: branch name must not be empty".to_string();
                }
                // Use -d (safe delete) — refuses to delete unmerged branches.
                return truncate_result(
                    run_git(&workdir, &["branch", "-d", name]).await,
                    DEFAULT_MAX_RESULT_BYTES,
                );
            }

            // Default: list branches.
            truncate_result(
                run_git(&workdir, &["branch", "--list", "-v"]).await,
                DEFAULT_MAX_RESULT_BYTES,
            )
        })
    }
}

// ── GitCheckout ─────────────────────────────────────────────────────

/// Arguments for `git_checkout`.
#[derive(Deserialize, JsonSchema)]
pub struct GitCheckoutArgs {
    /// Branch name or commit to switch to.
    pub target: String,
    /// Create a new branch and switch to it (like `git checkout -b`).
    #[serde(default)]
    pub create: Option<bool>,
}

/// Switch branches or restore files (`git checkout` / `git switch`).
pub struct GitCheckout {
    workdir: String,
}

impl GitCheckout {
    pub fn new(workdir: impl Into<String>) -> Self {
        Self {
            workdir: workdir.into(),
        }
    }
}

impl Tool for GitCheckout {
    fn definition(&self) -> ToolDef {
        ToolSpec::builder(super::GIT_CHECKOUT)
            .purpose("Switch to a different branch or create and switch to a new branch")
            .when_to_use(
                "When you need to switch to an existing branch or create a new feature branch \
                 and switch to it. Use create=true to create the branch if it doesn't exist",
            )
            .when_not_to_use(
                "Do not switch branches without the user's permission. This modifies the \
                 working tree and could discard uncommitted changes",
            )
            .parameters_for::<GitCheckoutArgs>()
            .example(
                "git_checkout(target='feature-x')",
                "[exit: 0]\nSwitched to branch 'feature-x'",
            )
            .example(
                "git_checkout(target='feature-y', create=true)",
                "[exit: 0]\nSwitched to a new branch 'feature-y'",
            )
            .build()
            .to_tool_def()
    }

    fn is_mutation(&self) -> bool {
        true
    }

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let workdir = self.workdir.clone();
        let arguments = arguments.to_string();
        Box::pin(async move {
            let args: GitCheckoutArgs = match serde_json::from_str(&arguments) {
                Ok(a) => a,
                Err(_) => return "Error: 'target' argument is required".to_string(),
            };

            if args.target.is_empty() {
                return "Error: target must not be empty".to_string();
            }

            let mut cmd_args = vec!["checkout"];
            if args.create.unwrap_or(false) {
                cmd_args.push("-b");
            }
            cmd_args.push(&args.target);

            truncate_result(run_git(&workdir, &cmd_args).await, DEFAULT_MAX_RESULT_BYTES)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_status_definition() {
        let tool = GitStatus::new("/tmp");
        let def = tool.definition();
        assert_eq!(def.function.name, "git_status");
        assert!(tool.cacheable());
        assert!(!tool.is_mutation());
    }

    #[test]
    fn git_diff_definition() {
        let tool = GitDiff::new("/tmp");
        let def = tool.definition();
        assert_eq!(def.function.name, "git_diff");
        assert!(tool.cacheable());
        assert!(!tool.is_mutation());
    }

    #[test]
    fn git_log_definition() {
        let tool = GitLog::new("/tmp");
        let def = tool.definition();
        assert_eq!(def.function.name, "git_log");
        assert!(tool.cacheable());
        assert!(!tool.is_mutation());
    }

    #[test]
    fn git_commit_definition() {
        let tool = GitCommit::new("/tmp");
        let def = tool.definition();
        assert_eq!(def.function.name, "git_commit");
        assert!(!tool.cacheable());
        assert!(tool.is_mutation());
    }

    #[tokio::test]
    async fn git_commit_rejects_empty_message() {
        let tool = GitCommit::new("/tmp");
        let result = tool.execute(r#"{"message":""}"#).await;
        assert!(result.contains("Error"));
    }

    #[tokio::test]
    async fn git_diff_rejects_path_traversal() {
        let tool = GitDiff::new("/tmp");
        let result = tool.execute(r#"{"path":"../../etc/passwd"}"#).await;
        assert!(result.contains("Error"));
    }

    #[tokio::test]
    async fn git_commit_rejects_path_traversal() {
        let tool = GitCommit::new("/tmp");
        let result = tool
            .execute(r#"{"message":"test","paths":["../../etc/passwd"]}"#)
            .await;
        assert!(result.contains("Error"));
    }

    #[test]
    fn git_branch_definition() {
        let tool = GitBranch::new("/tmp");
        let def = tool.definition();
        assert_eq!(def.function.name, "git_branch");
        assert!(!tool.cacheable());
        assert!(!tool.is_mutation());
    }

    #[tokio::test]
    async fn git_branch_create_rejects_empty_name() {
        let tool = GitBranch::new("/tmp");
        let result = tool.execute(r#"{"create":""}"#).await;
        assert!(result.contains("Error"));
    }

    #[tokio::test]
    async fn git_branch_delete_rejects_empty_name() {
        let tool = GitBranch::new("/tmp");
        let result = tool.execute(r#"{"delete":""}"#).await;
        assert!(result.contains("Error"));
    }

    #[test]
    fn git_checkout_definition() {
        let tool = GitCheckout::new("/tmp");
        let def = tool.definition();
        assert_eq!(def.function.name, "git_checkout");
        assert!(!tool.cacheable());
        assert!(tool.is_mutation());
    }

    #[tokio::test]
    async fn git_checkout_rejects_empty_target() {
        let tool = GitCheckout::new("/tmp");
        let result = tool.execute(r#"{"target":""}"#).await;
        assert!(result.contains("Error"));
    }

    #[tokio::test]
    async fn git_checkout_rejects_missing_target() {
        let tool = GitCheckout::new("/tmp");
        let result = tool.execute(r#"{}"#).await;
        assert!(result.contains("Error"));
    }
}
