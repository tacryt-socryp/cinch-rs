//! Terminal coding agent powered by cinch-rs.
//!
//! `cinch-code` provides a ready-to-use coding agent with git awareness,
//! built on the cinch-rs agent framework and cinch-tui terminal UI.
//!
//! # Library usage
//!
//! Use the library components to build custom coding agents:
//!
//! ```ignore
//! use cinch_code::{CodeConfig, GitToolsExt, coding_system_prompt};
//! use cinch_rs::tools::core::ToolSet;
//!
//! // Use CodeConfig for batteries-included setup
//! let config = CodeConfig::default();
//! let tools = config.build_tool_set();
//! let harness_config = config.build_harness_config();
//!
//! // Or add git tools to an existing ToolSet
//! let tools = ToolSet::new()
//!     .with_common_tools(".")
//!     .with_git_tools(".");
//! ```
//!
//! # Binary
//!
//! The `cinch-code` binary provides a TUI-based interactive coding agent:
//!
//! ```sh
//! # One-shot mode
//! cinch-code --prompt "Add error handling to src/main.rs"
//!
//! # Interactive mode (default)
//! cinch-code --workdir /path/to/project
//! ```

pub mod config;
pub mod prompt;
pub mod tools;

pub use config::CodeConfig;
pub use prompt::coding_system_prompt;
pub use tools::GitToolsExt;
