// ── Wire types matching the Rust cinch-web backend ──────────────────

/** Mirrors cinch_rs::ui::AgentEntry */
export type AgentEntry =
  | { Text: string }
  | { ToolExecuting: { name: string; arguments: string } }
  | { ToolResult: { name: string; result: string; is_error: boolean } }
  | { UserMessage: string }
  | { TodoUpdate: string };

/** Mirrors cinch_rs::ui::LogLevel */
export type LogLevel = "Trace" | "Debug" | "Info" | "Warn" | "Error";

/** Mirrors cinch_rs::ui::LogLine */
export interface LogLine {
  time: string;
  level: LogLevel;
  message: string;
}

/** Mirrors cinch_rs::ui::QuestionChoice */
export interface QuestionChoice {
  label: string;
  body: string;
  metadata: string;
}

/** Mirrors cinch_rs::ui::UserQuestion */
export interface UserQuestion {
  prompt: string;
  choices: QuestionChoice[];
  editable: boolean;
  max_edit_length: number | null;
}

/** Mirrors cinch_rs::ui::QuestionResponse */
export type QuestionResponse =
  | { Selected: number }
  | { SelectedEdited: { index: number; edited_text: string } }
  | "Skipped"
  | "TimedOut";

/** Mirrors cinch_web::snapshot::ActiveQuestionSnapshot */
export interface ActiveQuestionSnapshot {
  question: UserQuestion;
  remaining_secs: number | null;
  done: boolean;
}

/** Mirrors cinch_web::ext::StatusField */
export interface StatusField {
  label: string;
  value: string;
  variant: string;
}

/** Mirrors cinch_web::snapshot::UiStateSnapshot */
export interface UiStateSnapshot {
  phase: string;
  round: number;
  max_rounds: number;
  context_pct: number;
  model: string;
  cycle: number;
  agent_output: AgentEntry[];
  streaming_buffer: string;
  logs: LogLine[];
  running: boolean;
  next_cycle_secs: number | null;
  active_question: ActiveQuestionSnapshot | null;
  extension: Record<string, unknown> | null;
}

// ── Client-side state ─────────────────────────────────────────────────

/** Flattened client state derived from snapshot + incremental updates. */
export interface AgentState {
  phase: string;
  round: number;
  maxRounds: number;
  contextPct: number;
  model: string;
  cycle: number;
  entries: AgentEntry[];
  streamingBuffer: string;
  reasoningBuffer: string;
  logs: LogLine[];
  running: boolean;
  nextCycleSecs: number | null;
  activeQuestion: ActiveQuestionSnapshot | null;
  extension: Record<string, unknown> | null;
  totalPromptTokens: number;
  totalCompletionTokens: number;
}

export const INITIAL_STATE: AgentState = {
  phase: "Connecting...",
  round: 0,
  maxRounds: 0,
  contextPct: 0,
  model: "",
  cycle: 0,
  entries: [],
  streamingBuffer: "",
  reasoningBuffer: "",
  logs: [],
  running: true,
  nextCycleSecs: null,
  activeQuestion: null,
  extension: null,
  totalPromptTokens: 0,
  totalCompletionTokens: 0,
};
