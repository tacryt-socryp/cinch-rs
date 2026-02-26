//! Generic tracing subscriber layer that captures log events into a
//! [`LogBuffer`] for later draining into [`UiState`](super::UiState).
//!
//! This layer is UI-agnostic — it writes [`LogLine`] entries into a lock-free
//! buffer that any frontend can drain at its own pace.  The buffer uses a
//! **separate** mutex from `UiState`, so logging never blocks rendering and
//! vice-versa.

use std::sync::{Arc, Mutex};

use chrono::Local;
use tracing::Subscriber;
use tracing_subscriber::layer::Layer;
use tracing_subscriber::registry::LookupSpan;

use super::{LOG_TRIM_TO, LogLevel, LogLine, MAX_LOG_LINES};

/// A shared buffer of pending log lines.
///
/// The tracing layer pushes into this buffer; the UI frontend drains it once
/// per frame and merges the entries into `UiState::logs`.  Because the
/// buffer has its own mutex, `on_event` never contends with the render
/// thread's `UiState` lock.
#[derive(Clone)]
pub struct LogBuffer(Arc<Mutex<Vec<LogLine>>>);

impl LogBuffer {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::with_capacity(128))))
    }

    /// Drain all pending log lines from the buffer, returning them.
    ///
    /// Call this from the UI frontend once per frame to merge logs into
    /// `UiState::logs`.
    pub fn drain(&self) -> Vec<LogLine> {
        let mut buf = self.0.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *buf)
    }

    /// Drain pending log lines directly into `UiState::logs`, respecting
    /// the configured trim limits.
    ///
    /// Acquires the `UiState` lock only if there are new log lines.
    pub fn flush_into(&self, state: &Arc<Mutex<super::UiState>>) {
        let lines = self.drain();
        if lines.is_empty() {
            return;
        }
        if let Ok(mut s) = state.lock() {
            s.logs.extend(lines);
            if s.logs.len() > MAX_LOG_LINES {
                let trim_to = s.logs.len() - LOG_TRIM_TO;
                s.logs.drain(..trim_to);
            }
        }
    }
}

/// A [`tracing_subscriber::Layer`] that captures log events into
/// a [`LogBuffer`] so they can be rendered by any UI frontend.
pub struct UiTracingLayer {
    buffer: LogBuffer,
}

impl UiTracingLayer {
    /// Create a new tracing layer and its associated [`LogBuffer`].
    ///
    /// Pass the `LogBuffer` to your UI frontend so it can drain pending
    /// log lines each frame.
    pub fn new() -> (Self, LogBuffer) {
        let buffer = LogBuffer::new();
        (
            Self {
                buffer: buffer.clone(),
            },
            buffer,
        )
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for UiTracingLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        let level = match *event.metadata().level() {
            tracing::Level::TRACE => LogLevel::Trace,
            tracing::Level::DEBUG => LogLevel::Debug,
            tracing::Level::INFO => LogLevel::Info,
            tracing::Level::WARN => LogLevel::Warn,
            tracing::Level::ERROR => LogLevel::Error,
        };

        let mut message = visitor.message;
        if !visitor.fields.is_empty() {
            let extras: Vec<String> = visitor
                .fields
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            if message.is_empty() {
                message = extras.join(" ");
            } else {
                message = format!("{message} {{{}}}", extras.join(", "));
            }
        }

        let line = LogLine {
            time: Local::now().format("%H:%M:%S").to_string(),
            level,
            message,
        };

        // Lock only the log buffer — never the UiState — so log calls
        // from tokio workers can never block on the TUI render thread.
        if let Ok(mut buf) = self.buffer.0.lock() {
            buf.push(line);
            // Cap the buffer so a burst of logs before the next drain
            // doesn't consume unbounded memory.
            if buf.len() > MAX_LOG_LINES {
                let trim_to = buf.len() - LOG_TRIM_TO;
                buf.drain(..trim_to);
            }
        }
    }
}

/// Visitor that extracts the message and extra fields from a tracing event.
#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: Vec<(String, String)>,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let raw = format!("{value:?}");
            // Strip surrounding quotes from debug-formatted strings.
            if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
                #[allow(clippy::string_slice)] // stripping 1-byte ASCII quote chars
                {
                    self.message = raw[1..raw.len() - 1].to_string();
                }
            } else {
                self.message = raw;
            }
        } else {
            self.fields
                .push((field.name().to_string(), format!("{value:?}")));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }
    }
}
