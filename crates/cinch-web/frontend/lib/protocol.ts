/**
 * WebSocket wire protocol — server → client messages.
 *
 * Each message is a JSON object with a "type" discriminator matching
 * the Rust WsMessage enum (serde tag = "type", rename_all = "snake_case").
 */

import type {
  AgentState,
  LogLine,
  UiStateSnapshot,
  UserQuestion,
} from "./types";

// ── Server → Client messages ──────────────────────────────────────────

export type WsServerMessage =
  | { type: "snapshot"; data: UiStateSnapshot }
  | { type: "text"; text: string }
  | { type: "text_delta"; delta: string }
  | { type: "tool_executing"; name: string; arguments: string }
  | { type: "tool_result"; name: string; result: string; is_error: boolean }
  | { type: "reasoning"; text: string }
  | { type: "reasoning_delta"; delta: string }
  | {
      type: "round";
      round: number;
      max_rounds: number;
      context_pct: number;
    }
  | { type: "phase"; phase: string }
  | { type: "question"; question: UserQuestion }
  | { type: "question_dismissed" }
  | { type: "finished" }
  | { type: "log"; line: LogLine }
  | { type: "extension"; data: Record<string, unknown> }
  | { type: "user_message"; message: string }
  | { type: "token_usage"; prompt_tokens: number; completion_tokens: number }
  | { type: "tool_calls_received"; round: number; count: number }
  | { type: "tool_cache_hit"; name: string; arguments: string }
  | { type: "eviction"; freed_chars: number; evicted_count: number }
  | { type: "compaction"; compaction_number: number }
  | { type: "model_routed"; model: string; round: number }
  | { type: "checkpoint_saved"; round: number; path: string }
  | { type: "checkpoint_resumed"; round: number }
  | { type: "empty_response"; round: number; attempt: number; max_retries: number }
  | { type: "approval_required"; name: string; arguments: string }
  | { type: "todo_update"; content: string };

// ── State reducer ─────────────────────────────────────────────────────

/** Apply a server message to the current agent state, returning a new state. */
export function applyMessage(
  prev: AgentState,
  msg: WsServerMessage,
): AgentState {
  switch (msg.type) {
    case "snapshot": {
      const s = msg.data;
      return {
        phase: s.phase,
        round: s.round,
        maxRounds: s.max_rounds,
        contextPct: s.context_pct,
        model: s.model,
        cycle: s.cycle,
        entries: s.agent_output,
        streamingBuffer: s.streaming_buffer,
        reasoningBuffer: "",
        logs: s.logs,
        running: s.running,
        nextCycleSecs: s.next_cycle_secs,
        activeQuestion: s.active_question,
        extension: s.extension,
        totalPromptTokens: prev.totalPromptTokens,
        totalCompletionTokens: prev.totalCompletionTokens,
      };
    }

    case "text":
      return {
        ...prev,
        entries: [...prev.entries, { Text: msg.text }],
        streamingBuffer: "", // Complete text replaces the buffer.
      };

    case "text_delta":
      return {
        ...prev,
        streamingBuffer: prev.streamingBuffer + msg.delta,
      };

    case "tool_executing":
      return {
        ...prev,
        phase: `Tool: ${msg.name}`,
        entries: [
          ...prev.entries,
          { ToolExecuting: { name: msg.name, arguments: msg.arguments } },
        ],
      };

    case "tool_result":
      return {
        ...prev,
        entries: [
          ...prev.entries,
          {
            ToolResult: {
              name: msg.name,
              result: msg.result,
              is_error: msg.is_error,
            },
          },
        ],
      };

    case "reasoning":
      return {
        ...prev,
        entries: [
          ...prev.entries,
          { ToolExecuting: { name: "think", arguments: JSON.stringify({ reasoning: msg.text }) } },
          { ToolResult: { name: "think", result: "ok", is_error: false } },
        ],
        reasoningBuffer: "", // Complete reasoning replaces the buffer.
      };

    case "reasoning_delta":
      return {
        ...prev,
        reasoningBuffer: prev.reasoningBuffer + msg.delta,
      };

    case "round":
      return {
        ...prev,
        round: msg.round,
        maxRounds: msg.max_rounds,
        contextPct: msg.context_pct,
      };

    case "phase":
      return { ...prev, phase: msg.phase };

    case "question":
      return {
        ...prev,
        activeQuestion: {
          question: msg.question,
          remaining_secs: null,
          done: false,
        },
      };

    case "question_dismissed":
      return { ...prev, activeQuestion: null };

    case "finished":
      return { ...prev, running: false, phase: "Finished" };

    case "log":
      return { ...prev, logs: [...prev.logs, msg.line] };

    case "extension":
      return { ...prev, extension: msg.data };

    case "user_message":
      return {
        ...prev,
        entries: [...prev.entries, { UserMessage: msg.message }],
      };

    case "token_usage":
      return {
        ...prev,
        totalPromptTokens: prev.totalPromptTokens + msg.prompt_tokens,
        totalCompletionTokens: prev.totalCompletionTokens + msg.completion_tokens,
      };

    case "tool_calls_received":
      return {
        ...prev,
        phase: `${msg.count} tool call(s)`,
      };

    case "tool_cache_hit":
      return {
        ...prev,
        entries: [
          ...prev.entries,
          { ToolExecuting: { name: `${msg.name} (cached)`, arguments: msg.arguments } },
        ],
      };

    case "model_routed":
      return { ...prev, model: msg.model };

    case "eviction":
      return {
        ...prev,
        logs: [
          ...prev.logs,
          {
            time: new Date().toLocaleTimeString(),
            level: "Info",
            message: `Context eviction: freed ${msg.freed_chars} chars from ${msg.evicted_count} tool result(s)`,
          },
        ],
      };

    case "compaction":
      return {
        ...prev,
        logs: [
          ...prev.logs,
          {
            time: new Date().toLocaleTimeString(),
            level: "Info",
            message: `Context compaction #${msg.compaction_number} completed`,
          },
        ],
      };

    case "checkpoint_saved":
      return {
        ...prev,
        logs: [
          ...prev.logs,
          {
            time: new Date().toLocaleTimeString(),
            level: "Debug",
            message: `Checkpoint saved at round ${msg.round}: ${msg.path}`,
          },
        ],
      };

    case "checkpoint_resumed":
      return {
        ...prev,
        logs: [
          ...prev.logs,
          {
            time: new Date().toLocaleTimeString(),
            level: "Info",
            message: `Resumed from checkpoint at round ${msg.round}`,
          },
        ],
      };

    case "empty_response":
      return {
        ...prev,
        phase: `Retrying (${msg.attempt}/${msg.max_retries})`,
        logs: [
          ...prev.logs,
          {
            time: new Date().toLocaleTimeString(),
            level: "Warn",
            message: `Empty API response at round ${msg.round}, retrying (${msg.attempt}/${msg.max_retries})`,
          },
        ],
      };

    case "approval_required":
      return {
        ...prev,
        logs: [
          ...prev.logs,
          {
            time: new Date().toLocaleTimeString(),
            level: "Info",
            message: `Approval required for tool: ${msg.name}`,
          },
        ],
      };

    case "todo_update": {
      // Replace existing TodoUpdate entry in-place, or append a new one.
      const idx = prev.entries.findIndex((e) => "TodoUpdate" in e);
      if (idx >= 0) {
        const updated = [...prev.entries];
        updated[idx] = { TodoUpdate: msg.content };
        return { ...prev, entries: updated };
      }
      return {
        ...prev,
        entries: [...prev.entries, { TodoUpdate: msg.content }],
      };
    }

    default:
      return prev;
  }
}
