//! Default file-based memory instructions for agent system prompts.
//!
//! Research shows that giving agents filesystem tools and clear instructions
//! on how to use files as memory outperforms custom memory modules (Letta
//! benchmark, 2025; mem-agent, 2025). This module provides a default prompt
//! section that teaches agents to use a `memory/` directory for:
//!
//! - **Scratchpad**: working memory within a task
//! - **Learnings**: persistent cross-session observations
//! - **Source tracking**: what reference material has been used recently
//!
//! The prompt is injected into the system message by the [`Harness`] when
//! `HarnessConfig::memory_prompt` is `Some`. Agents override by providing
//! their own string; the default is available via [`default_memory_prompt()`].

/// Returns the default file-based memory prompt for injection into system messages.
///
/// This teaches the agent to use the filesystem as its primary memory system:
/// - `memory/learnings.md` for persistent cross-session learnings
/// - `memory/scratchpad.md` for within-task working memory
/// - Other `memory/*.md` files for topic-specific notes
///
/// The agent is expected to already have filesystem tools (read_file, shell,
/// list_files, or equivalent) in its tool set.
pub fn default_memory_prompt() -> String {
    r#"## File-Based Memory

You have a persistent memory system built on the filesystem. Use it to accumulate
knowledge across sessions and to organize your thinking within each task.

### Memory Directory: `memory/`

This directory is your persistent workspace. Files here survive across sessions.

**At the START of every session**, read your learnings file:
```
read_file("memory/learnings.md")
```

**At the END of every session**, update your learnings with new observations.
Use `shell` to append insights. Examples of what to record:

- What worked well and what didn't
- Sources or areas you explored that had rich material
- Patterns you noticed (in data, outputs, or tool results)
- Calibrations: what approaches worked in which contexts
- Sources you've already drawn from recently (to avoid repetition)
- Experiments: what you tried differently and why

### File Conventions

| File | Purpose | Persistence |
|------|---------|-------------|
| `memory/learnings.md` | Cross-session observations & lessons | Permanent — append-only |
| `memory/scratchpad.md` | Working notes for the current task | Overwrite each session |
| `memory/sources-used.md` | Track which sources you've drawn from | Update each session |
| `memory/*.md` | Any other structured notes you find useful | As needed |

### Learnings Format

When appending to `learnings.md`, use this structure:

```
## YYYY-MM-DD — Session N

- **Observation**: [what you noticed]
- **Insight**: [what you learned from it]
- **Action**: [what to do differently next time]
```

### Scratchpad Usage

Use `scratchpad.md` as working memory during a task. Write down:
- Your plan for the current task
- Interesting findings while exploring
- Draft ideas before finalizing
- Notes or alternatives worth tracking

Overwrite the scratchpad freely — it's scratch space, not permanent record.

### Important

- **Always read `learnings.md` early** in your workflow. Past-you left notes
  for future-you. Use them.
- **Always update `learnings.md` late** in your workflow, after you've made
  decisions and seen results.
- Keep learnings **concise and actionable**. Don't dump raw data — distill it
  into decisions. Remove learnings that are no longer relevant.
- If `learnings.md` grows beyond ~200 lines, consolidate: merge redundant
  entries, remove outdated observations, keep only what actively guides decisions.
"#
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_prompt_is_nonempty() {
        let prompt = default_memory_prompt();
        assert!(!prompt.is_empty());
        assert!(prompt.contains("memory/learnings.md"));
        assert!(prompt.contains("memory/scratchpad.md"));
    }

    #[test]
    fn default_prompt_contains_key_sections() {
        let prompt = default_memory_prompt();
        assert!(prompt.contains("## File-Based Memory"));
        assert!(prompt.contains("### Memory Directory"));
        assert!(prompt.contains("### Learnings Format"));
        assert!(prompt.contains("### Scratchpad Usage"));
    }
}
