//! Plan-then-execute two-phase workflow.
//!
//! The agent starts in the **orient & plan** phase with exploration tools
//! (read, search, shell, think, todo) but no mutation tools (write, post,
//! deploy). Once the agent understands the situation and has a plan, it calls
//! `submit_plan` to transition to the **execute** phase where all tools are
//! available.
//!
//! This separation prevents aimless tool-calling — the agent can't start
//! writing before it understands the problem — while still allowing real
//! exploration (shell commands, file reads, searches) during planning.
//!
//! Based on patterns from Claude Code (plan mode) and LATS/React.

use crate::ToolDef;

/// Phase of a plan-then-execute workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Orient & plan phase: exploration tools available, mutation tools hidden.
    Planning,
    /// Execution phase: all tools enabled, agent follows its plan.
    Executing,
}

/// Configuration for plan-then-execute workflow.
#[derive(Debug, Clone)]
pub struct PlanExecuteConfig {
    /// Maximum rounds for the planning phase. After this many rounds, the
    /// harness auto-transitions to execution even without `submit_plan`.
    /// Default: 10.
    pub max_planning_rounds: u32,
    /// Tool names allowed during planning. Tools not in this list are hidden
    /// from the LLM during the planning phase. Any name listed here that
    /// doesn't correspond to a registered tool is silently ignored.
    pub planning_tools: Vec<String>,
    /// Prompt injected as a user message at the start of the planning phase.
    pub planning_prompt: String,
    /// Prompt injected as a user message when transitioning to execution.
    pub execution_prompt: String,
}

impl Default for PlanExecuteConfig {
    fn default() -> Self {
        Self {
            max_planning_rounds: 10,
            planning_tools: vec![
                // Reasoning tools (free — don't consume rounds).
                crate::tools::names::THINK.into(),
                crate::tools::names::TODO.into(),
                // Exploration tools.
                crate::tools::names::READ_FILE.into(),
                crate::tools::names::LIST_DIR.into(),
                crate::tools::names::GREP.into(),
                crate::tools::names::FIND_FILES.into(),
                crate::tools::names::SHELL.into(),
                // Sub-agent delegation (if registered).
                "delegate_sub_agent".into(),
            ],
            planning_prompt: DEFAULT_PLANNING_PROMPT.into(),
            execution_prompt: DEFAULT_EXECUTION_PROMPT.into(),
        }
    }
}

/// Default planning phase prompt.
///
/// Guides the agent through structured exploration and plan formulation.
/// Emphasizes concrete, file-level planning over vague summaries.
const DEFAULT_PLANNING_PROMPT: &str = "\
You are in the PLAN phase. Mutation tools (write, edit, etc.) are disabled \
until you submit a plan. Your goal is to understand the problem and produce a \
concrete, actionable plan before making any changes.

## Step 1 — Understand the request
Restate the user's goal in your own words so you're clear on what success \
looks like.

## Step 2 — Explore the codebase
Use read_file, grep, list_dir, and find_files to understand the relevant code. \
Make multiple tool calls in parallel when exploring independent areas. Follow \
references — if a function delegates to another module, read that module too.

## Step 3 — Identify scope
Determine exactly which files and functions need to change. Look for existing \
patterns, utilities, and conventions you should reuse rather than reinvent.

## Step 4 — Design your approach
Use `think` (free — doesn't consume a round) to reason about tradeoffs. Then \
use `todo` (also free) to create a checklist. Each todo item should be specific \
enough to execute without further exploration, e.g.:
  - \"Add `sort` field to `ProviderPreferences` in src/lib.rs\"
  - \"Update `build_request()` in src/agent/execution.rs to pass provider\"

Avoid vague items like \"improve error handling\" or \"update the code\".

## Step 5 — Verification plan
Decide how you'll verify the changes work. This could include:
  - Running existing tests (`cargo test`, `npm test`, etc.)
  - Running linters or formatters
  - Manual verification steps

## Step 6 — Submit
Call `submit_plan` with a summary, the list of files you'll modify, your \
approach, and your verification plan. Don't submit until your plan references \
specific files and changes — but don't over-plan either. A few concrete steps \
are better than an exhaustive specification.";

/// Default execution phase prompt.
///
/// Uses `{plan_summary}` as a placeholder that gets replaced with the actual
/// plan summary at transition time.
const DEFAULT_EXECUTION_PROMPT: &str = "\
You are now in the EXECUTE phase. All tools are available.

## Your plan
{plan_summary}

## Guidelines
- Work through your todo list in order. Mark items in-progress when you start \
them and complete when done.
- Read each file before editing it — don't assume its contents from planning.
- If you discover something unexpected, adapt your plan rather than blindly \
following it. Add new todo items if needed.
- After completing all changes, verify by running any relevant tests or checks.";

impl PlanExecuteConfig {
    /// Filter tool definitions to only include planning-phase tools.
    ///
    /// Tools listed in `planning_tools` that don't exist in `all_tools` are
    /// silently skipped — this is intentional so that optional tools like
    /// `delegate_sub_agent` can appear in the default list without requiring
    /// every agent to register them.
    pub fn filter_planning_tools(&self, all_tools: &[ToolDef]) -> Vec<ToolDef> {
        all_tools
            .iter()
            .filter(|t| self.planning_tools.contains(&t.function.name))
            .cloned()
            .collect()
    }

    /// Create a tool definition for the `submit_plan` tool that signals
    /// transition from planning to execution phase.
    pub fn submit_plan_tool_def() -> ToolDef {
        ToolDef::new(
            "submit_plan",
            "Signal that you've finished planning and are ready to execute. \
             Call this after you've explored the codebase, created a todo \
             checklist, and are confident in your approach. After calling this, \
             all tools (including write/edit) become available.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "Brief summary of what you plan to do and why"
                    },
                    "files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of file paths that will be modified or created"
                    },
                    "approach": {
                        "type": "string",
                        "description": "Key design decisions: patterns to reuse, tradeoffs considered, why this approach over alternatives"
                    },
                    "verification": {
                        "type": "string",
                        "description": "How to verify the changes work: tests to run, commands to execute, manual checks to perform"
                    }
                },
                "required": ["summary", "files", "verification"]
            }),
        )
    }

    /// Check if a tool call represents a plan submission.
    pub fn is_plan_submission(tool_name: &str) -> bool {
        tool_name == "submit_plan"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_planning_tools_includes_defaults() {
        let config = PlanExecuteConfig::default();
        let all_tools = vec![
            ToolDef::new("read_file", "Read a file", serde_json::json!({})),
            ToolDef::new("shell", "Run shell command", serde_json::json!({})),
            ToolDef::new("save_draft", "Save a draft", serde_json::json!({})),
        ];

        let planning = config.filter_planning_tools(&all_tools);
        // read_file and shell are in the default planning tools, save_draft is not.
        assert_eq!(planning.len(), 2);
        let names: Vec<_> = planning.iter().map(|t| t.function.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"shell"));
        assert!(!names.contains(&"save_draft"));
    }

    #[test]
    fn filter_planning_tools_ignores_unregistered() {
        let config = PlanExecuteConfig::default();
        // delegate_sub_agent is in the default list but not registered here.
        let all_tools = vec![ToolDef::new(
            "read_file",
            "Read a file",
            serde_json::json!({}),
        )];

        let planning = config.filter_planning_tools(&all_tools);
        assert_eq!(planning.len(), 1);
        assert_eq!(planning[0].function.name, "read_file");
    }

    #[test]
    fn submit_plan_tool() {
        let def = PlanExecuteConfig::submit_plan_tool_def();
        assert_eq!(def.function.name, "submit_plan");
    }

    #[test]
    fn phase_equality() {
        assert_eq!(Phase::Planning, Phase::Planning);
        assert_ne!(Phase::Planning, Phase::Executing);
    }

    #[test]
    fn default_planning_rounds() {
        let config = PlanExecuteConfig::default();
        assert_eq!(config.max_planning_rounds, 10);
    }
}
