//! Extension trait for domain-specific TUI rendering.
//!
//! Domain crates implement [`TuiExtensionRenderer`] to inject custom status
//! bar spans and question choice decorations without forking the generic TUI.

use cinch_rs::ui::{QuestionChoice, UiExtension};
use ratatui::text::Span;

/// Trait for domain-specific TUI rendering additions.
///
/// All methods have default no-op implementations, so agents without custom
/// UI needs can use [`NoTuiExtension`] directly.
pub trait TuiExtensionRenderer: Send + Sync {
    /// Extra spans to append to the status bar's primary line
    /// (e.g., "drafted: 3 | posted: 1").
    fn status_spans(&self, ext: &dyn UiExtension) -> Vec<Span<'_>> {
        let _ = ext;
        vec![]
    }

    /// Extra spans for the status bar's secondary line
    /// (e.g., diff-check status).
    fn status_secondary_spans(&self, ext: &dyn UiExtension) -> Vec<Span<'_>> {
        let _ = ext;
        vec![]
    }

    /// Optional metadata line rendered below each question choice
    /// (e.g., character count with color-coded warning).
    fn choice_decoration(
        &self,
        ext: &dyn UiExtension,
        choice: &QuestionChoice,
    ) -> Option<Span<'_>> {
        let _ = (ext, choice);
        None
    }
}

/// No-op renderer for agents with no domain-specific UI.
pub struct NoTuiExtension;

impl TuiExtensionRenderer for NoTuiExtension {}
