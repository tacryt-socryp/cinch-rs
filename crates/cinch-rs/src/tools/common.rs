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
//! | [`ListFiles`] | `list_files` | List a directory |
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
use crate::tools::core::{Tool, ToolFuture};
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
}

/// Typed arguments for `list_files`.
#[derive(Deserialize, JsonSchema)]
pub struct ListFilesArgs {
    /// Directory path relative to repo root (e.g. 'docs/').
    pub path: String,
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
}

/// Typed arguments for `find_files`.
#[derive(Deserialize, JsonSchema)]
pub struct FindFilesArgs {
    /// Glob pattern (e.g. 'src/**/*.rs', 'docs/*.md').
    pub pattern: String,
}

/// Typed arguments for `shell`.
#[derive(Deserialize, JsonSchema)]
pub struct ShellArgs {
    /// Shell command to execute (e.g. 'wc -l *.md', 'git log --oneline -5').
    pub command: String,
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

impl Tool for ReadFile {
    fn definition(&self) -> ToolDef {
        ToolSpec::builder("read_file")
            .purpose("Read a file from the repository")
            .when_to_use("When you need to read a specific file whose path you already know")
            .when_not_to_use(
                "When searching for a pattern across many files — use grep instead. \
                 When you need to list files in a directory — use list_files instead",
            )
            .parameters_for::<ReadFileArgs>()
            .example(
                "read_file(path='docs/readme.md')",
                "Returns the full text content of the file",
            )
            .output_format("Raw file content as text")
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
                     Use list_files to browse directories.",
                    args.path
                );
            }

            match fs::read_to_string(&full_path).await {
                Ok(content) => {
                    // Register the read with full (untruncated) content.
                    if let Some(ref t) = tracker {
                        t.record_read(&full_path.to_string_lossy(), &content);
                    }
                    truncate_result(content, max)
                }
                Err(e) => format!("Error reading '{}': {e}", full_path.display()),
            }
        })
    }
}

// ── ListFiles ───────────────────────────────────────────────────────

/// List files in a directory under the working directory.
///
/// Path traversal (`..`) is blocked. Uses `ls -ap1t` for the listing
/// (one entry per line, directories marked with trailing `/`).
pub struct ListFiles {
    workdir: String,
}

impl ListFiles {
    pub fn new(workdir: impl Into<String>) -> Self {
        Self {
            workdir: workdir.into(),
        }
    }
}

impl Tool for ListFiles {
    fn definition(&self) -> ToolDef {
        ToolSpec::builder("list_files")
            .purpose("List files in a directory")
            .when_to_use(
                "When you need to discover what files exist in a specific directory",
            )
            .when_not_to_use(
                "When searching for files by glob pattern across nested directories — \
                 use find_files instead. When you already know the file path — use read_file",
            )
            .parameters_for::<ListFilesArgs>()
            .example(
                "list_files(path='docs/')",
                "Returns one entry per line, sorted newest first. Directories have a trailing '/'",
            )
            .output_format("One entry per line, newest first. Directories end with '/'.")
            .disambiguate(
                "Need to find files matching a glob pattern recursively",
                "find_files",
                "find_files supports glob patterns across nested directories; list_files shows a single directory",
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
            let args: ListFilesArgs = match serde_json::from_str(&arguments) {
                Ok(a) => a,
                Err(_) => return "Error: 'path' argument is required".to_string(),
            };
            if args.path.contains("..") {
                return "Error: path traversal not allowed".to_string();
            }
            let full_path = Path::new(&workdir).join(&args.path);
            // -a: include hidden files, -p: append '/' to dirs, -1: one per
            // line, -t: sort newest first so the most recent entries appear
            // before any output truncation.
            run_command("ls", &["-ap1t", &full_path.to_string_lossy()], &[]).await
        })
    }
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
        ToolSpec::builder("grep")
            .purpose("Search for a regex pattern in file contents")
            .when_to_use("When you need to find text matching a pattern across multiple files")
            .when_not_to_use(
                "When you already know the file path — use read_file instead. \
                 When you need to find files by name — use find_files instead",
            )
            .parameters_for::<GrepArgs>()
            .example(
                "grep(pattern='TODO', glob='*.rs')",
                "Returns matching lines with file:line_number prefix",
            )
            .output_format("Matching lines prefixed with file_path:line_number:")
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

            let mut cmd_args = vec![
                "-rn".to_string(),
                "--color=never".to_string(),
                format!("--max-count={max_matches}"),
            ];

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
        ToolSpec::builder("find_files")
            .purpose("Find files matching a glob pattern")
            .when_to_use(
                "When you need to discover files by name or extension across nested directories",
            )
            .when_not_to_use(
                "When listing files in a single known directory — use list_files instead. \
                 When searching file content — use grep instead",
            )
            .parameters_for::<FindFilesArgs>()
            .example(
                "find_files(pattern='docs/**/*.md')",
                "Returns a sorted list of matching file paths",
            )
            .output_format("Newline-separated list of file paths relative to repo root")
            .disambiguate(
                "Need to see files in one directory",
                "list_files",
                "list_files shows one directory with details; find_files searches recursively by pattern",
            )
            .build()
            .to_tool_def()
    }

    fn cacheable(&self) -> bool {
        true
    }

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let workdir = self.workdir.clone();
        let max_results = self.max_results;
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
            let pattern = &args.pattern;
            let result = run_shell(
                &workdir,
                &format!(
                    "find . -path './{pattern}' -type f 2>/dev/null | head -{max_results} | sort"
                ),
            )
            .await;
            if result.trim().is_empty() {
                format!("No files found matching '{pattern}'")
            } else {
                truncate_result(result, max_result_bytes)
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
        ToolSpec::builder("shell")
            .purpose("Run a shell command and return its output")
            .when_to_use(
                "When you need an operation not covered by other tools: git commands, \
                 word counts, date calculations, file manipulation, data processing, etc. \
                 Commands run in the repo root directory",
            )
            .when_not_to_use(
                "When a dedicated tool exists for the task — use read_file to read files, \
                 grep to search content, find_files to find files by name. \
                 Prefer dedicated tools for better error handling",
            )
            .parameters_for::<ShellArgs>()
            .example(
                "shell(command='git log --oneline -5')",
                "Returns the last 5 git commits as one-line summaries",
            )
            .output_format("Command stdout (and stderr if non-empty)")
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
            let result = run_shell(&workdir, &args.command).await;
            truncate_result(result, max)
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
        ToolSpec::builder("web_search")
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
        ToolSpec::builder("edit_file")
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
                    return "Error: 'path', 'old_string', and 'new_string' are required".to_string();
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
        ToolSpec::builder("write_file")
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
fn format_output(output: std::process::Output, lenient_exit_codes: &[i32]) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let ok = output.status.success()
        || output
            .status
            .code()
            .is_some_and(|c| lenient_exit_codes.contains(&c));
    if ok {
        if stderr.is_empty() {
            stdout
        } else {
            format!("{stdout}\n[stderr]\n{stderr}")
        }
    } else {
        format!("Command failed ({}):\n{stdout}\n{stderr}", output.status)
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
    fn list_files_definition_has_tool_spec_fields() {
        let tool = ListFiles::new("/tmp");
        let def = tool.definition();
        assert_eq!(def.function.name, "list_files");
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
            .with(ListFiles::new("/tmp"))
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
            result.contains("list_files"),
            "expected list_files suggestion, got: {result}"
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
    async fn list_files_blocks_path_traversal() {
        let tool = ListFiles::new("/tmp");
        let result = tool.execute(r#"{"path": "../../secret"}"#).await;
        assert_eq!(result, "Error: path traversal not allowed");
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
}
