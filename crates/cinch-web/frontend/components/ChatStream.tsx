"use client";

import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useAgentState } from "@/hooks/useAgentState";
import { StreamingText } from "./StreamingText";
import { ToolCall } from "./ToolCall";
import { Markdown } from "./Markdown";
import type { AgentEntry } from "@/lib/types";

// ── Pre-resolved entry types (decouples ToolCall look-ahead from render) ──

type ResolvedEntry =
  | { type: "text"; text: string }
  | { type: "user"; message: string }
  | {
      type: "tool";
      name: string;
      arguments: string;
      result: string | undefined;
      isError: boolean | undefined;
    }
  | {
      type: "orphan_result";
      name: string;
      result: string;
      isError: boolean;
    }
  | { type: "todo"; content: string };

/**
 * Resolve raw AgentEntry[] into a flat list ready for rendering.
 *
 * Pairs ToolExecuting + ToolResult entries so each memoized component
 * receives only simple props (no array reference needed).
 */
function resolveEntries(entries: AgentEntry[]): ResolvedEntry[] {
  const resolved: ResolvedEntry[] = [];
  for (let i = 0; i < entries.length; i++) {
    const entry = entries[i]!;

    if ("UserMessage" in entry) {
      resolved.push({ type: "user", message: entry.UserMessage });
    } else if ("Text" in entry) {
      resolved.push({ type: "text", text: entry.Text });
    } else if ("ToolExecuting" in entry) {
      const next = entries[i + 1];
      let result: string | undefined;
      let isError: boolean | undefined;
      if (
        next &&
        "ToolResult" in next &&
        next.ToolResult.name === entry.ToolExecuting.name
      ) {
        result = next.ToolResult.result;
        isError = next.ToolResult.is_error;
      }
      resolved.push({
        type: "tool",
        name: entry.ToolExecuting.name,
        arguments: entry.ToolExecuting.arguments,
        result,
        isError,
      });
    } else if ("ToolResult" in entry) {
      // Skip if preceded by matching ToolExecuting (already paired above).
      const prev = entries[i - 1];
      if (
        prev &&
        "ToolExecuting" in prev &&
        prev.ToolExecuting.name === entry.ToolResult.name
      ) {
        continue;
      }
      // Orphaned result (no preceding ToolExecuting).
      resolved.push({
        type: "orphan_result",
        name: entry.ToolResult.name,
        result: entry.ToolResult.result,
        isError: entry.ToolResult.is_error,
      });
    } else if ("TodoUpdate" in entry) {
      resolved.push({ type: "todo", content: entry.TodoUpdate });
    }
  }
  return resolved;
}

// ── Memoized entry renderers ──────────────────────────────────────────

const TextEntry = memo(function TextEntry({ text }: { text: string }) {
  return (
    <div className="px-4 py-2 text-[var(--text-primary)] message-appear">
      <Markdown content={text} />
    </div>
  );
});

const UserMessageEntry = memo(function UserMessageEntry({
  message,
}: {
  message: string;
}) {
  return (
    <div className="flex justify-end px-4 py-1.5 message-appear">
      <div
        className="max-w-[80%] px-4 py-2.5 text-white whitespace-pre-wrap text-sm"
        style={{
          background: "linear-gradient(135deg, var(--accent), oklch(from var(--accent) l c calc(h + 20)))",
          borderRadius: "18px 18px 4px 18px",
          boxShadow: "0 1px 3px var(--shadow-msg)",
        }}
      >
        {message}
      </div>
    </div>
  );
});

const TodoUpdateEntry = memo(function TodoUpdateEntry({
  content,
}: {
  content: string;
}) {
  return (
    <div
      className="mx-4 my-1.5 px-3 py-2 rounded-lg bg-[var(--bg-surface)]"
      style={{
        borderLeft: "3px solid var(--accent)",
        boxShadow: "0 1px 2px var(--shadow-msg)",
      }}
    >
      <pre
        className="text-sm text-[var(--text-primary)] whitespace-pre-wrap"
        style={{ fontFamily: "var(--font-mono), ui-monospace, monospace" }}
      >
        {content}
      </pre>
    </div>
  );
});

/** Extract the reasoning string from a think tool's JSON arguments. */
function extractThinkReasoning(args: string): string {
  try {
    const parsed = JSON.parse(args) as Record<string, unknown>;
    if (typeof parsed.reasoning === "string") return parsed.reasoning;
  } catch {
    // fall through
  }
  return args;
}

/** Think tool — inline dark gray text while running, collapsible once complete. */
const ThinkingEntry = memo(function ThinkingEntry({
  arguments: args,
  result,
}: {
  arguments: string;
  result: string | undefined;
}) {
  const [collapsed, setCollapsed] = useState(result !== undefined);
  const reasoning = extractThinkReasoning(args);

  // In-progress: show inline dark gray text.
  if (result === undefined) {
    return (
      <div className="px-4 py-1.5 message-appear">
        <p
          className="text-sm italic whitespace-pre-wrap"
          style={{ color: "var(--text-muted)" }}
        >
          {reasoning}
        </p>
      </div>
    );
  }

  // Complete: collapsible block (collapsed by default).
  return (
    <div className="mx-4 my-1.5 message-appear">
      <button
        onClick={() => { setCollapsed(!collapsed); }}
        className="flex items-center gap-1.5 text-xs hover:underline"
        style={{ color: "var(--text-muted)" }}
      >
        <svg
          width="12"
          height="12"
          viewBox="0 0 12 12"
          fill="none"
          aria-hidden="true"
          className="transition-transform shrink-0"
          style={{
            transform: collapsed ? "rotate(0deg)" : "rotate(90deg)",
            transitionDuration: "var(--duration-fast)",
          }}
        >
          <path
            d="M4 2.5l3.5 3.5L4 9.5"
            stroke="currentColor"
            strokeWidth="1.3"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
        Thinking
      </button>
      <div
        className="grid transition-[grid-template-rows]"
        style={{
          gridTemplateRows: collapsed ? "0fr" : "1fr",
          transitionDuration: "var(--duration-normal)",
          transitionTimingFunction: "var(--ease-spring)",
        }}
      >
        <div className="overflow-hidden">
          <p
            className="text-sm italic whitespace-pre-wrap pt-1"
            style={{ color: "var(--text-muted)" }}
          >
            {reasoning}
          </p>
        </div>
      </div>
    </div>
  );
});

const OrphanToolResult = memo(function OrphanToolResult({
  name,
  result,
  isError,
}: {
  name: string;
  result: string;
  isError: boolean;
}) {
  return (
    <div
      className="mx-4 my-1.5 px-3 py-2 rounded-lg bg-[var(--bg-surface)]"
      style={{
        borderLeft: `3px solid ${isError ? "var(--error)" : "var(--success)"}`,
        boxShadow: "0 1px 2px var(--shadow-msg)",
      }}
    >
      <span
        className="text-sm font-medium"
        style={{
          fontFamily: "var(--font-mono), ui-monospace, monospace",
          color: isError ? "var(--error)" : "var(--success)",
        }}
      >
        {name}
      </span>
      <pre
        className="text-xs text-[var(--text-secondary)] mt-1 overflow-x-auto whitespace-pre-wrap max-h-32 overflow-y-auto"
        style={{ fontFamily: "var(--font-mono), ui-monospace, monospace" }}
      >
        {result}
      </pre>
    </div>
  );
});

/** Renders the main chat stream: user messages, LLM text, tool calls, and tool results. */
export function ChatStream(): React.ReactNode {
  const { state } = useAgentState();
  const containerRef = useRef<HTMLDivElement>(null);
  const isNearBottomRef = useRef(true);
  const scrollRafRef = useRef(0);

  // Pre-resolve entries so each child gets simple, stable props.
  const resolved = useMemo(
    () => resolveEntries(state.entries),
    [state.entries],
  );

  // ── Scroll management ──────────────────────────────────────────────

  const handleScroll = useCallback(() => {
    const el = containerRef.current;
    if (!el) return;
    isNearBottomRef.current =
      el.scrollHeight - el.scrollTop - el.clientHeight < 80;
  }, []);

  const scrollToBottom = useCallback(() => {
    cancelAnimationFrame(scrollRafRef.current);
    scrollRafRef.current = requestAnimationFrame(() => {
      const el = containerRef.current;
      if (el) {
        el.scrollTop = el.scrollHeight;
      }
    });
  }, []);

  useEffect(() => {
    return () => {
      cancelAnimationFrame(scrollRafRef.current);
    };
  }, []);

  useEffect(() => {
    if (isNearBottomRef.current) {
      scrollToBottom();
    }
  }, [state.entries.length, scrollToBottom]);

  useEffect(() => {
    if (
      isNearBottomRef.current &&
      (state.streamingBuffer || state.reasoningBuffer)
    ) {
      scrollToBottom();
    }
  }, [state.streamingBuffer, state.reasoningBuffer, scrollToBottom]);

  // ── Empty state ─────────────────────────────────────────────────────

  if (resolved.length === 0 && !state.streamingBuffer && !state.reasoningBuffer) {
    return (
      <div className="h-full flex items-center justify-center">
        <div className="text-center space-y-2">
          <p className="text-[var(--text-primary)] text-lg font-medium">
            {state.model || "Cinch Agent"}
          </p>
          <p className="text-[var(--text-muted)] text-sm">
            Send a message to begin
          </p>
        </div>
      </div>
    );
  }

  // ── Render ─────────────────────────────────────────────────────────

  return (
    <div
      ref={containerRef}
      onScroll={handleScroll}
      className="h-full overflow-y-auto"
    >
      <div className="flex flex-col max-w-3xl mx-auto py-4">
        {resolved.map((entry, i) => {
          switch (entry.type) {
            case "text":
              return <TextEntry key={i} text={entry.text} />;
            case "user":
              return <UserMessageEntry key={i} message={entry.message} />;
            case "tool":
              if (entry.name === "think") {
                return (
                  <ThinkingEntry
                    key={i}
                    arguments={entry.arguments}
                    result={entry.result}
                  />
                );
              }
              return (
                <ToolCall
                  key={i}
                  name={entry.name}
                  arguments={entry.arguments}
                  result={entry.result}
                  isError={entry.isError}
                />
              );
            case "orphan_result":
              return (
                <OrphanToolResult
                  key={i}
                  name={entry.name}
                  result={entry.result}
                  isError={entry.isError}
                />
              );
            case "todo":
              return <TodoUpdateEntry key={i} content={entry.content} />;
          }
        })}

        {/* Live reasoning buffer (streaming thinking) */}
        {state.reasoningBuffer && (
          <div className="px-4 py-1.5">
            <p
              className="text-sm italic whitespace-pre-wrap"
              style={{ color: "var(--text-muted)" }}
            >
              {state.reasoningBuffer}
            </p>
          </div>
        )}

        {/* Live streaming buffer */}
        <StreamingText buffer={state.streamingBuffer} />
      </div>
    </div>
  );
}
