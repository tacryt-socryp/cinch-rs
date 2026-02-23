//! API interaction layer: HTTP client support, streaming, retry, routing, and cost tracking.
//!
//! These modules handle everything between the [`Harness`](crate::agent::harness::Harness)
//! loop and the OpenRouter API:
//!
//! - [`retry`] — transient error detection (429, 5xx, network timeouts) with
//!   configurable exponential backoff and jitter. Never retries 400/401 errors.
//! - [`streaming`] — SSE parser for incremental text, reasoning, and tool-call
//!   deltas. Produces [`StreamEvent`](streaming::StreamEvent) values.
//! - [`router`] — [`RoutingStrategy`] for per-round model selection. Use a
//!   cheap model for early rounds and a powerful model for later rounds.
//! - [`tracing`] — correlation IDs (`trace_id` / `span_id`), per-model pricing
//!   tables, and cumulative [`CostTracker`] for spend monitoring.

pub mod retry;
pub mod router;
pub mod streaming;
pub mod tracing;

// Re-export commonly used items at the module level.
pub use retry::RetryConfig;
pub use router::RoutingStrategy;
pub use tracing::{CostTracker, generate_span_id, generate_trace_id, pricing_for_model};
