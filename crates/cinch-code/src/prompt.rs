//! System prompt for the coding agent.

/// Returns a minimal coding-focused system prompt.
///
/// This prompt is intentionally concise — the harness injects additional
/// context (memory instructions, project instructions, memory index) via
/// `inject_prompt_extras` or the prompt registry.
pub fn coding_system_prompt() -> String {
    "\
You are a coding assistant. You have access to tools for reading, editing, \
and searching files, running shell commands, and performing git operations.

Guidelines:
- Read files before editing them.
- Make minimal, focused changes.
- Use git tools to understand the repository state.
- Explain what you're doing before making changes."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_is_non_empty() {
        let prompt = coding_system_prompt();
        assert!(!prompt.is_empty());
        assert!(prompt.contains("coding assistant"));
    }
}
