//! Structured tool descriptions with usage guidance.
//!
//! `ToolSpec` replaces free-form tool description strings with structured
//! metadata including purpose, when to use, when not to use, parameter
//! documentation, and usage examples. This dramatically improves LLM tool
//! selection accuracy (EASYTOOL ICLR 2024: 70% token cost reduction).

use crate::ToolDef;

/// A structured tool specification with rich usage guidance.
///
/// The `when_not_to_use` field is the single highest-ROI field — it prevents
/// the LLM from confusing semantically similar tools.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    /// Tool name (must be unique within a ToolSet).
    pub name: String,
    /// One-sentence imperative purpose: "Search file contents by regex pattern".
    pub purpose: String,
    /// When this tool should be used.
    pub when_to_use: String,
    /// When this tool should NOT be used (prevents confusion with similar tools).
    pub when_not_to_use: String,
    /// Parameter specifications with constraints.
    pub parameters: serde_json::Value,
    /// Usage examples as (input_description, expected_behavior) pairs.
    pub examples: Vec<UsageExample>,
    /// Description of the output format.
    pub output_format: String,
    /// Disambiguation examples: situations where this tool is commonly
    /// confused with another tool, with clarification.
    pub disambiguation: Vec<DisambiguationExample>,
}

/// An example clarifying when to use this tool vs a similar one.
#[derive(Debug, Clone)]
pub struct DisambiguationExample {
    /// The scenario where confusion arises.
    pub scenario: String,
    /// The correct tool to use in this scenario.
    pub correct_tool: String,
    /// Brief explanation of why.
    pub reason: String,
}

/// A usage example for a tool.
#[derive(Debug, Clone)]
pub struct UsageExample {
    /// Description of the input/scenario.
    pub input: String,
    /// Expected behavior or output.
    pub output: String,
}

impl ToolSpec {
    /// Create a new ToolSpec builder.
    pub fn builder(name: impl Into<String>) -> ToolSpecBuilder {
        ToolSpecBuilder {
            name: name.into(),
            purpose: None,
            when_to_use: None,
            when_not_to_use: None,
            parameters: None,
            examples: Vec::new(),
            output_format: None,
            disambiguation: Vec::new(),
        }
    }

    /// Convert this spec to a rich description string for the LLM.
    pub fn to_description(&self) -> String {
        let mut desc = format!("{}.", self.purpose);
        desc.push_str(&format!("\nWhen to use: {}", self.when_to_use));
        desc.push_str(&format!("\nWhen NOT to use: {}", self.when_not_to_use));

        if !self.examples.is_empty() {
            desc.push_str("\nExamples:");
            for ex in &self.examples {
                desc.push_str(&format!("\n  - Input: {} → {}", ex.input, ex.output));
            }
        }

        if !self.output_format.is_empty() {
            desc.push_str(&format!("\nOutput format: {}", self.output_format));
        }

        if !self.disambiguation.is_empty() {
            desc.push_str("\nDisambiguation:");
            for d in &self.disambiguation {
                desc.push_str(&format!(
                    "\n  - {}: Use '{}' instead — {}",
                    d.scenario, d.correct_tool, d.reason
                ));
            }
        }

        desc
    }

    /// Convert to the standard `ToolDef` used by the API, using the rich
    /// description generated from the structured fields.
    pub fn to_tool_def(&self) -> ToolDef {
        ToolDef::new(
            self.name.clone(),
            self.to_description(),
            self.parameters.clone(),
        )
    }
}

/// Builder for constructing a `ToolSpec`. Panics on `build()` if required
/// fields are missing — this ensures completeness at registration time.
pub struct ToolSpecBuilder {
    name: String,
    purpose: Option<String>,
    when_to_use: Option<String>,
    when_not_to_use: Option<String>,
    parameters: Option<serde_json::Value>,
    examples: Vec<UsageExample>,
    output_format: Option<String>,
    disambiguation: Vec<DisambiguationExample>,
}

impl ToolSpecBuilder {
    pub fn purpose(mut self, purpose: impl Into<String>) -> Self {
        self.purpose = Some(purpose.into());
        self
    }

    pub fn when_to_use(mut self, when: impl Into<String>) -> Self {
        self.when_to_use = Some(when.into());
        self
    }

    pub fn when_not_to_use(mut self, when_not: impl Into<String>) -> Self {
        self.when_not_to_use = Some(when_not.into());
        self
    }

    pub fn parameters(mut self, params: serde_json::Value) -> Self {
        self.parameters = Some(params);
        self
    }

    /// Derive JSON Schema parameters from a type implementing `schemars::JsonSchema`.
    ///
    /// This is the preferred way to define tool parameters — the schema is
    /// generated from the Rust type, ensuring the schema and deserialization
    /// logic can never diverge.
    pub fn parameters_for<T: schemars::JsonSchema>(self) -> Self {
        self.parameters(crate::json_schema_for::<T>())
    }

    pub fn example(mut self, input: impl Into<String>, output: impl Into<String>) -> Self {
        self.examples.push(UsageExample {
            input: input.into(),
            output: output.into(),
        });
        self
    }

    pub fn output_format(mut self, format: impl Into<String>) -> Self {
        self.output_format = Some(format.into());
        self
    }

    pub fn disambiguate(
        mut self,
        scenario: impl Into<String>,
        correct_tool: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        self.disambiguation.push(DisambiguationExample {
            scenario: scenario.into(),
            correct_tool: correct_tool.into(),
            reason: reason.into(),
        });
        self
    }

    /// Shortcut: build the spec and immediately convert to [`ToolDef`].
    ///
    /// Equivalent to `.build().to_tool_def()` but avoids the intermediate
    /// `ToolSpec` when you only need the `ToolDef`.
    pub fn to_tool_def(self) -> crate::ToolDef {
        self.build().to_tool_def()
    }

    /// Build the `ToolSpec`. Panics if required fields are missing.
    pub fn build(self) -> ToolSpec {
        ToolSpec {
            name: self.name,
            purpose: self.purpose.expect("ToolSpec requires 'purpose'"),
            when_to_use: self.when_to_use.expect("ToolSpec requires 'when_to_use'"),
            when_not_to_use: self
                .when_not_to_use
                .expect("ToolSpec requires 'when_not_to_use'"),
            parameters: self.parameters.expect("ToolSpec requires 'parameters'"),
            examples: self.examples,
            output_format: self.output_format.unwrap_or_else(|| "Plain text".into()),
            disambiguation: self.disambiguation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_tool_spec() {
        let spec = ToolSpec::builder("grep")
            .purpose("Search file contents by regex pattern")
            .when_to_use("When you need to find text matching a pattern across multiple files")
            .when_not_to_use("When you already know the file path — use read_file instead")
            .parameters(serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "glob": { "type": "string" }
                },
                "required": ["pattern"]
            }))
            .example(
                r#"grep(pattern="TODO", glob="*.rs")"#,
                "Returns matching lines with file:line_number prefix",
            )
            .output_format("Matching lines with file:line_number prefix")
            .build();

        assert_eq!(spec.name, "grep");
        assert!(spec.to_description().contains("When NOT to use:"));
        assert!(spec.to_description().contains("read_file"));
    }

    #[test]
    fn to_tool_def_conversion() {
        let spec = ToolSpec::builder("test_tool")
            .purpose("A test tool")
            .when_to_use("When testing")
            .when_not_to_use("In production")
            .parameters(serde_json::json!({"type": "object", "properties": {}}))
            .build();

        let def = spec.to_tool_def();
        assert_eq!(def.function.name, "test_tool");
        assert!(def.function.description.contains("A test tool"));
    }

    #[test]
    fn builder_to_tool_def_shortcut() {
        let def = ToolSpec::builder("shortcut_tool")
            .purpose("A tool built via the shortcut")
            .when_to_use("When testing the shortcut")
            .when_not_to_use("Never")
            .parameters(serde_json::json!({"type": "object", "properties": {}}))
            .to_tool_def();

        assert_eq!(def.function.name, "shortcut_tool");
        assert!(
            def.function
                .description
                .contains("A tool built via the shortcut")
        );
        assert!(def.function.description.contains("When NOT to use:"));
    }

    #[test]
    #[should_panic(expected = "ToolSpec requires 'purpose'")]
    fn builder_panics_on_missing_purpose() {
        ToolSpec::builder("incomplete")
            .when_to_use("test")
            .when_not_to_use("test")
            .parameters(serde_json::json!({}))
            .build();
    }
}
