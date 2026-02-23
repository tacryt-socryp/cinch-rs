//! Structured reflection on tool failures.
//!
//! When a tool returns an error, wrap it in a structured format that helps
//! the LLM reason about what went wrong and how to recover.

/// Format a tool failure for structured reflection.
///
/// Returns a rich error message that includes:
/// - The original error
/// - Possible causes
/// - Suggested recovery actions
pub fn format_tool_failure(tool_name: &str, arguments: &str, error: &str) -> String {
    let mut msg = format!("Error from tool '{tool_name}':\n  {error}\n");

    // Analyze common error patterns and add suggestions.
    let suggestions = analyze_error(tool_name, error);

    if !suggestions.is_empty() {
        msg.push_str("\nPossible causes and recovery:\n");
        for suggestion in &suggestions {
            msg.push_str(&format!("  - {suggestion}\n"));
        }
    }

    // Include truncated arguments for context.
    let args_preview: String = arguments.chars().take(200).collect();
    msg.push_str(&format!("\nArguments used: {args_preview}"));
    if arguments.len() > 200 {
        msg.push_str("...");
    }

    msg
}

/// Analyze an error and return recovery suggestions.
fn analyze_error(tool_name: &str, error: &str) -> Vec<String> {
    let error_lower = error.to_lowercase();
    let mut suggestions = Vec::new();

    // File not found.
    if error_lower.contains("not found")
        || error_lower.contains("no such file")
        || error_lower.contains("does not exist")
    {
        suggestions.push("Check that the file path is correct. Use list_files or find_files to discover the right path.".into());
        if tool_name == "read_file" {
            suggestions.push(
                "The file may have been moved or renamed. Try searching with grep or find_files."
                    .into(),
            );
        }
    }

    // Permission denied.
    if error_lower.contains("permission denied") || error_lower.contains("access denied") {
        suggestions.push(
            "The file or directory may have restricted permissions. Try a different approach."
                .into(),
        );
    }

    // Path traversal blocked.
    if error_lower.contains("path traversal") || error_lower.contains("outside") {
        suggestions
            .push("The path must be within the working directory. Use a relative path.".into());
    }

    // Command blocked.
    if error_lower.contains("blocked") || error_lower.contains("forbidden") {
        suggestions.push("This command is blocked for safety. Try an alternative approach.".into());
    }

    // Timeout.
    if error_lower.contains("timed out") || error_lower.contains("timeout") {
        suggestions.push(
            "The operation took too long. Try with smaller input or different arguments.".into(),
        );
    }

    // JSON parsing.
    if error_lower.contains("json") || error_lower.contains("parse") {
        suggestions.push(
            "Check that the arguments are valid JSON with correct field names and types.".into(),
        );
    }

    // Generic fallback.
    if suggestions.is_empty() {
        suggestions.push("Review the error message and adjust your approach.".into());
        suggestions.push(
            "Consider using the 'think' tool to reason about what went wrong before retrying."
                .into(),
        );
    }

    suggestions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_failure_includes_error() {
        let result = format_tool_failure("read_file", r#"{"path": "foo.rs"}"#, "File not found");
        assert!(result.contains("read_file"));
        assert!(result.contains("File not found"));
        assert!(result.contains("list_files"));
    }

    #[test]
    fn format_failure_includes_suggestions() {
        let result = format_tool_failure("shell", r#"{"cmd": "rm -rf /"}"#, "Command blocked");
        assert!(result.contains("blocked"));
        assert!(result.contains("alternative"));
    }

    #[test]
    fn format_failure_json_error() {
        let result = format_tool_failure("grep", "bad json", "JSON parse error");
        assert!(result.contains("valid JSON"));
    }
}
