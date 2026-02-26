//! Reusable tool implementations for LLM agents.
//!
//! These tools are generic filesystem/shell operations that any agent can
//! register in its [`ToolSet`](crate::tools::core::ToolSet). Each tool takes a
//! `workdir` root and optional configuration, and can be used as-is or
//! customized via builder methods.
//!
//! # Available tools
//!
//! | Tool | Name | Purpose |
//! |------|------|---------|
//! | [`ReadFile`] | `read_file` | Read a single file |
//! | [`EditFile`] | `edit_file` | Edit a file by replacing an exact string |
//! | [`WriteFile`] | `write_file` | Create or overwrite a file |
//! | [`ListDir`] | `list_dir` | List a directory tree |
//! | [`Grep`] | `grep` | Regex search in files |
//! | [`FindFiles`] | `find_files` | Glob-based file search |
//! | [`Shell`] | `shell` | Execute shell commands |
//! | [`WebSearch`] | `web_search` | Search the web via Brave Search API |
//!
//! # Example
//!
//! ```ignore
//! use cinch_rs::common_tools::*;
//! use cinch_rs::tools::core::ToolSet;
//!
//! let tools = ToolSet::new()
//!     .with(ReadFile::new("/my/project"))
//!     .with(Grep::new("/my/project").max_matches(500))
//!     .with(Shell::new("/my/project").block_command("rm -rf"));
//! ```

use std::path::Path;
use tokio::fs;
use tokio::process::Command;

use crate::ToolDef;
use crate::tools::core::{Tool, ToolFuture, TruncationStrategy, truncate_with_strategy};
use crate::tools::spec::ToolSpec;
use schemars::JsonSchema;
use serde::Deserialize;

// ── Defaults ────────────────────────────────────────────────────────

/// Default maximum grep matches before truncation.
pub const DEFAULT_MAX_GREP_MATCHES: u32 = 200;

/// Default maximum find results.
pub const DEFAULT_MAX_FIND_RESULTS: u32 = 100;

/// Default blocked shell command patterns (lowercased substrings).
pub const DEFAULT_BLOCKED_COMMANDS: &[&str] = &["rm -rf /", "mkfs", "> /dev/"];

use crate::tools::core::{DEFAULT_MAX_RESULT_BYTES, truncate_result};
use crate::tools::read_tracker::ReadTracker;
use std::sync::Arc;

// ── Typed argument structs ──────────────────────────────────────────

/// Typed arguments for `read_file`.
#[derive(Deserialize, JsonSchema)]
pub struct ReadFileArgs {
    /// File path relative to repo root (e.g. 'docs/readme.md').
    pub path: String,
    /// Starting line number (1-indexed). Default: 1.
    #[serde(default)]
    pub offset: Option<u32>,
    /// Maximum number of lines to return. Default: 2000.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Typed arguments for `list_dir`.
#[derive(Deserialize, JsonSchema)]
pub struct ListDirArgs {
    /// Directory path relative to repo root (e.g. 'docs/').
    pub path: String,
    /// Maximum directory depth to recurse into. Default: 2.
    #[serde(default)]
    pub depth: Option<u32>,
    /// Maximum number of entries to return. Default: 50.
    #[serde(default)]
    pub limit: Option<u32>,
    /// 1-indexed offset for pagination. Default: 1.
    #[serde(default)]
    pub offset: Option<u32>,
}

/// Typed arguments for `grep`.
#[derive(Deserialize, JsonSchema)]
pub struct GrepArgs {
    /// Regex pattern to search for.
    pub pattern: String,
    /// Directory or file to search in (relative to repo root, default '.').
    #[serde(default)]
    pub path: Option<String>,
    /// File glob filter (e.g. '*.md', '*.rs', '*.json').
    #[serde(default)]
    pub glob: Option<String>,
    /// Case-insensitive search (default false).
    #[serde(default)]
    pub case_insensitive: Option<bool>,
    /// Output mode: 'files' (paths only, default), 'content' (matching lines), 'count' (match counts per file).
    #[serde(default)]
    pub mode: Option<String>,
    /// Lines of context around each match (only used in 'content' mode). Default: 0.
    #[serde(default)]
    pub context_lines: Option<u32>,
}

/// Typed arguments for `find_files`.
#[derive(Deserialize, JsonSchema)]
pub struct FindFilesArgs {
    /// Glob pattern (e.g. 'src/**/*.rs', 'docs/*.md').
    pub pattern: String,
    /// Directory to search in, relative to repo root. Default: repo root.
    #[serde(default)]
    pub path: Option<String>,
    /// Maximum number of results to return. Default: 100, max: 1000.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Typed arguments for `shell`.
#[derive(Deserialize, JsonSchema)]
pub struct ShellArgs {
    /// Shell command to execute (e.g. 'wc -l *.md', 'git log --oneline -5').
    pub command: String,
    /// Timeout in seconds. Default: 120, max: 600.
    #[serde(default)]
    pub timeout: Option<u32>,
    /// Working directory relative to repo root. Default: repo root.
    #[serde(default)]
    pub working_dir: Option<String>,
}

/// Typed arguments for `web_search`.
#[derive(Deserialize, JsonSchema)]
pub struct WebSearchArgs {
    /// The search query (e.g. 'creatine monohydrate dosing research 2024').
    pub query: String,
    /// Number of results to return (default 5, max 20).
    #[serde(default)]
    pub count: Option<u32>,
}

/// Typed arguments for `edit_file`.
#[derive(Deserialize, JsonSchema)]
pub struct EditFileArgs {
    /// File path relative to repo root (e.g. 'src/main.rs').
    pub path: String,
    /// Exact text to find in the file.
    pub old_string: String,
    /// Replacement text.
    pub new_string: String,
    /// Replace all occurrences instead of requiring uniqueness. Default: false.
    #[serde(default)]
    pub replace_all: Option<bool>,
}

/// Typed arguments for `write_file`.
#[derive(Deserialize, JsonSchema)]
pub struct WriteFileArgs {
    /// File path relative to repo root (e.g. 'src/new_module.rs').
    pub path: String,
    /// Full file content to write.
    pub content: String,
}

// ── ReadFile ────────────────────────────────────────────────────────

/// Read a file from a working directory.
///
/// Path traversal (`..`) is blocked. Results are truncated to
/// `max_result_bytes`.
pub struct ReadFile {
    workdir: String,
    max_result_bytes: usize,
    tracker: Option<Arc<ReadTracker>>,
}

impl ReadFile {
    pub fn new(workdir: impl Into<String>) -> Self {
        Self {
            workdir: workdir.into(),
            max_result_bytes: DEFAULT_MAX_RESULT_BYTES,
            tracker: None,
        }
    }

    pub fn max_result_bytes(mut self, max: usize) -> Self {
        self.max_result_bytes = max;
        self
    }

    /// Attach a [`ReadTracker`] so successful reads are recorded for
    /// read-before-write enforcement.
    pub fn with_tracker(mut self, tracker: Arc<ReadTracker>) -> Self {
        self.tracker = Some(tracker);
        self
    }
}

/// Default line limit for `read_file`.
const DEFAULT_READ_LINE_LIMIT: u32 = 2000;

/// Maximum characters per line before truncation.
const MAX_LINE_CHARS: usize = 500;

impl Tool for ReadFile {
    fn definition(&self) -> ToolDef {
        ToolSpec::builder(super::names::READ_FILE)
            .purpose("Read a file with numbered lines")
            .when_to_use(
                "When you need to read a specific file whose path you already know. \
                 Returns numbered lines you can reference in edit_file operations",
            )
            .when_not_to_use(
                "When searching for a pattern across many files — use grep instead. \
                 When you need to list files in a directory — use list_dir instead",
            )
            .parameters_for::<ReadFileArgs>()
            .example(
                "read_file(path='src/main.rs')",
                "L1: use std::fs;\nL2: use std::path::Path;\nL3:\nL4: fn main() {",
            )
            .example(
                "read_file(path='src/main.rs', offset=50, limit=20)",
                "Returns lines 50-69 with L{n}: prefix",
            )
            .output_format("Numbered lines: L{n}: {content}. For large files use offset/limit to read specific sections.")
            .disambiguate(
                "Need to find which files contain a keyword",
                "grep",
                "grep searches content across files; read_file reads a single known file",
            )
            .build()
            .to_tool_def()
    }

    fn cacheable(&self) -> bool {
        true
    }

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let workdir = self.workdir.clone();
        let max = self.max_result_bytes;
        let tracker = self.tracker.clone();
        let arguments = arguments.to_string();
        Box::pin(async move {
            let args: ReadFileArgs = match serde_json::from_str(&arguments) {
                Ok(a) => a,
                Err(_) => return "Error: 'path' argument is required".to_string(),
            };
            if args.path.contains("..") {
                return "Error: path traversal not allowed".to_string();
            }
            let full_path = Path::new(&workdir).join(&args.path);

            // Catch directories early so the LLM gets an actionable hint
            // instead of the raw OS error ("Is a directory (os error 21)").
            if let Ok(meta) = fs::metadata(&full_path).await
                && meta.is_dir()
            {
                return format!(
                    "Error: '{}' is a directory, not a file. \
                     Use list_dir to browse directories.",
                    args.path
                );
            }

            match fs::read_to_string(&full_path).await {
                Ok(content) => {
                    // Register the read with full (untruncated) content.
                    if let Some(ref t) = tracker {
                        t.record_read(&full_path.to_string_lossy(), &content);
                    }

                    let total_lines = content.lines().count();
                    let offset = args.offset.unwrap_or(1).max(1) as usize;
                    let limit = args.limit.unwrap_or(DEFAULT_READ_LINE_LIMIT) as usize;

                    // Format with line numbers: L{n}: {content}
                    let mut output = String::new();
                    for (i, line) in content.lines().enumerate() {
                        let line_num = i + 1; // 1-indexed
                        if line_num < offset {
                            continue;
                        }
                        if line_num >= offset + limit {
                            break;
                        }
                        // Truncate long lines.
                        if line.len() > MAX_LINE_CHARS {
                            let end = line.floor_char_boundary(MAX_LINE_CHARS);
                            #[allow(clippy::string_slice)] // end from floor_char_boundary
                            output.push_str(&format!(
                                "L{line_num}: {}... [line truncated at {MAX_LINE_CHARS} chars]\n",
                                &line[..end]
                            ));
                        } else {
                            output.push_str(&format!("L{line_num}: {line}\n"));
                        }
                    }

                    // Append truncation notice if there are more lines.
                    if offset + limit <= total_lines {
                        output.push_str(&format!(
                            "[truncated: {total_lines} total lines. Use offset/limit for more.]"
                        ));
                    }

                    truncate_result(output, max)
                }
                Err(e) => format!("Error reading '{}': {e}", full_path.display()),
            }
        })
    }
}

// ── ListDir ─────────────────────────────────────────────────────────

/// List a directory tree under the working directory.
///
/// Uses a native Rust recursive walk (no shell dependency) with
/// configurable depth, limit, and offset for pagination. Path traversal
/// (`..`) is blocked.
pub struct ListDir {
    workdir: String,
}

impl ListDir {
    pub fn new(workdir: impl Into<String>) -> Self {
        Self {
            workdir: workdir.into(),
        }
    }
}

/// Default maximum depth for `list_dir`.
const DEFAULT_LIST_DIR_DEPTH: u32 = 2;

/// Default entry limit for `list_dir`.
const DEFAULT_LIST_DIR_LIMIT: u32 = 50;

impl Tool for ListDir {
    fn definition(&self) -> ToolDef {
        ToolSpec::builder(super::names::LIST_DIR)
            .purpose("List directory contents as an indented tree")
            .when_to_use(
                "When you need to discover what files exist in a specific directory",
            )
            .when_not_to_use(
                "When searching for files by glob pattern across nested directories — \
                 use find_files instead. When you already know the file path — use read_file",
            )
            .parameters_for::<ListDirArgs>()
            .example(
                "list_dir(path='src/')",
                "Absolute path: /project/src\ntools/\n  common.rs\n  core.rs\nREADME.md",
            )
            .output_format(
                "Indented tree rooted at the target directory. Directories end with '/', \
                 symlinks end with '@'. Use limit/offset for pagination.",
            )
            .disambiguate(
                "Need to find files matching a glob pattern recursively",
                "find_files",
                "find_files supports glob patterns across nested directories; list_dir shows a directory tree",
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
            let args: ListDirArgs = match serde_json::from_str(&arguments) {
                Ok(a) => a,
                Err(_) => return "Error: 'path' argument is required".to_string(),
            };
            if args.path.contains("..") {
                return "Error: path traversal not allowed".to_string();
            }
            let full_path = Path::new(&workdir).join(&args.path);

            let depth = args.depth.unwrap_or(DEFAULT_LIST_DIR_DEPTH) as usize;
            let limit = args.limit.unwrap_or(DEFAULT_LIST_DIR_LIMIT) as usize;
            let offset = args.offset.unwrap_or(1).max(1) as usize - 1; // convert to 0-indexed

            // Resolve to absolute path for the header.
            let abs_path = match tokio::fs::canonicalize(&full_path).await {
                Ok(p) => p.to_string_lossy().to_string(),
                Err(e) => return format!("Error: {e}"),
            };

            // Collect all entries with recursive walk.
            let mut entries: Vec<String> = Vec::new();
            if let Err(e) = collect_dir_entries(&full_path, depth, 0, &mut entries).await {
                return format!("Error: {e}");
            }

            let total = entries.len();
            let page: Vec<&str> = entries
                .iter()
                .skip(offset)
                .take(limit)
                .map(|s| s.as_str())
                .collect();

            let mut out = format!("Absolute path: {abs_path}\n");
            for entry in &page {
                out.push_str(entry);
                out.push('\n');
            }

            if offset + limit < total {
                out.push_str(&format!(
                    "More than {} entries found ({total} total). Use offset/limit for more.",
                    offset + limit,
                ));
            }

            out
        })
    }
}

/// Recursively collect directory entries into an indented list.
///
/// Directories are suffixed with `/`, symlinks with `@`. Entries at each
/// level are sorted alphabetically.
async fn collect_dir_entries(
    dir: &std::path::Path,
    max_depth: usize,
    current_depth: usize,
    out: &mut Vec<String>,
) -> Result<(), String> {
    let mut rd = tokio::fs::read_dir(dir)
        .await
        .map_err(|e| format!("cannot read directory: {e}"))?;

    // Collect entries so we can sort them.
    let mut children: Vec<(String, std::fs::FileType)> = Vec::new();
    while let Some(entry) = rd.next_entry().await.map_err(|e| e.to_string())? {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Ok(ft) = entry.file_type().await {
            children.push((name, ft));
        }
    }
    children.sort_by(|a, b| a.0.cmp(&b.0));

    let indent = "  ".repeat(current_depth);
    for (name, ft) in &children {
        let suffix = if ft.is_dir() {
            "/"
        } else if ft.is_symlink() {
            "@"
        } else {
            ""
        };
        out.push(format!("{indent}{name}{suffix}"));

        // Recurse into subdirectories if within depth.
        if ft.is_dir() && current_depth < max_depth {
            let child_path = dir.join(name);
            // Best-effort: skip directories we can't read.
            let _ = Box::pin(collect_dir_entries(
                &child_path,
                max_depth,
                current_depth + 1,
                out,
            ))
            .await;
        }
    }
    Ok(())
}

// ── Grep ────────────────────────────────────────────────────────────

/// Regex search in file contents under the working directory.
///
/// Supports optional glob filtering and case-insensitive search.
/// Path traversal (`..`) is blocked.
pub struct Grep {
    workdir: String,
    max_matches: u32,
    max_result_bytes: usize,
}

impl Grep {
    pub fn new(workdir: impl Into<String>) -> Self {
        Self {
            workdir: workdir.into(),
            max_matches: DEFAULT_MAX_GREP_MATCHES,
            max_result_bytes: DEFAULT_MAX_RESULT_BYTES,
        }
    }

    pub fn max_matches(mut self, max: u32) -> Self {
        self.max_matches = max;
        self
    }

    pub fn max_result_bytes(mut self, max: usize) -> Self {
        self.max_result_bytes = max;
        self
    }
}

impl Tool for Grep {
    fn definition(&self) -> ToolDef {
        ToolSpec::builder(super::names::GREP)
            .purpose("Search for a regex pattern in file contents")
            .when_to_use(
                "When you need to find text matching a pattern across multiple files. \
                 Defaults to returning file paths only (compact). Use mode='content' \
                 to see matching lines when needed",
            )
            .when_not_to_use(
                "When you already know the file path — use read_file instead. \
                 When you need to find files by name — use find_files instead",
            )
            .parameters_for::<GrepArgs>()
            .example(
                "grep(pattern='TODO', glob='*.rs')",
                "src/main.rs\nsrc/tools/common.rs",
            )
            .example(
                "grep(pattern='fn execute', mode='content', glob='*.rs')",
                "src/tools/common.rs:42: fn execute(&self, arguments: &str) -> ToolFuture",
            )
            .output_format(
                "Depends on mode: 'files' returns paths only (default), \
                 'content' returns file:line:match, 'count' returns file:count",
            )
            .disambiguate(
                "Need to read a file you already know the path of",
                "read_file",
                "read_file returns full file content; grep returns matching lines across files",
            )
            .disambiguate(
                "Need to find files by name pattern",
                "find_files",
                "find_files matches file paths; grep matches file content",
            )
            .build()
            .to_tool_def()
    }

    fn cacheable(&self) -> bool {
        true
    }

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let workdir = self.workdir.clone();
        let max_matches = self.max_matches;
        let max_result_bytes = self.max_result_bytes;
        let arguments = arguments.to_string();
        Box::pin(async move {
            let args: GrepArgs = match serde_json::from_str(&arguments) {
                Ok(a) => a,
                Err(_) => return "Error: 'pattern' argument is required".to_string(),
            };
            let search_path = args.path.as_deref().unwrap_or(".");
            if search_path.contains("..") {
                return "Error: path traversal not allowed".to_string();
            }
            let full_path = Path::new(&workdir).join(search_path);

            let mode = args.mode.as_deref().unwrap_or("files");

            let mut cmd_args: Vec<String> = vec![
                "-r".to_string(),
                "--color=never".to_string(),
                format!("--max-count={max_matches}"),
            ];

            match mode {
                "files" => {
                    // Files-only mode: return paths only (most token-efficient).
                    cmd_args.push("-l".to_string());
                }
                "content" => {
                    // Content mode: return matching lines with line numbers.
                    cmd_args.push("-n".to_string());
                    if let Some(ctx) = args.context_lines
                        && ctx > 0
                    {
                        cmd_args.push(format!("-C{ctx}"));
                    }
                }
                "count" => {
                    // Count mode: return match counts per file.
                    cmd_args.push("-c".to_string());
                }
                _ => {
                    return format!(
                        "Error: invalid mode '{}'. Use 'files', 'content', or 'count'.",
                        mode
                    );
                }
            }

            if args.case_insensitive.unwrap_or(false) {
                cmd_args.push("-i".to_string());
            }

            if let Some(glob) = &args.glob {
                cmd_args.push(format!("--include={glob}"));
            }

            cmd_args.push(args.pattern);
            cmd_args.push(full_path.to_string_lossy().to_string());

            let arg_refs: Vec<&str> = cmd_args.iter().map(|s| s.as_str()).collect();
            // grep returns exit code 1 for "no matches" — not an error.
            let result = run_command("grep", &arg_refs, &[1]).await;

            // For count mode, strip lines with :0 (no matches in that file).
            let result = if mode == "count" {
                result
                    .lines()
                    .filter(|line| !line.ends_with(":0"))
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                result
            };

            truncate_result(result, max_result_bytes)
        })
    }
}

// ── FindFiles ───────────────────────────────────────────────────────

/// Find files matching a glob pattern under the working directory.
///
/// Path traversal (`..`) is blocked.
pub struct FindFiles {
    workdir: String,
    max_results: u32,
    max_result_bytes: usize,
}

impl FindFiles {
    pub fn new(workdir: impl Into<String>) -> Self {
        Self {
            workdir: workdir.into(),
            max_results: DEFAULT_MAX_FIND_RESULTS,
            max_result_bytes: DEFAULT_MAX_RESULT_BYTES,
        }
    }

    pub fn max_results(mut self, max: u32) -> Self {
        self.max_results = max;
        self
    }

    pub fn max_result_bytes(mut self, max: usize) -> Self {
        self.max_result_bytes = max;
        self
    }
}

impl Tool for FindFiles {
    fn definition(&self) -> ToolDef {
        ToolSpec::builder(super::names::FIND_FILES)
            .purpose(
                "Find files matching a glob pattern, sorted by modification time (newest first)",
            )
            .when_to_use(
                "When you need to discover files by name or extension across nested directories. \
                 Results are sorted by modification time (newest first) so the most recently \
                 changed files appear first",
            )
            .when_not_to_use(
                "When listing files in a single known directory — use list_dir instead. \
                 When searching file content — use grep instead",
            )
            .parameters_for::<FindFilesArgs>()
            .example(
                "find_files(pattern='docs/**/*.md')",
                "Returns matching file paths sorted by modification time (newest first)",
            )
            .example(
                "find_files(pattern='*.rs', path='src/tools', limit=10)",
                "Returns the 10 most recently modified .rs files under src/tools",
            )
            .output_format(
                "Newline-separated list of file paths relative to repo root, \
                 sorted by modification time (newest first)",
            )
            .disambiguate(
                "Need to see files in one directory",
                "list_dir",
                "list_dir shows a directory tree; find_files searches recursively by pattern",
            )
            .build()
            .to_tool_def()
    }

    fn cacheable(&self) -> bool {
        true
    }

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let workdir = self.workdir.clone();
        let default_max_results = self.max_results;
        let max_result_bytes = self.max_result_bytes;
        let arguments = arguments.to_string();
        Box::pin(async move {
            let args: FindFilesArgs = match serde_json::from_str(&arguments) {
                Ok(a) => a,
                Err(_) => return "Error: 'pattern' argument is required".to_string(),
            };
            if args.pattern.contains("..") {
                return "Error: path traversal not allowed".to_string();
            }
            if let Some(ref p) = args.path
                && p.contains("..")
            {
                return "Error: path traversal not allowed".to_string();
            }

            let limit = args.limit.unwrap_or(default_max_results).min(1000);
            let search_path = args.path.as_deref().unwrap_or(".");
            let pattern = &args.pattern;

            // Build the -path argument. When searching from a subdirectory,
            // the path prefix in find changes from "./" to "{search_path}/".
            let find_pattern = if search_path == "." {
                format!("./{pattern}")
            } else {
                format!("{search_path}/{pattern}")
            };

            // Use find + xargs ls -1t for mtime-sorted results.
            // Cap intermediate results at 1000 for the sort step.
            let result = run_shell(
                &workdir,
                &format!(
                    "find {search_path} -path '{find_pattern}' -type f 2>/dev/null \
                     | head -1000 \
                     | xargs ls -1t 2>/dev/null \
                     | head -{limit}"
                ),
            )
            .await;

            // Strip the [exit: N] prefix from the inner shell call for clean output.
            let clean = result
                .strip_prefix("[exit: 0]\n")
                .or_else(|| result.strip_prefix("[exit: 1]\n"))
                .unwrap_or(&result);

            if clean.trim().is_empty() {
                format!("No files found matching '{pattern}'")
            } else {
                truncate_result(clean.to_string(), max_result_bytes)
            }
        })
    }
}

// ── Shell ───────────────────────────────────────────────────────────

/// Execute shell commands in the working directory.
///
/// Commands matching any pattern in `blocked_commands` are rejected.
pub struct Shell {
    workdir: String,
    blocked_commands: Vec<String>,
    max_result_bytes: usize,
}

impl Shell {
    pub fn new(workdir: impl Into<String>) -> Self {
        Self {
            workdir: workdir.into(),
            blocked_commands: DEFAULT_BLOCKED_COMMANDS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            max_result_bytes: DEFAULT_MAX_RESULT_BYTES,
        }
    }

    /// Add a blocked command pattern (lowercased substring match).
    pub fn block_command(mut self, pattern: impl Into<String>) -> Self {
        self.blocked_commands.push(pattern.into());
        self
    }

    /// Replace the entire blocked commands list.
    pub fn blocked_commands(mut self, patterns: Vec<String>) -> Self {
        self.blocked_commands = patterns;
        self
    }

    pub fn max_result_bytes(mut self, max: usize) -> Self {
        self.max_result_bytes = max;
        self
    }
}

impl Tool for Shell {
    fn definition(&self) -> ToolDef {
        ToolSpec::builder(super::names::SHELL)
            .purpose("Run a shell command and return its output")
            .when_to_use(
                "When you need an operation not covered by other tools: git commands, \
                 word counts, date calculations, file manipulation, data processing, etc. \
                 Commands run in the repo root directory by default",
            )
            .when_not_to_use(
                "When a dedicated tool exists for the task — use read_file to read files, \
                 grep to search content, find_files to find files by name. \
                 Prefer dedicated tools for better error handling",
            )
            .parameters_for::<ShellArgs>()
            .example(
                "shell(command='git log --oneline -5')",
                "[exit: 0]\na1b2c3d First commit\n...",
            )
            .output_format(
                "Prefixed with [exit: N]. Stdout follows; stderr appended on failure. \
                 Long output is truncated with head+tail preserved.",
            )
            .build()
            .to_tool_def()
    }

    fn is_mutation(&self) -> bool {
        true
    }

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let workdir = self.workdir.clone();
        let blocked = self.blocked_commands.clone();
        let max = self.max_result_bytes;
        let arguments = arguments.to_string();
        Box::pin(async move {
            let args: ShellArgs = match serde_json::from_str(&arguments) {
                Ok(a) => a,
                Err(_) => return "Error: 'command' argument is required".to_string(),
            };
            let lower = args.command.to_lowercase();
            if blocked.iter().any(|pat| lower.contains(pat)) {
                return "Error: potentially destructive command blocked".to_string();
            }

            // Resolve working directory.
            let effective_workdir = if let Some(ref wd) = args.working_dir {
                if wd.contains("..") {
                    return "Error: path traversal not allowed in working_dir".to_string();
                }
                let p = std::path::Path::new(&workdir).join(wd);
                p.to_string_lossy().to_string()
            } else {
                workdir.clone()
            };

            // Timeout: default 120s, cap at 600s.
            let timeout_secs = args.timeout.unwrap_or(120).min(600);
            let timeout_dur = std::time::Duration::from_secs(timeout_secs as u64);

            let result = match tokio::time::timeout(
                timeout_dur,
                run_shell(&effective_workdir, &args.command),
            )
            .await
            {
                Ok(output) => output,
                Err(_) => {
                    return format!("Error: command timed out after {timeout_secs} seconds");
                }
            };

            truncate_with_strategy(
                result,
                max,
                &TruncationStrategy::HeadAndTail { tail_ratio: 0.4 },
            )
        })
    }
}

// ── WebSearch ──────────────────────────────────────────────────────

/// Search the web via the Brave Search API and return formatted results.
///
/// Requires the `BRAVE_SEARCH_KEY` environment variable (free tier: 2000
/// queries/month at <https://brave.com/search/api/>).
pub struct WebSearch {
    max_result_bytes: usize,
}

impl Default for WebSearch {
    fn default() -> Self {
        Self::new()
    }
}

impl WebSearch {
    pub fn new() -> Self {
        Self {
            max_result_bytes: DEFAULT_MAX_RESULT_BYTES,
        }
    }

    pub fn max_result_bytes(mut self, max: usize) -> Self {
        self.max_result_bytes = max;
        self
    }
}

impl Tool for WebSearch {
    fn definition(&self) -> ToolDef {
        ToolSpec::builder(super::names::WEB_SEARCH)
            .purpose("Search the web and return results with titles, URLs, and snippets")
            .when_to_use(
                "When you need current information, recent research, up-to-date data, \
                 or facts you are unsure about. Use specific, targeted queries",
            )
            .when_not_to_use(
                "When the answer is already in your training data or in local files. \
                 Use read_file or grep for local content instead",
            )
            .parameters_for::<WebSearchArgs>()
            .example(
                "web_search(query='creatine monohydrate dosing research 2024')",
                "Returns web search results with titles, URLs, and snippets",
            )
            .output_format("Numbered list of results: title, URL, and snippet")
            .build()
            .to_tool_def()
    }

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let max = self.max_result_bytes;
        let arguments = arguments.to_string();
        Box::pin(async move {
            let args: WebSearchArgs = match serde_json::from_str(&arguments) {
                Ok(a) => a,
                Err(_) => return "Error: 'query' argument is required".to_string(),
            };
            let count = args.count.unwrap_or(5).min(20);
            match brave_search(&args.query, count).await {
                Ok(results) => {
                    if results.is_empty() {
                        format!("No results found for '{}'", args.query)
                    } else {
                        truncate_result(results, max)
                    }
                }
                Err(e) => format!("Error: web search failed: {e}"),
            }
        })
    }
}

/// Call the Brave Search API and return formatted results.
async fn brave_search(query: &str, count: u32) -> Result<String, String> {
    let api_key = std::env::var("BRAVE_SEARCH_KEY").map_err(|_| {
        "BRAVE_SEARCH_KEY env var not set. \
         Get a free API key at https://brave.com/search/api/"
            .to_string()
    })?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;

    let url = format!(
        "https://api.search.brave.com/res/v1/web/search?q={}&count={count}",
        urlencoded(query),
    );
    let resp = client
        .get(&url)
        .header("X-Subscription-Token", &api_key)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("HTTP {status}: {body}"));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e: reqwest::Error| e.to_string())?;
    Ok(format_brave_results(&body))
}

/// Minimal percent-encoding for URL query parameters.
fn urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}

/// Format Brave Search API JSON response into readable text.
fn format_brave_results(body: &serde_json::Value) -> String {
    let mut out = Vec::new();

    if let Some(results) = body["web"]["results"].as_array() {
        for (i, r) in results.iter().enumerate() {
            let title = r["title"].as_str().unwrap_or("");
            let url = r["url"].as_str().unwrap_or("");
            let snippet = r["description"].as_str().unwrap_or("");

            let mut entry = format!("{}. {title}\n   {url}", i + 1);
            if !snippet.is_empty() {
                entry.push_str(&format!("\n   {snippet}"));
            }
            out.push(entry);
        }
    }

    out.join("\n\n")
}

// ── EditFile ──────────────────────────────────────────────────────

/// Edit a file by replacing an exact string.
///
/// Requires the file to have been read with `read_file` first
/// (enforced via [`ReadTracker`]).
pub struct EditFile {
    workdir: String,
    tracker: Arc<ReadTracker>,
}

impl EditFile {
    pub fn new(workdir: impl Into<String>, tracker: Arc<ReadTracker>) -> Self {
        Self {
            workdir: workdir.into(),
            tracker,
        }
    }
}

impl Tool for EditFile {
    fn definition(&self) -> ToolDef {
        ToolSpec::builder(super::names::EDIT_FILE)
            .purpose("Edit a file by replacing an exact string")
            .when_to_use(
                "When you need to make a targeted change to a file you have already read with read_file. \
                 Prefer this over write_file for modifying existing content",
            )
            .when_not_to_use(
                "When creating a new file — use write_file instead. \
                 When you haven't read the file yet — call read_file first",
            )
            .parameters_for::<EditFileArgs>()
            .example(
                "edit_file(path='src/main.rs', old_string='fn foo()', new_string='fn bar()')",
                "Edited src/main.rs: replaced 1 occurrence (line 42)",
            )
            .output_format("Confirmation with file path, count, and affected line numbers")
            .disambiguate(
                "Creating a brand-new file",
                "write_file",
                "write_file creates or overwrites; edit_file modifies existing content in-place",
            )
            .build()
            .to_tool_def()
    }

    fn is_mutation(&self) -> bool {
        true
    }

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let workdir = self.workdir.clone();
        let tracker = self.tracker.clone();
        let arguments = arguments.to_string();
        Box::pin(async move {
            let args: EditFileArgs = match serde_json::from_str(&arguments) {
                Ok(a) => a,
                Err(_) => {
                    return "Error: 'path', 'old_string', and 'new_string' are required"
                        .to_string();
                }
            };
            if args.path.contains("..") {
                return "Error: path traversal not allowed".to_string();
            }

            let full_path = Path::new(&workdir).join(&args.path);
            let abs_path = full_path.to_string_lossy().to_string();

            // Read-before-write enforcement.
            if full_path.exists() && !tracker.has_been_read(&abs_path) {
                return "Error: You must read this file before editing it. \
                        Use read_file first."
                    .to_string();
            }

            // Read current content.
            let content = match fs::read_to_string(&full_path).await {
                Ok(c) => c,
                Err(e) => return format!("Error reading '{}': {e}", args.path),
            };

            let replace_all = args.replace_all.unwrap_or(false);

            // Count occurrences.
            let count = content.matches(&args.old_string).count();

            if count == 0 {
                return format!(
                    "Error: old_string not found in {}. \
                     Verify the exact text (including whitespace and indentation).",
                    args.path
                );
            }

            if count > 1 && !replace_all {
                // Report line numbers of each match.
                #[allow(clippy::string_slice)] // byte_offset from match_indices
                let line_nums: Vec<usize> = content
                    .match_indices(&args.old_string)
                    .map(|(byte_offset, _)| content[..byte_offset].lines().count().max(1))
                    .collect();
                return format!(
                    "Error: old_string found {count} times in {} (lines: {}). \
                     Provide more surrounding context to make it unique, or set replace_all=true.",
                    args.path,
                    line_nums
                        .iter()
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }

            // Perform replacement.
            let new_content = if replace_all {
                content.replace(&args.old_string, &args.new_string)
            } else {
                content.replacen(&args.old_string, &args.new_string, 1)
            };

            // Write back.
            if let Err(e) = fs::write(&full_path, &new_content).await {
                return format!("Error writing '{}': {e}", args.path);
            }

            // Update tracker so subsequent edits don't require re-reading.
            tracker.record_write(&abs_path, &new_content);

            // Calculate affected line range for the first occurrence.
            let start_byte = content.find(&args.old_string).unwrap();
            #[allow(clippy::string_slice)] // start_byte from find()
            let start_line = content[..start_byte].lines().count().max(1);
            let end_line = start_line + args.old_string.lines().count().saturating_sub(1);

            if replace_all && count > 1 {
                format!("Edited {}: replaced {count} occurrences", args.path)
            } else if start_line == end_line {
                format!(
                    "Edited {}: replaced 1 occurrence (line {start_line})",
                    args.path
                )
            } else {
                format!(
                    "Edited {}: replaced 1 occurrence (lines {start_line}-{end_line})",
                    args.path
                )
            }
        })
    }
}

// ── WriteFile ─────────────────────────────────────────────────────

/// Create a new file or overwrite an existing file.
///
/// Existing files require a prior `read_file` call (enforced via
/// [`ReadTracker`]). New files can be written without reading first.
pub struct WriteFile {
    workdir: String,
    tracker: Arc<ReadTracker>,
}

impl WriteFile {
    pub fn new(workdir: impl Into<String>, tracker: Arc<ReadTracker>) -> Self {
        Self {
            workdir: workdir.into(),
            tracker,
        }
    }
}

impl Tool for WriteFile {
    fn definition(&self) -> ToolDef {
        ToolSpec::builder(super::names::WRITE_FILE)
            .purpose("Create a new file or overwrite an existing file")
            .when_to_use(
                "When creating a brand-new file, or when you need to replace an entire \
                 file's content. You must have read the file first if it already exists",
            )
            .when_not_to_use(
                "When making a small targeted change to an existing file — \
                 use edit_file instead (avoids rewriting unchanged content)",
            )
            .parameters_for::<WriteFileArgs>()
            .example(
                "write_file(path='src/new_module.rs', content='pub mod foo;\\n')",
                "Wrote 1 line to src/new_module.rs",
            )
            .output_format("Confirmation with line count and file path")
            .disambiguate(
                "Changing a few lines in an existing file",
                "edit_file",
                "edit_file is more precise for targeted changes; write_file replaces the whole file",
            )
            .build()
            .to_tool_def()
    }

    fn is_mutation(&self) -> bool {
        true
    }

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let workdir = self.workdir.clone();
        let tracker = self.tracker.clone();
        let arguments = arguments.to_string();
        Box::pin(async move {
            let args: WriteFileArgs = match serde_json::from_str(&arguments) {
                Ok(a) => a,
                Err(_) => return "Error: 'path' and 'content' arguments are required".to_string(),
            };
            if args.path.contains("..") {
                return "Error: path traversal not allowed".to_string();
            }

            let full_path = Path::new(&workdir).join(&args.path);
            let abs_path = full_path.to_string_lossy().to_string();

            // Read-before-overwrite: only enforce for existing files.
            let file_exists = fs::metadata(&full_path).await.is_ok();
            if file_exists && !tracker.has_been_read(&abs_path) {
                return "Error: You must read this file before overwriting it. \
                        Use read_file first."
                    .to_string();
            }

            // Create parent directories if needed.
            if let Some(parent) = full_path.parent()
                && !parent.exists()
                && let Err(e) = fs::create_dir_all(parent).await
            {
                return format!("Error creating directories for '{}': {e}", args.path);
            }

            // Write the file.
            if let Err(e) = fs::write(&full_path, &args.content).await {
                return format!("Error writing '{}': {e}", args.path);
            }

            // Update tracker with the written content.
            tracker.record_write(&abs_path, &args.content);

            let line_count = args.content.lines().count();
            format!(
                "Wrote {line_count} line{} to {}",
                if line_count == 1 { "" } else { "s" },
                args.path
            )
        })
    }
}

// ── Shared helpers ──────────────────────────────────────────────────

/// Parse a JSON arguments string, returning an empty object on failure.
pub fn parse_args(arguments: &str) -> serde_json::Value {
    serde_json::from_str(arguments).unwrap_or(serde_json::Value::Object(serde_json::Map::new()))
}

/// Format command output into a result string.
///
/// Output is prefixed with `[exit: N]` for consistent machine-readable
/// parsing. On success, only stdout is included. On failure (or when
/// stderr is non-empty), both streams are included.
fn format_output(output: std::process::Output, lenient_exit_codes: &[i32]) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);
    let ok = output.status.success() || lenient_exit_codes.contains(&code);
    if ok {
        if stderr.is_empty() {
            format!("[exit: {code}]\n{stdout}")
        } else {
            format!("[exit: {code}]\n{stdout}\n[stderr]\n{stderr}")
        }
    } else {
        format!("[exit: {code}]\n{stdout}\n{stderr}")
    }
}

/// Run a command with arguments and return its output.
///
/// When `lenient_exit_codes` contains extra exit codes (e.g. `&[1]` for grep's
/// "no matches"), those codes are treated as success.
pub async fn run_command(cmd: &str, args: &[&str], lenient_exit_codes: &[i32]) -> String {
    match Command::new(cmd).args(args).output().await {
        Ok(output) => format_output(output, lenient_exit_codes),
        Err(e) => format!("Error running {cmd}: {e}"),
    }
}

/// Run a shell command (`sh -c`) in the given working directory.
pub async fn run_shell(workdir: &str, command: &str) -> String {
    match Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(workdir)
        .output()
        .await
    {
        Ok(output) => format_output(output, &[]),
        Err(e) => format!("Error running command: {e}"),
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::core::ToolSet;

    #[test]
    fn read_file_definition_has_tool_spec_fields() {
        let tool = ReadFile::new("/tmp");
        let def = tool.definition();
        assert_eq!(def.function.name, "read_file");
        assert!(def.function.description.contains("When to use:"));
        assert!(def.function.description.contains("When NOT to use:"));
    }

    #[test]
    fn list_dir_definition_has_tool_spec_fields() {
        let tool = ListDir::new("/tmp");
        let def = tool.definition();
        assert_eq!(def.function.name, "list_dir");
        assert!(def.function.description.contains("When NOT to use:"));
    }

    #[test]
    fn grep_definition_has_disambiguations() {
        let tool = Grep::new("/tmp");
        let def = tool.definition();
        assert_eq!(def.function.name, "grep");
        assert!(def.function.description.contains("Disambiguation:"));
    }

    #[test]
    fn find_files_definition_has_tool_spec_fields() {
        let tool = FindFiles::new("/tmp");
        let def = tool.definition();
        assert_eq!(def.function.name, "find_files");
        assert!(def.function.description.contains("When NOT to use:"));
    }

    #[test]
    fn shell_definition_has_tool_spec_fields() {
        let tool = Shell::new("/tmp");
        let def = tool.definition();
        assert_eq!(def.function.name, "shell");
        assert!(def.function.description.contains("When NOT to use:"));
    }

    #[test]
    fn shell_builder_adds_blocked_command() {
        let tool = Shell::new("/tmp").block_command("dangerous_cmd");
        assert!(tool.blocked_commands.contains(&"dangerous_cmd".to_string()));
        // Defaults are preserved.
        assert!(tool.blocked_commands.iter().any(|c| c.contains("rm -rf")));
    }

    #[test]
    fn grep_builder_sets_max_matches() {
        let tool = Grep::new("/tmp").max_matches(500);
        assert_eq!(tool.max_matches, 500);
    }

    #[test]
    fn all_common_tools_register_in_toolset() {
        let set = ToolSet::new()
            .with(ReadFile::new("/tmp"))
            .with(ListDir::new("/tmp"))
            .with(Grep::new("/tmp"))
            .with(FindFiles::new("/tmp"))
            .with(Shell::new("/tmp"));
        assert_eq!(set.len(), 5);
    }

    // ── Directory detection tests ──────────────────────────────

    #[tokio::test]
    async fn read_file_returns_hint_for_directory() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("subdir");
        std::fs::create_dir(&sub).unwrap();

        let tool = ReadFile::new(dir.path().to_str().unwrap());
        let result = tool.execute(r#"{"path": "subdir"}"#).await;
        assert!(
            result.contains("is a directory, not a file"),
            "expected directory hint, got: {result}"
        );
        assert!(
            result.contains("list_dir"),
            "expected list_dir suggestion, got: {result}"
        );
    }

    // ── Path traversal tests ────────────────────────────────────

    #[tokio::test]
    async fn read_file_blocks_path_traversal() {
        let tool = ReadFile::new("/tmp");
        let result = tool.execute(r#"{"path": "../../../etc/passwd"}"#).await;
        assert_eq!(result, "Error: path traversal not allowed");
    }

    #[tokio::test]
    async fn list_dir_blocks_path_traversal() {
        let tool = ListDir::new("/tmp");
        let result = tool.execute(r#"{"path": "../../secret"}"#).await;
        assert_eq!(result, "Error: path traversal not allowed");
    }

    // ── ListDir upgrade tests ────────────────────────────────────

    #[tokio::test]
    async fn list_dir_depth_default() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("a");
        std::fs::create_dir_all(sub.join("b")).unwrap();
        std::fs::write(sub.join("b").join("deep.txt"), "").unwrap();
        std::fs::write(sub.join("file.txt"), "").unwrap();

        let tool = ListDir::new(dir.path().to_str().unwrap());
        let result = tool.execute(r#"{"path": "."}"#).await;
        // Default depth 2: should see a/, a/file.txt, a/b/, a/b/deep.txt
        assert!(result.contains("a/"), "expected 'a/', got:\n{result}");
        assert!(
            result.contains("file.txt"),
            "expected file.txt, got:\n{result}"
        );
        assert!(
            result.contains("deep.txt"),
            "expected deep.txt at depth 2, got:\n{result}"
        );
    }

    #[tokio::test]
    async fn list_dir_depth_one() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("a");
        std::fs::create_dir_all(sub.join("b")).unwrap();
        std::fs::write(sub.join("b").join("deep.txt"), "").unwrap();
        std::fs::write(dir.path().join("top.txt"), "").unwrap();

        let tool = ListDir::new(dir.path().to_str().unwrap());
        let result = tool.execute(r#"{"path": ".", "depth": 0}"#).await;
        // Depth 0: should only see top-level entries, not recurse.
        assert!(result.contains("a/"), "expected 'a/' at depth 0");
        assert!(result.contains("top.txt"), "expected top.txt");
        assert!(
            !result.contains("deep.txt"),
            "should not see deep.txt at depth 0"
        );
    }

    #[tokio::test]
    async fn list_dir_limit_and_offset() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..10 {
            std::fs::write(dir.path().join(format!("file_{i:02}.txt")), "").unwrap();
        }

        let tool = ListDir::new(dir.path().to_str().unwrap());
        // Get entries 3-5 (offset=3, limit=3)
        let result = tool
            .execute(r#"{"path": ".", "depth": 0, "limit": 3, "offset": 3}"#)
            .await;
        assert!(
            result.contains("file_02"),
            "expected file_02 at offset 3, got:\n{result}"
        );
        assert!(
            result.contains("file_04"),
            "expected file_04, got:\n{result}"
        );
        assert!(
            !result.contains("file_00"),
            "file_00 should be before offset"
        );
        assert!(
            result.contains("More than"),
            "should indicate more entries available"
        );
    }

    #[tokio::test]
    async fn list_dir_dirs_marked_with_slash() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        std::fs::write(dir.path().join("file.txt"), "").unwrap();

        let tool = ListDir::new(dir.path().to_str().unwrap());
        let result = tool.execute(r#"{"path": ".", "depth": 0}"#).await;
        assert!(
            result.contains("subdir/"),
            "directories should have trailing /"
        );
        assert!(
            result.contains("file.txt") && !result.contains("file.txt/"),
            "files should not have trailing /"
        );
    }

    #[tokio::test]
    async fn list_dir_absolute_path_header() {
        let dir = tempfile::tempdir().unwrap();
        let tool = ListDir::new(dir.path().to_str().unwrap());
        let result = tool.execute(r#"{"path": "."}"#).await;
        assert!(
            result.starts_with("Absolute path:"),
            "expected Absolute path: header, got: {result}"
        );
    }

    #[tokio::test]
    async fn grep_blocks_path_traversal() {
        let tool = Grep::new("/tmp");
        let result = tool
            .execute(r#"{"pattern": "password", "path": "../../../etc"}"#)
            .await;
        assert_eq!(result, "Error: path traversal not allowed");
    }

    #[tokio::test]
    async fn find_files_blocks_path_traversal() {
        let tool = FindFiles::new("/tmp");
        let result = tool.execute(r#"{"pattern": "../../*.txt"}"#).await;
        assert_eq!(result, "Error: path traversal not allowed");
    }

    // ── FindFiles upgrade tests ──────────────────────────────────

    #[tokio::test]
    async fn find_files_mtime_sorted() {
        let dir = tempfile::tempdir().unwrap();
        // Create files with a slight delay so mtimes differ.
        std::fs::write(dir.path().join("old.txt"), "old").unwrap();
        // Touch to ensure different mtime.
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(dir.path().join("new.txt"), "new").unwrap();

        let tool = FindFiles::new(dir.path().to_str().unwrap());
        let result = tool.execute(r#"{"pattern": "*.txt"}"#).await;
        let new_pos = result.find("new.txt");
        let old_pos = result.find("old.txt");
        assert!(
            new_pos.is_some() && old_pos.is_some(),
            "both files should appear, got:\n{result}"
        );
        assert!(
            new_pos.unwrap() < old_pos.unwrap(),
            "new.txt should appear before old.txt (mtime order), got:\n{result}"
        );
    }

    #[tokio::test]
    async fn find_files_respects_limit_param() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), "").unwrap();
        }

        let tool = FindFiles::new(dir.path().to_str().unwrap());
        let result = tool.execute(r#"{"pattern": "*.txt", "limit": 2}"#).await;
        let lines: Vec<&str> = result.lines().filter(|l| l.ends_with(".txt")).collect();
        assert_eq!(lines.len(), 2, "expected exactly 2 results, got:\n{result}");
    }

    #[tokio::test]
    async fn find_files_respects_path_param() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("inside.txt"), "").unwrap();
        std::fs::write(dir.path().join("outside.txt"), "").unwrap();

        let tool = FindFiles::new(dir.path().to_str().unwrap());
        let result = tool.execute(r#"{"pattern": "*.txt", "path": "sub"}"#).await;
        assert!(
            result.contains("inside.txt"),
            "should find inside.txt, got:\n{result}"
        );
        assert!(
            !result.contains("outside.txt"),
            "should not find outside.txt, got:\n{result}"
        );
    }

    // ── Shell blocking tests ────────────────────────────────────

    #[tokio::test]
    async fn shell_blocks_rm_rf_root() {
        let tool = Shell::new("/tmp");
        let result = tool.execute(r#"{"command": "rm -rf /"}"#).await;
        assert_eq!(result, "Error: potentially destructive command blocked");
    }

    #[tokio::test]
    async fn shell_blocks_mkfs() {
        let tool = Shell::new("/tmp");
        let result = tool.execute(r#"{"command": "mkfs.ext4 /dev/sda"}"#).await;
        assert_eq!(result, "Error: potentially destructive command blocked");
    }

    #[tokio::test]
    async fn shell_blocks_custom_pattern() {
        let tool = Shell::new("/tmp").block_command("drop table");
        let result = tool
            .execute(r#"{"command": "echo DROP TABLE users"}"#)
            .await;
        assert_eq!(result, "Error: potentially destructive command blocked");
    }

    // ── Shell upgrade tests ───────────────────────────────────

    #[tokio::test]
    async fn shell_output_has_exit_code_prefix() {
        let tool = Shell::new("/tmp");
        let result = tool.execute(r#"{"command": "echo hello"}"#).await;
        assert!(
            result.starts_with("[exit: 0]"),
            "expected [exit: 0] prefix, got: {result}"
        );
        assert!(result.contains("hello"));
    }

    #[tokio::test]
    async fn shell_failed_command_has_exit_code() {
        let tool = Shell::new("/tmp");
        let result = tool.execute(r#"{"command": "false"}"#).await;
        assert!(
            result.starts_with("[exit: 1]"),
            "expected [exit: 1] prefix, got: {result}"
        );
    }

    #[tokio::test]
    async fn shell_timeout_enforced() {
        let tool = Shell::new("/tmp");
        let result = tool
            .execute(r#"{"command": "sleep 10", "timeout": 1}"#)
            .await;
        assert!(
            result.contains("timed out after 1 seconds"),
            "expected timeout error, got: {result}"
        );
    }

    #[tokio::test]
    async fn shell_working_dir_override() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("mydir");
        std::fs::create_dir(&sub).unwrap();

        let tool = Shell::new(dir.path().to_str().unwrap());
        let result = tool
            .execute(r#"{"command": "pwd", "working_dir": "mydir"}"#)
            .await;
        assert!(
            result.contains("mydir"),
            "expected 'mydir' in output, got: {result}"
        );
    }

    // ── Missing argument tests ──────────────────────────────────

    #[tokio::test]
    async fn read_file_requires_path() {
        let tool = ReadFile::new("/tmp");
        let result = tool.execute("{}").await;
        assert_eq!(result, "Error: 'path' argument is required");
    }

    #[tokio::test]
    async fn grep_requires_pattern() {
        let tool = Grep::new("/tmp");
        let result = tool.execute("{}").await;
        assert_eq!(result, "Error: 'pattern' argument is required");
    }

    #[tokio::test]
    async fn shell_requires_command() {
        let tool = Shell::new("/tmp");
        let result = tool.execute("{}").await;
        assert_eq!(result, "Error: 'command' argument is required");
    }

    // ── Helper tests ────────────────────────────────────────────

    #[test]
    fn parse_args_returns_empty_object_on_invalid_json() {
        let result = parse_args("not json");
        assert!(result.is_object());
        assert!(result.as_object().unwrap().is_empty());
    }

    // ── Schema generation tests ─────────────────────────────────

    #[test]
    fn read_file_args_schema_has_required_path() {
        let schema = crate::json_schema_for::<ReadFileArgs>();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("path")));
    }

    #[test]
    fn grep_args_schema_has_required_pattern() {
        let schema = crate::json_schema_for::<GrepArgs>();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("pattern")));
        // Optional fields should not be in required.
        assert!(!required.contains(&serde_json::json!("glob")));
    }

    #[test]
    fn shell_args_schema_has_required_command() {
        let schema = crate::json_schema_for::<ShellArgs>();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("command")));
    }

    // ── EditFile tests ─────────────────────────────────────────────

    fn make_tracker() -> Arc<ReadTracker> {
        Arc::new(ReadTracker::new())
    }

    #[tokio::test]
    async fn edit_file_requires_prior_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn main() {}").unwrap();

        let tracker = make_tracker();
        let tool = EditFile::new(dir.path().to_str().unwrap(), tracker);
        let result = tool
            .execute(r#"{"path": "test.rs", "old_string": "main", "new_string": "start"}"#)
            .await;
        assert!(
            result.contains("must read this file before editing"),
            "expected enforcement error, got: {result}"
        );
    }

    #[tokio::test]
    async fn edit_file_succeeds_after_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn main() {}").unwrap();

        let tracker = make_tracker();
        let read_tool = ReadFile::new(dir.path().to_str().unwrap()).with_tracker(tracker.clone());
        let edit_tool = EditFile::new(dir.path().to_str().unwrap(), tracker);

        // Read first.
        read_tool.execute(r#"{"path": "test.rs"}"#).await;

        // Then edit.
        let result = edit_tool
            .execute(r#"{"path": "test.rs", "old_string": "main", "new_string": "start"}"#)
            .await;
        assert!(
            result.contains("Edited test.rs"),
            "expected success, got: {result}"
        );
        assert!(result.contains("replaced 1 occurrence"));

        // Verify file was actually changed.
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "fn start() {}");
    }

    #[tokio::test]
    async fn edit_file_rejects_ambiguous_match() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "foo\nbar\nfoo\n").unwrap();

        let tracker = make_tracker();
        let abs_path = file.to_string_lossy().to_string();
        tracker.record_read(&abs_path, "foo\nbar\nfoo\n");

        let tool = EditFile::new(dir.path().to_str().unwrap(), tracker);
        let result = tool
            .execute(r#"{"path": "test.rs", "old_string": "foo", "new_string": "baz"}"#)
            .await;
        assert!(
            result.contains("found 2 times"),
            "expected ambiguity error, got: {result}"
        );
        assert!(result.contains("lines:"));
    }

    #[tokio::test]
    async fn edit_file_replace_all() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "foo\nbar\nfoo\n").unwrap();

        let tracker = make_tracker();
        let abs_path = file.to_string_lossy().to_string();
        tracker.record_read(&abs_path, "foo\nbar\nfoo\n");

        let tool = EditFile::new(dir.path().to_str().unwrap(), tracker);
        let result = tool
            .execute(
                r#"{"path": "test.rs", "old_string": "foo", "new_string": "baz", "replace_all": true}"#,
            )
            .await;
        assert!(
            result.contains("replaced 2 occurrences"),
            "expected replace_all success, got: {result}"
        );

        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "baz\nbar\nbaz\n");
    }

    #[tokio::test]
    async fn edit_file_not_found_string() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn main() {}").unwrap();

        let tracker = make_tracker();
        let abs_path = file.to_string_lossy().to_string();
        tracker.record_read(&abs_path, "fn main() {}");

        let tool = EditFile::new(dir.path().to_str().unwrap(), tracker);
        let result = tool
            .execute(r#"{"path": "test.rs", "old_string": "nonexistent", "new_string": "x"}"#)
            .await;
        assert!(
            result.contains("old_string not found"),
            "expected not-found error, got: {result}"
        );
    }

    #[tokio::test]
    async fn edit_file_allows_subsequent_edit_without_reread() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "aaa bbb ccc").unwrap();

        let tracker = make_tracker();
        let read_tool = ReadFile::new(dir.path().to_str().unwrap()).with_tracker(tracker.clone());
        let edit_tool = EditFile::new(dir.path().to_str().unwrap(), tracker);

        // Read, then edit twice without re-reading.
        read_tool.execute(r#"{"path": "test.rs"}"#).await;

        let r1 = edit_tool
            .execute(r#"{"path": "test.rs", "old_string": "aaa", "new_string": "xxx"}"#)
            .await;
        assert!(r1.contains("Edited"), "first edit failed: {r1}");

        let r2 = edit_tool
            .execute(r#"{"path": "test.rs", "old_string": "bbb", "new_string": "yyy"}"#)
            .await;
        assert!(r2.contains("Edited"), "second edit failed: {r2}");

        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "xxx yyy ccc");
    }

    #[tokio::test]
    async fn edit_file_blocks_path_traversal() {
        let tracker = make_tracker();
        let tool = EditFile::new("/tmp", tracker);
        let result = tool
            .execute(r#"{"path": "../../../etc/passwd", "old_string": "x", "new_string": "y"}"#)
            .await;
        assert_eq!(result, "Error: path traversal not allowed");
    }

    // ── WriteFile tests ────────────────────────────────────────────

    #[tokio::test]
    async fn write_file_creates_new_without_read() {
        let dir = tempfile::tempdir().unwrap();
        let tracker = make_tracker();
        let tool = WriteFile::new(dir.path().to_str().unwrap(), tracker);

        let result = tool
            .execute(r#"{"path": "new.rs", "content": "pub fn hello() {}\n"}"#)
            .await;
        assert!(result.contains("Wrote"), "expected success, got: {result}");

        let content = std::fs::read_to_string(dir.path().join("new.rs")).unwrap();
        assert!(content.contains("pub fn hello()"));
    }

    #[tokio::test]
    async fn write_file_requires_read_for_existing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("existing.rs");
        std::fs::write(&file, "old content").unwrap();

        let tracker = make_tracker();
        let tool = WriteFile::new(dir.path().to_str().unwrap(), tracker);

        let result = tool
            .execute(r#"{"path": "existing.rs", "content": "new content"}"#)
            .await;
        assert!(
            result.contains("must read this file before overwriting"),
            "expected enforcement error, got: {result}"
        );

        // File should be unchanged.
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "old content");
    }

    #[tokio::test]
    async fn write_file_succeeds_after_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("existing.rs");
        std::fs::write(&file, "old content").unwrap();

        let tracker = make_tracker();
        let read_tool = ReadFile::new(dir.path().to_str().unwrap()).with_tracker(tracker.clone());
        let write_tool = WriteFile::new(dir.path().to_str().unwrap(), tracker);

        read_tool.execute(r#"{"path": "existing.rs"}"#).await;

        let result = write_tool
            .execute(r#"{"path": "existing.rs", "content": "new content"}"#)
            .await;
        assert!(result.contains("Wrote"), "expected success, got: {result}");

        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "new content");
    }

    #[tokio::test]
    async fn write_file_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let tracker = make_tracker();
        let tool = WriteFile::new(dir.path().to_str().unwrap(), tracker);

        let result = tool
            .execute(r#"{"path": "deep/nested/dir/file.rs", "content": "hello"}"#)
            .await;
        assert!(result.contains("Wrote"), "expected success, got: {result}");

        let content = std::fs::read_to_string(dir.path().join("deep/nested/dir/file.rs")).unwrap();
        assert_eq!(content, "hello");
    }

    #[tokio::test]
    async fn write_file_blocks_path_traversal() {
        let tracker = make_tracker();
        let tool = WriteFile::new("/tmp", tracker);
        let result = tool
            .execute(r#"{"path": "../../../tmp/evil.sh", "content": "bad"}"#)
            .await;
        assert_eq!(result, "Error: path traversal not allowed");
    }

    #[test]
    fn edit_file_definition_has_tool_spec_fields() {
        let tracker = make_tracker();
        let tool = EditFile::new("/tmp", tracker);
        let def = tool.definition();
        assert_eq!(def.function.name, "edit_file");
        assert!(def.function.description.contains("When to use:"));
        assert!(def.function.description.contains("When NOT to use:"));
    }

    #[test]
    fn write_file_definition_has_tool_spec_fields() {
        let tracker = make_tracker();
        let tool = WriteFile::new("/tmp", tracker);
        let def = tool.definition();
        assert_eq!(def.function.name, "write_file");
        assert!(def.function.description.contains("When to use:"));
        assert!(def.function.description.contains("When NOT to use:"));
    }

    // ── read_file line numbers and offset/limit tests ──────────────

    #[tokio::test]
    async fn read_file_output_has_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.rs"), "use std::fs;\nfn main() {}\n").unwrap();

        let tool = ReadFile::new(dir.path().to_str().unwrap());
        let result = tool.execute(r#"{"path": "test.rs"}"#).await;
        assert!(result.contains("L1: use std::fs;"), "got: {result}");
        assert!(result.contains("L2: fn main() {}"), "got: {result}");
    }

    #[tokio::test]
    async fn read_file_offset_skips_lines() {
        let dir = tempfile::tempdir().unwrap();
        let content = (1..=10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.path().join("test.txt"), &content).unwrap();

        let tool = ReadFile::new(dir.path().to_str().unwrap());
        let result = tool
            .execute(r#"{"path": "test.txt", "offset": 3, "limit": 2}"#)
            .await;
        assert!(result.contains("L3: line 3"), "got: {result}");
        assert!(result.contains("L4: line 4"), "got: {result}");
        assert!(
            !result.contains("L1:"),
            "should not contain line 1, got: {result}"
        );
        assert!(
            !result.contains("L5:"),
            "should not contain line 5, got: {result}"
        );
    }

    #[tokio::test]
    async fn read_file_truncation_notice() {
        let dir = tempfile::tempdir().unwrap();
        let content = (1..=100)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.path().join("test.txt"), &content).unwrap();

        let tool = ReadFile::new(dir.path().to_str().unwrap());
        let result = tool.execute(r#"{"path": "test.txt", "limit": 5}"#).await;
        assert!(result.contains("L1: line 1"), "got: {result}");
        assert!(result.contains("L5: line 5"), "got: {result}");
        assert!(!result.contains("L6:"), "should not contain line 6");
        assert!(
            result.contains("[truncated: 100 total lines"),
            "expected truncation notice, got: {result}"
        );
    }

    #[tokio::test]
    async fn read_file_long_line_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let long_line = "x".repeat(600);
        std::fs::write(dir.path().join("test.txt"), &long_line).unwrap();

        let tool = ReadFile::new(dir.path().to_str().unwrap());
        let result = tool.execute(r#"{"path": "test.txt"}"#).await;
        assert!(
            result.contains("[line truncated at 500 chars]"),
            "expected line truncation, got: {result}"
        );
    }

    #[tokio::test]
    async fn read_file_args_schema_has_offset_and_limit() {
        let schema = crate::json_schema_for::<ReadFileArgs>();
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("offset"));
        assert!(props.contains_key("limit"));
        // offset and limit should NOT be required.
        let required = schema["required"].as_array().unwrap();
        assert!(!required.contains(&serde_json::json!("offset")));
        assert!(!required.contains(&serde_json::json!("limit")));
    }

    // ── grep mode tests ────────────────────────────────────────────

    #[tokio::test]
    async fn grep_default_mode_returns_files_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello world\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "hello again\n").unwrap();

        let tool = Grep::new(dir.path().to_str().unwrap());
        let result = tool.execute(r#"{"pattern": "hello"}"#).await;
        // Files mode: paths only, no line numbers or content.
        assert!(result.contains("a.txt"), "got: {result}");
        assert!(result.contains("b.txt"), "got: {result}");
        // Should NOT contain line content or line numbers.
        assert!(
            !result.contains("hello world"),
            "files mode should not contain content: {result}"
        );
    }

    #[tokio::test]
    async fn grep_content_mode_returns_matching_lines() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.rs"), "fn main() {}\nfn helper() {}\n").unwrap();

        let tool = Grep::new(dir.path().to_str().unwrap());
        let result = tool
            .execute(r#"{"pattern": "fn main", "mode": "content"}"#)
            .await;
        // Content mode: includes line numbers and matching content.
        assert!(result.contains("fn main"), "got: {result}");
        assert!(
            result.contains(":1:"),
            "expected line number, got: {result}"
        );
    }

    #[tokio::test]
    async fn grep_count_mode_returns_counts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.rs"), "foo\nbar\nfoo\n").unwrap();

        let tool = Grep::new(dir.path().to_str().unwrap());
        let result = tool.execute(r#"{"pattern": "foo", "mode": "count"}"#).await;
        // Count mode: file:count format. Should show 2 matches.
        assert!(result.contains(":2"), "expected count of 2, got: {result}");
    }

    #[tokio::test]
    async fn grep_content_mode_with_context_lines() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.rs"), "aaa\nbbb\nccc\nddd\neee\n").unwrap();

        let tool = Grep::new(dir.path().to_str().unwrap());
        let result = tool
            .execute(r#"{"pattern": "ccc", "mode": "content", "context_lines": 1}"#)
            .await;
        // Should include surrounding context lines.
        assert!(
            result.contains("bbb"),
            "expected context before, got: {result}"
        );
        assert!(
            result.contains("ddd"),
            "expected context after, got: {result}"
        );
    }

    #[tokio::test]
    async fn grep_invalid_mode_returns_error() {
        let tool = Grep::new("/tmp");
        let result = tool
            .execute(r#"{"pattern": "test", "mode": "invalid"}"#)
            .await;
        assert!(
            result.contains("invalid mode"),
            "expected mode error, got: {result}"
        );
    }

    #[test]
    fn grep_args_schema_has_mode_and_context_lines() {
        let schema = crate::json_schema_for::<GrepArgs>();
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("mode"));
        assert!(props.contains_key("context_lines"));
    }
}
