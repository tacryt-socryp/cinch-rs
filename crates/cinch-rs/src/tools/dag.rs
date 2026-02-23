//! Dependency-aware tool execution ordering.
//!
//! When the LLM returns multiple tool calls in a single round, they are
//! normally executed in parallel via `join_all`. This module adds support
//! for dependency annotations — a tool call can declare `depends_on` to
//! reference another call's ID in the same round, creating a DAG.
//!
//! Tool calls are grouped into execution waves via topological sort:
//! independent calls run in parallel, dependent calls wait for their
//! prerequisites. If no dependency annotations are present, all calls
//! form a single wave (backward-compatible parallel execution).
//!
//! Alternatively, `HarnessConfig::sequential_tools` forces all tool calls
//! to execute sequentially regardless of annotations — a conservative
//! mode for tool sets with destructive operations.

use std::collections::{HashMap, VecDeque};

/// A tool call with optional dependency annotation.
#[derive(Debug, Clone)]
pub struct AnnotatedToolCall {
    /// The tool call ID.
    pub call_id: String,
    /// Tool name.
    pub name: String,
    /// Arguments JSON string.
    pub arguments: String,
    /// Optional ID of another tool call in this round that must complete first.
    pub depends_on: Option<String>,
}

/// A wave of tool calls that can execute in parallel.
pub type ExecutionWave = Vec<AnnotatedToolCall>;

/// Build execution waves from annotated tool calls via topological sort.
///
/// Returns a sequence of waves. Calls within a wave are independent and
/// can run in parallel. Wave N must complete before wave N+1 begins.
///
/// If the dependency graph contains a cycle, returns an error.
pub fn build_execution_waves(calls: Vec<AnnotatedToolCall>) -> Result<Vec<ExecutionWave>, String> {
    if calls.is_empty() {
        return Ok(vec![]);
    }

    // If no dependencies, everything is one wave.
    if calls.iter().all(|c| c.depends_on.is_none()) {
        return Ok(vec![calls]);
    }

    // Build adjacency list and in-degree map.
    let mut in_degree: HashMap<String, usize> = HashMap::new();
    let mut dependents: HashMap<String, Vec<String>> = HashMap::new();
    let mut call_map: HashMap<String, AnnotatedToolCall> = HashMap::new();

    for call in &calls {
        in_degree.entry(call.call_id.clone()).or_insert(0);
        if let Some(ref dep) = call.depends_on {
            *in_degree.entry(call.call_id.clone()).or_insert(0) += 1;
            dependents
                .entry(dep.clone())
                .or_default()
                .push(call.call_id.clone());
        }
    }

    for call in calls {
        call_map.insert(call.call_id.clone(), call);
    }

    // Kahn's algorithm for topological sort, grouping by wave.
    let mut waves: Vec<ExecutionWave> = Vec::new();
    let mut queue: VecDeque<String> = VecDeque::new();

    // Seed with nodes that have no dependencies.
    for (id, &deg) in &in_degree {
        if deg == 0 {
            queue.push_back(id.clone());
        }
    }

    let mut processed = 0;
    let total = call_map.len();

    while !queue.is_empty() {
        // All items currently in the queue form one wave.
        let wave_size = queue.len();
        let mut wave = Vec::with_capacity(wave_size);

        for _ in 0..wave_size {
            let id = queue.pop_front().unwrap();
            processed += 1;

            // Release dependents.
            if let Some(deps) = dependents.get(&id) {
                for dep_id in deps {
                    if let Some(deg) = in_degree.get_mut(dep_id) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push_back(dep_id.clone());
                        }
                    }
                }
            }

            if let Some(call) = call_map.remove(&id) {
                wave.push(call);
            }
        }

        if !wave.is_empty() {
            waves.push(wave);
        }
    }

    if processed < total {
        return Err(format!(
            "Dependency cycle detected among tool calls: {} of {} calls could not be ordered",
            total - processed,
            total
        ));
    }

    Ok(waves)
}

/// Extract dependency annotations from tool call arguments.
///
/// Looks for a `depends_on` field in the arguments JSON. This is a
/// convention on top of the OpenAI tool-calling format (extra fields
/// are ignored by the API).
pub fn extract_depends_on(arguments: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(arguments).ok()?;
    parsed
        .get("depends_on")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Convert raw tool calls into annotated tool calls by extracting
/// `depends_on` from their arguments.
pub fn annotate_tool_calls(calls: &[crate::ToolCall]) -> Vec<AnnotatedToolCall> {
    calls
        .iter()
        .map(|call| {
            let depends_on = extract_depends_on(&call.function.arguments);
            AnnotatedToolCall {
                call_id: call.id.clone(),
                name: call.function.name.clone(),
                arguments: call.function.arguments.clone(),
                depends_on,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shorthand for building test AnnotatedToolCall.
    fn call(id: &str, dep: Option<&str>) -> AnnotatedToolCall {
        AnnotatedToolCall {
            call_id: id.into(),
            name: format!("tool_{id}"),
            arguments: "{}".into(),
            depends_on: dep.map(Into::into),
        }
    }

    #[test]
    fn no_dependencies_single_wave() {
        let waves = build_execution_waves(vec![call("a", None), call("b", None)]).unwrap();
        assert_eq!(waves.len(), 1);
        assert_eq!(waves[0].len(), 2);
    }

    #[test]
    fn linear_dependency_chain() {
        // a -> b -> c
        let waves = build_execution_waves(vec![
            call("a", None),
            call("b", Some("a")),
            call("c", Some("b")),
        ])
        .unwrap();
        assert_eq!(waves.len(), 3);
        assert_eq!(waves[0][0].call_id, "a");
        assert_eq!(waves[1][0].call_id, "b");
        assert_eq!(waves[2][0].call_id, "c");
    }

    #[test]
    fn diamond_dependency() {
        // a -> b, a -> c, b -> d
        let waves = build_execution_waves(vec![
            call("a", None),
            call("b", Some("a")),
            call("c", Some("a")),
            call("d", Some("b")),
        ])
        .unwrap();
        assert_eq!(waves.len(), 3);
        assert_eq!(waves[0].len(), 1);
        assert_eq!(waves[1].len(), 2);
        assert_eq!(waves[2].len(), 1);
    }

    #[test]
    fn cycle_detection() {
        let result = build_execution_waves(vec![call("a", Some("b")), call("b", Some("a"))]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("cycle"));
    }

    #[test]
    fn empty_calls() {
        let waves = build_execution_waves(vec![]).unwrap();
        assert!(waves.is_empty());
    }

    #[test]
    fn extract_depends_on_present() {
        let args = r#"{"path": "test.rs", "depends_on": "call_abc"}"#;
        assert_eq!(extract_depends_on(args), Some("call_abc".into()));
    }

    #[test]
    fn extract_depends_on_absent() {
        assert_eq!(extract_depends_on(r#"{"path": "test.rs"}"#), None);
    }

    #[test]
    fn extract_depends_on_invalid_json() {
        assert_eq!(extract_depends_on("not json"), None);
    }

    #[test]
    fn annotate_tool_calls_extracts_deps() {
        let calls = vec![
            crate::ToolCall {
                id: "call_1".into(),
                call_type: crate::CallType::Function,
                function: crate::FunctionCallData {
                    name: "read_file".into(),
                    arguments: r#"{"path": "a.rs"}"#.into(),
                },
            },
            crate::ToolCall {
                id: "call_2".into(),
                call_type: crate::CallType::Function,
                function: crate::FunctionCallData {
                    name: "grep".into(),
                    arguments: r#"{"pattern": "fn", "depends_on": "call_1"}"#.into(),
                },
            },
        ];

        let annotated = annotate_tool_calls(&calls);
        assert_eq!(annotated.len(), 2);
        assert!(annotated[0].depends_on.is_none());
        assert_eq!(annotated[1].depends_on.as_deref(), Some("call_1"));
    }

    #[test]
    fn mixed_deps_and_independent() {
        // a (no deps), b depends on a, c (no deps)
        let waves =
            build_execution_waves(vec![call("a", None), call("b", Some("a")), call("c", None)])
                .unwrap();
        assert_eq!(waves.len(), 2);
        assert_eq!(waves[0].len(), 2);
        assert_eq!(waves[1].len(), 1);
        assert_eq!(waves[1][0].call_id, "b");
    }
}
