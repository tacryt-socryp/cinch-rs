//! Canonical tool name constants.
//!
//! All tool-name string literals should reference these constants to avoid
//! scattered magic strings. When a tool is renamed, only this file needs
//! to change.

pub const READ_FILE: &str = "read_file";
pub const EDIT_FILE: &str = "edit_file";
pub const WRITE_FILE: &str = "write_file";
pub const LIST_DIR: &str = "list_dir";
pub const FIND_FILES: &str = "find_files";
pub const GREP: &str = "grep";
pub const SHELL: &str = "shell";
pub const WEB_SEARCH: &str = "web_search";
pub const THINK: &str = "think";
pub const TODO: &str = "todo";
