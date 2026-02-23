//! Extension trait for domain-specific web UI rendering.
//!
//! Domain crates implement [`WebExtensionRenderer`] to provide structured data
//! that the Next.js frontend can render. Unlike the TUI renderer (which returns
//! ratatui `Span`s), this returns JSON-serializable data.

use cinch_rs::ui::{QuestionChoice, UiExtension};
use serde::Serialize;

/// Trait for domain-specific web UI rendering.
///
/// Domain crates implement this to provide structured data that the
/// Next.js frontend can render as status badges, extension panels,
/// and choice metadata.
///
/// # Example
///
/// ```ignore
/// struct MyWebRenderer;
///
/// impl WebExtensionRenderer for MyWebRenderer {
///     fn status_fields(&self, ext: &dyn UiExtension) -> Vec<StatusField> {
///         vec![StatusField {
///             label: "Count".into(),
///             value: "42".into(),
///             variant: "info".into(),
///         }]
///     }
/// }
/// ```
pub trait WebExtensionRenderer: Send + Sync {
    /// Extra status fields to display in the status bar.
    ///
    /// Returns key-value pairs rendered as badges in the web UI.
    fn status_fields(&self, ext: &dyn UiExtension) -> Vec<StatusField> {
        let _ = ext;
        vec![]
    }

    /// Serialize the current extension state for WebSocket broadcast.
    ///
    /// Called after relevant events to push domain-specific state to clients.
    fn to_ws_json(&self, ext: &dyn UiExtension) -> Option<serde_json::Value> {
        let _ = ext;
        None
    }

    /// Optional metadata to display alongside each question choice.
    ///
    /// For example, a tweet agent might return character count and a color
    /// variant indicating whether the tweet is within the 280-char limit.
    fn choice_metadata(
        &self,
        ext: &dyn UiExtension,
        choice: &QuestionChoice,
    ) -> Option<ChoiceMetadata> {
        let _ = (ext, choice);
        None
    }
}

/// A key-value status field displayed as a badge in the web UI status bar.
#[derive(Clone, Debug, Serialize)]
pub struct StatusField {
    /// Short label (e.g., "Drafted").
    pub label: String,
    /// Display value (e.g., "3").
    pub value: String,
    /// CSS class variant for styling (e.g., "info", "success", "warning", "error").
    pub variant: String,
}

/// Metadata displayed alongside a question choice in the web UI.
#[derive(Clone, Debug, Serialize)]
pub struct ChoiceMetadata {
    /// Display text (e.g., "142 chars").
    pub text: String,
    /// CSS class variant for styling.
    pub variant: String,
}

/// No-op renderer for agents with no domain-specific web UI.
pub struct NoWebExtension;

impl WebExtensionRenderer for NoWebExtension {}
