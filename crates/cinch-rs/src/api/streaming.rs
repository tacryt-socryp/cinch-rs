//! Server-Sent Events (SSE) streaming for the OpenRouter chat completions API.
//!
//! Provides [`StreamEvent`] and the [`OpenRouterClient::chat_stream`] method
//! for receiving incremental text and reasoning deltas from the LLM. This
//! allows the harness (or TUI) to display output as it arrives rather than
//! waiting for the full response.

use crate::{ChatRequest, OPENROUTER_URL, OpenRouterClient, UsageInfo};
use serde::Deserialize;
use tracing::{debug, trace, warn};

/// A single event from an SSE stream.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// An incremental text content delta.
    TextDelta(String),
    /// An incremental reasoning/thinking delta.
    ReasoningDelta(String),
    /// A tool call chunk (accumulated until complete).
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: String,
    },
    /// Token usage information (sent in the final chunk).
    Usage(UsageInfo),
    /// The stream is complete.
    Done,
    /// An error occurred during streaming.
    Error(String),
}

/// Raw SSE data chunk from the OpenRouter API.
#[derive(Deserialize, Debug)]
struct StreamChunk {
    choices: Option<Vec<StreamChoice>>,
    usage: Option<UsageInfo>,
}

#[derive(Deserialize, Debug)]
struct StreamChoice {
    delta: Option<StreamDelta>,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Debug)]
struct StreamDelta {
    content: Option<String>,
    reasoning: Option<String>,
    tool_calls: Option<Vec<StreamToolCallDelta>>,
}

#[derive(Deserialize, Debug)]
struct StreamToolCallDelta {
    index: Option<usize>,
    id: Option<String>,
    function: Option<StreamFunctionDelta>,
}

#[derive(Deserialize, Debug)]
struct StreamFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

impl OpenRouterClient {
    /// Send a chat completion request with SSE streaming.
    ///
    /// Returns a `Vec<StreamEvent>` representing the full stream. In a future
    /// iteration, this will return an async `Stream` for true incremental
    /// processing; for now, events are collected and returned as a batch
    /// so the harness can process them.
    ///
    /// This is the foundation for incremental TUI updates and mid-stream
    /// cancellation.
    pub async fn chat_stream(&self, body: &ChatRequest) -> Result<Vec<StreamEvent>, String> {
        let mut stream_body =
            serde_json::to_value(body).map_err(|e| format!("failed to serialize request: {e}"))?;
        stream_body["stream"] = serde_json::Value::Bool(true);

        debug!("Sending streaming chat request");

        let mut resp = self
            .client
            .post(OPENROUTER_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("HTTP-Referer", &self.referer)
            .header("X-Title", &self.title)
            .json(&stream_body)
            .send()
            .await
            .map_err(|e| format!("streaming request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("OpenRouter API HTTP {status}: {text}"));
        }

        // Read the SSE stream incrementally via chunk() so long responses
        // (e.g. file-write tool calls) don't hit a single-body timeout.
        let mut events = Vec::new();
        let mut buffer = String::new();

        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| format!("failed to read streaming chunk: {e}"))?
        {
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            // Process all complete lines in the buffer.
            while let Some(newline_pos) = buffer.find('\n') {
                let line: String = buffer.drain(..=newline_pos).collect();
                let line = line.trim();
                if line.is_empty() || line.starts_with(':') {
                    continue;
                }
                if line == "data: [DONE]" {
                    events.push(StreamEvent::Done);
                    break;
                }
                if let Some(data) = line.strip_prefix("data: ") {
                    parse_sse_data(data, &mut events);
                }
            }

            // If we already received Done, stop reading.
            if events.iter().any(|e| matches!(e, StreamEvent::Done)) {
                break;
            }
        }

        // Process any remaining data in the buffer (incomplete final line).
        let remaining = buffer.trim();
        if !remaining.is_empty()
            && remaining != "data: [DONE]"
            && let Some(data) = remaining.strip_prefix("data: ")
        {
            parse_sse_data(data, &mut events);
        }

        // Ensure Done event at the end.
        if !events.iter().any(|e| matches!(e, StreamEvent::Done)) {
            events.push(StreamEvent::Done);
        }

        debug!("Stream completed with {} events", events.len());
        Ok(events)
    }

    /// Send a streaming chat request, invoking `on_event` for each event as
    /// it arrives off the wire.
    ///
    /// This gives the caller (e.g. the TUI) real-time visibility into text
    /// and reasoning deltas while tool-call argument fragments are still
    /// being received. The full event list is also returned for post-hoc
    /// assembly of tool calls, usage, etc.
    pub async fn chat_stream_live(
        &self,
        body: &ChatRequest,
        mut on_event: impl FnMut(&StreamEvent),
    ) -> Result<Vec<StreamEvent>, String> {
        let mut stream_body =
            serde_json::to_value(body).map_err(|e| format!("failed to serialize request: {e}"))?;
        stream_body["stream"] = serde_json::Value::Bool(true);

        debug!("Sending live streaming chat request");

        let mut resp = self
            .client
            .post(OPENROUTER_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("HTTP-Referer", &self.referer)
            .header("X-Title", &self.title)
            .json(&stream_body)
            .send()
            .await
            .map_err(|e| format!("streaming request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("OpenRouter API HTTP {status}: {text}"));
        }

        let mut events = Vec::new();
        let mut buffer = String::new();
        let mut done = false;

        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| format!("failed to read streaming chunk: {e}"))?
        {
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(newline_pos) = buffer.find('\n') {
                let line: String = buffer.drain(..=newline_pos).collect();
                let line = line.trim();
                if line.is_empty() || line.starts_with(':') {
                    continue;
                }
                if line == "data: [DONE]" {
                    let ev = StreamEvent::Done;
                    on_event(&ev);
                    events.push(ev);
                    done = true;
                    break;
                }
                if let Some(data) = line.strip_prefix("data: ") {
                    let before = events.len();
                    parse_sse_data(data, &mut events);
                    // Emit newly parsed events to the callback.
                    for ev in &events[before..] {
                        on_event(ev);
                    }
                }
            }

            if done {
                break;
            }
        }

        // Process any remaining data in the buffer.
        let remaining = buffer.trim();
        if !remaining.is_empty()
            && remaining != "data: [DONE]"
            && let Some(data) = remaining.strip_prefix("data: ")
        {
            let before = events.len();
            parse_sse_data(data, &mut events);
            for ev in &events[before..] {
                on_event(ev);
            }
        }

        if !events.iter().any(|e| matches!(e, StreamEvent::Done)) {
            let ev = StreamEvent::Done;
            on_event(&ev);
            events.push(ev);
        }

        debug!("Live stream completed with {} events", events.len());
        Ok(events)
    }
}

/// Parse a single SSE `data:` payload into stream events.
fn parse_sse_data(data: &str, events: &mut Vec<StreamEvent>) {
    match serde_json::from_str::<StreamChunk>(data) {
        Ok(chunk) => {
            // Emit usage if present.
            if let Some(usage) = chunk.usage {
                events.push(StreamEvent::Usage(usage));
            }

            // Process choices.
            if let Some(choices) = chunk.choices {
                for choice in choices {
                    if let Some(delta) = choice.delta {
                        // Text content delta.
                        if let Some(content) = delta.content
                            && !content.is_empty()
                        {
                            events.push(StreamEvent::TextDelta(content));
                        }
                        // Reasoning delta.
                        if let Some(reasoning) = delta.reasoning
                            && !reasoning.is_empty()
                        {
                            events.push(StreamEvent::ReasoningDelta(reasoning));
                        }
                        // Tool call deltas.
                        if let Some(tool_calls) = delta.tool_calls {
                            for tc in tool_calls {
                                let func = tc.function.unwrap_or(StreamFunctionDelta {
                                    name: None,
                                    arguments: None,
                                });
                                events.push(StreamEvent::ToolCallDelta {
                                    index: tc.index.unwrap_or(0),
                                    id: tc.id,
                                    name: func.name,
                                    arguments_delta: func.arguments.unwrap_or_default(),
                                });
                            }
                        }
                    }
                    // If finish_reason is set, mark as done.
                    if choice.finish_reason.is_some() {
                        trace!("Stream finish_reason: {:?}", choice.finish_reason);
                    }
                }
            }
        }
        Err(e) => {
            warn!("Failed to parse SSE chunk: {e} — data: {data}");
        }
    }
}

/// Assemble a complete text string from a sequence of stream events.
pub fn collect_text(events: &[StreamEvent]) -> String {
    let mut text = String::new();
    for event in events {
        if let StreamEvent::TextDelta(delta) = event {
            text.push_str(delta);
        }
    }
    text
}

/// Assemble complete reasoning from a sequence of stream events.
pub fn collect_reasoning(events: &[StreamEvent]) -> String {
    let mut reasoning = String::new();
    for event in events {
        if let StreamEvent::ReasoningDelta(delta) = event {
            reasoning.push_str(delta);
        }
    }
    reasoning
}

/// Extract usage info from stream events (if present).
pub fn extract_usage(events: &[StreamEvent]) -> Option<UsageInfo> {
    for event in events.iter().rev() {
        if let StreamEvent::Usage(usage) = event {
            return Some(usage.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_text_from_deltas() {
        let events = vec![
            StreamEvent::TextDelta("Hello ".into()),
            StreamEvent::TextDelta("world!".into()),
            StreamEvent::Done,
        ];
        assert_eq!(collect_text(&events), "Hello world!");
    }

    #[test]
    fn collect_reasoning_from_deltas() {
        let events = vec![
            StreamEvent::ReasoningDelta("Let me think...".into()),
            StreamEvent::ReasoningDelta(" Okay.".into()),
            StreamEvent::Done,
        ];
        assert_eq!(collect_reasoning(&events), "Let me think... Okay.");
    }

    #[test]
    fn extract_usage_from_events() {
        let events = vec![
            StreamEvent::TextDelta("hi".into()),
            StreamEvent::Usage(UsageInfo {
                prompt_tokens: Some(100),
                completion_tokens: Some(50),
                total_tokens: Some(150),
            }),
            StreamEvent::Done,
        ];
        let usage = extract_usage(&events).unwrap();
        assert_eq!(usage.prompt_tokens, Some(100));
    }

    #[test]
    fn extract_usage_returns_none_when_missing() {
        let events = vec![StreamEvent::TextDelta("hi".into()), StreamEvent::Done];
        assert!(extract_usage(&events).is_none());
    }
}
