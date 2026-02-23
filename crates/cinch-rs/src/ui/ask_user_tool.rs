//! LLM-callable tool for asking the human operator a question.
//!
//! When the LLM needs human input during its tool-use loop, it calls
//! `ask_user` with a prompt and choices. The tool presents the question
//! via [`ask_question`] and polls for a response.
//!
//! In headless mode (no UI state), the tool returns `timed_out` immediately
//! so the calling code can implement its own fallback.

use std::sync::{Arc, Mutex};

use schemars::JsonSchema;
use serde::Deserialize;

use crate::ToolDef;
use crate::tools::core::{Tool, ToolFuture};
use crate::tools::spec::ToolSpec;

use super::{QuestionChoice, UiState, UserQuestion, ask_question, poll_question};

/// Arguments for the `ask_user` tool.
#[derive(Deserialize, JsonSchema)]
struct AskUserArgs {
    /// The question to ask the human operator.
    prompt: String,
    /// 2-10 selectable options presented to the user.
    choices: Vec<String>,
    /// Whether the user can edit the selected option before confirming.
    #[serde(default)]
    editable: bool,
    /// Seconds before the question times out (default: 120).
    #[serde(default = "default_timeout")]
    timeout: u64,
}

fn default_timeout() -> u64 {
    120
}

/// Tool that lets the LLM ask the human operator a question.
///
/// Internally calls [`ask_question`] on [`UiState`] and polls for a response.
/// When no UI is attached (headless mode), returns `timed_out` immediately.
///
/// # Example
///
/// ```ignore
/// let tool = AskUserTool::new(Some(ui_state.clone()));
/// tool_set.register(tool);
/// ```
pub struct AskUserTool {
    ui_state: Option<Arc<Mutex<UiState>>>,
}

impl AskUserTool {
    /// Create a new `ask_user` tool.
    ///
    /// Pass `Some(state)` when a UI frontend is active, or `None` for
    /// headless mode (the tool will return `timed_out` immediately).
    pub fn new(ui_state: Option<Arc<Mutex<UiState>>>) -> Self {
        Self { ui_state }
    }
}

impl Tool for AskUserTool {
    fn definition(&self) -> ToolDef {
        ToolSpec::builder("ask_user")
            .purpose(
                "Ask the human operator a question and wait for their response. \
                 Use this when you need human judgment or approval before proceeding.",
            )
            .when_to_use(
                "When you need the operator to choose between options, confirm an action, \
                 or provide input that requires human judgment",
            )
            .when_not_to_use(
                "For routine decisions the agent can make autonomously. \
                 Do not use this as a delay tactic or for trivial confirmations",
            )
            .parameters_for::<AskUserArgs>()
            .example(
                r#"ask_user(prompt="Which tweet should we post?", choices=["Tweet A: ...", "Tweet B: ..."])"#,
                r#"{"status": "selected", "index": 0, "text": "Tweet A: ..."}"#,
            )
            .output_format(
                "JSON object with 'status' (selected|edited|skipped|timed_out), \
                 'index' (selected choice index or null), and 'text' (choice text or null)",
            )
            .build()
            .to_tool_def()
    }

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let arguments = arguments.to_string();
        Box::pin(async move {
            let args: AskUserArgs = match serde_json::from_str(&arguments) {
                Ok(a) => a,
                Err(e) => return format!("Error: invalid arguments: {e}"),
            };

            if args.choices.len() < 2 {
                return "Error: at least 2 choices are required".to_string();
            }
            if args.choices.len() > 10 {
                return "Error: at most 10 choices are allowed".to_string();
            }

            let Some(ref state) = self.ui_state else {
                // Headless mode — no UI attached.
                return r#"{"status": "timed_out", "index": null, "text": null}"#.to_string();
            };

            let question = UserQuestion {
                prompt: args.prompt,
                choices: args
                    .choices
                    .iter()
                    .enumerate()
                    .map(|(i, c)| QuestionChoice {
                        label: format!("Option {}", i + 1),
                        body: c.clone(),
                        metadata: String::new(),
                    })
                    .collect(),
                editable: args.editable,
                max_edit_length: None,
            };

            ask_question(state, question, args.timeout);

            // Poll until the question is resolved.
            loop {
                if let Some(response) = poll_question(state) {
                    return format_response(&response, &args.choices);
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        })
    }
}

fn format_response(response: &super::QuestionResponse, choices: &[String]) -> String {
    match response {
        super::QuestionResponse::Selected(idx) => {
            let text = choices.get(*idx).cloned().unwrap_or_default();
            serde_json::json!({
                "status": "selected",
                "index": idx,
                "text": text,
            })
            .to_string()
        }
        super::QuestionResponse::SelectedEdited { index, edited_text } => serde_json::json!({
            "status": "edited",
            "index": index,
            "text": edited_text,
        })
        .to_string(),
        super::QuestionResponse::Skipped => {
            r#"{"status": "skipped", "index": null, "text": null}"#.to_string()
        }
        super::QuestionResponse::TimedOut => {
            r#"{"status": "timed_out", "index": null, "text": null}"#.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_user_tool_definition_has_required_fields() {
        let tool = AskUserTool::new(None);
        let def = tool.definition();
        assert_eq!(def.function.name, "ask_user");
        assert!(!def.function.description.is_empty());

        // Check that the schema includes prompt and choices as required.
        let schema = &def.function.parameters;
        let required = schema.get("required").and_then(|v| v.as_array());
        assert!(required.is_some());
        let required: Vec<&str> = required
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(required.contains(&"prompt"));
        assert!(required.contains(&"choices"));
    }

    #[tokio::test]
    async fn ask_user_tool_headless_returns_timed_out() {
        let tool = AskUserTool::new(None);
        let result = tool
            .execute(r#"{"prompt": "Pick one:", "choices": ["A", "B"]}"#)
            .await;
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "timed_out");
        assert!(parsed["index"].is_null());
    }

    #[tokio::test]
    async fn ask_user_tool_validates_too_few_choices() {
        let tool = AskUserTool::new(None);
        let result = tool
            .execute(r#"{"prompt": "Pick:", "choices": ["A"]}"#)
            .await;
        assert!(result.starts_with("Error:"));
        assert!(result.contains("at least 2"));
    }

    #[tokio::test]
    async fn ask_user_tool_validates_too_many_choices() {
        let tool = AskUserTool::new(None);
        let choices: Vec<String> = (1..=11).map(|i| format!("Option {i}")).collect();
        let args = serde_json::json!({
            "prompt": "Pick:",
            "choices": choices,
        });
        let result = tool.execute(&args.to_string()).await;
        assert!(result.starts_with("Error:"));
        assert!(result.contains("at most 10"));
    }

    #[tokio::test]
    async fn ask_user_tool_with_ui_state() {
        let state = Arc::new(Mutex::new(UiState::default()));
        let tool = AskUserTool::new(Some(state.clone()));

        // Spawn the tool in a background task.
        let tool_handle = tokio::spawn(async move {
            tool.execute(r#"{"prompt": "Pick one:", "choices": ["A", "B", "C"]}"#)
                .await
        });

        // Wait for the question to appear.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Simulate user selecting option B.
        {
            let mut s = state.lock().unwrap();
            let aq = s.active_question.as_mut().unwrap();
            aq.response = Some(super::super::QuestionResponse::Selected(1));
            aq.done = true;
        }

        let result = tool_handle.await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "selected");
        assert_eq!(parsed["index"], 1);
        assert_eq!(parsed["text"], "B");
    }

    #[test]
    fn format_response_variants() {
        use super::super::QuestionResponse;

        let choices = vec!["A".into(), "B".into(), "C".into()];

        let r = format_response(&QuestionResponse::Selected(0), &choices);
        let v: serde_json::Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["status"], "selected");
        assert_eq!(v["index"], 0);
        assert_eq!(v["text"], "A");

        let r = format_response(
            &QuestionResponse::SelectedEdited {
                index: 1,
                edited_text: "modified B".into(),
            },
            &choices,
        );
        let v: serde_json::Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["status"], "edited");
        assert_eq!(v["text"], "modified B");

        let r = format_response(&QuestionResponse::Skipped, &choices);
        let v: serde_json::Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["status"], "skipped");

        let r = format_response(&QuestionResponse::TimedOut, &choices);
        let v: serde_json::Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["status"], "timed_out");
    }
}
