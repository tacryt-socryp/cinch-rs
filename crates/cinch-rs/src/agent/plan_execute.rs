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
                "think".into(),
                "todo".into(),
                // Exploration tools.
                "read_file".into(),
                "list_dir".into(),
                "grep".into(),
                "find_files".into(),
                "shell".into(),
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
/// Teaches the agent the purpose of the phase and how to use the available
/// tools to orient itself before acting. Emphasizes `think` and `todo` as
/// free reasoning tools and explains when to call `submit_plan`.
const DEFAULT_PLANNING_PROMPT: &str = "\
You are in the ORIENT & PLAN phase. Your goal is to understand the situation \
before taking action.

Use your exploration tools (read_file, grep, list_dir, shell, etc.) to gather \
the information you need. Follow threads — if something looks interesting, \
investigate further.

When you've explored enough, use the `think` tool (free — doesn't consume a \
round) to reason about what you've found and decide on your approach. Then use \
the `todo` tool (also free) to outline your plan as a checklist.

When you have a clear understanding of the problem and a concrete plan, call \
`submit_plan` to transition to execution. Don't submit until you're confident \
in your approach — but don't over-plan either. A few clear steps are better \
than an exhaustive specification.";

/// Default execution phase prompt.
const DEFAULT_EXECUTION_PROMPT: &str = "\
You are now in the EXECUTE phase. All tools are available. Follow the plan you \
created, adapting as needed if you discover something unexpected. Mark todo \
items as in-progress when you start them and complete when done.";

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
             Call this when you understand the problem, have gathered the \
             information you need, and have outlined your approach (ideally \
             in the todo list). After calling this, all tools become available \
             and you should follow your plan.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "Brief summary of what you plan to do and why"
                    }
                },
                "required": ["summary"]
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
