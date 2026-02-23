//! Extension trait for domain-specific UI state.
//!
//! Agents attach custom state (e.g. tweet counts, diff-check status) to
//! [`UiState`](super::UiState) via this trait, without cinch-rs needing
//! to know the concrete types.

use std::any::Any;

/// Allows domain crates to attach custom state to [`UiState`](super::UiState)
/// without cinch-rs knowing the concrete types.
///
/// Implementations must be `Send + Sync + Any` so the extension can live inside
/// the `Arc<Mutex<UiState>>` and be downcast by domain-aware code.
///
/// # Example
///
/// ```
/// use cinch_rs::ui::UiExtension;
///
/// struct MyExtension {
///     pub draft_count: u32,
/// }
///
/// impl UiExtension for MyExtension {
///     fn as_any(&self) -> &dyn std::any::Any { self }
///     fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
/// }
/// ```
pub trait UiExtension: Send + Sync + Any {
    /// Downcast to a concrete type for reading.
    fn as_any(&self) -> &dyn Any;
    /// Downcast to a concrete type for writing.
    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// Serialize the extension state for transport (e.g., over WebSocket).
    ///
    /// Returns `None` by default. Domain crates that want their extension
    /// state visible in a web UI should override this to return a JSON
    /// representation of their state.
    fn to_json(&self) -> Option<serde_json::Value> {
        None
    }
}

/// Default no-op extension for agents that don't need custom UI state.
pub struct NoExtension;

impl UiExtension for NoExtension {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
