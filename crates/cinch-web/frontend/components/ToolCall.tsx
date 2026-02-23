"use client";

import { memo, useState } from "react";

/** Chevron icon — rotates when expanded. */
function ChevronIcon({ expanded }: { expanded: boolean }): React.ReactNode {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 14 14"
      fill="none"
      aria-hidden="true"
      className="transition-transform shrink-0"
      style={{
        transform: expanded ? "rotate(90deg)" : "rotate(0deg)",
        transitionDuration: "var(--duration-fast)",
      }}
    >
      <path
        d="M5 3l4 4-4 4"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

/** Summarize tool arguments to a short preview string. */
function summarizeArgs(args: string): string {
  try {
    const parsed = JSON.parse(args) as Record<string, unknown>;
    const keys = Object.keys(parsed);
    if (keys.length === 0) return "{}";
    const firstKey = keys[0];
    if (firstKey === undefined) return "{}";
    const first: unknown = parsed[firstKey];
    const preview =
      typeof first === "string" ? first : JSON.stringify(first);
    const truncated =
      preview.length > 60 ? preview.slice(0, 57) + "..." : preview;
    return keys.length > 1
      ? `${firstKey}=${truncated} (+${String(keys.length - 1)})`
      : `${firstKey}=${truncated}`;
  } catch {
    return args.length > 60 ? args.slice(0, 57) + "..." : args;
  }
}

/** Render a single argument value based on its type. */
function ArgValue({ value }: { value: unknown }): React.ReactNode {
  if (value === null || value === undefined) {
    return (
      <span
        className="text-xs italic"
        style={{ color: "var(--text-muted)" }}
      >
        null
      </span>
    );
  }
  if (typeof value === "string") {
    if (value.includes("\n") || value.length > 120) {
      return (
        <pre
          className="text-xs overflow-x-auto whitespace-pre-wrap break-all mt-0.5"
          style={{
            fontFamily: "var(--font-mono), ui-monospace, monospace",
            color: "var(--text-secondary)",
          }}
        >
          {value}
        </pre>
      );
    }
    return (
      <span
        className="text-xs"
        style={{ color: "var(--text-secondary)" }}
      >
        {value}
      </span>
    );
  }
  if (typeof value === "number" || typeof value === "boolean") {
    return (
      <code
        className="text-xs px-1 py-0.5 rounded"
        style={{
          background: "var(--bg-subtle)",
          fontFamily: "var(--font-mono), ui-monospace, monospace",
          color: "var(--text-secondary)",
        }}
      >
        {String(value)}
      </code>
    );
  }
  return (
    <pre
      className="text-xs overflow-x-auto whitespace-pre-wrap break-all mt-0.5"
      style={{
        fontFamily: "var(--font-mono), ui-monospace, monospace",
        color: "var(--text-secondary)",
      }}
    >
      {JSON.stringify(value, null, 2)}
    </pre>
  );
}

/** Render tool arguments as structured key-value pairs. */
function ToolArguments({ args }: { args: string }): React.ReactNode {
  try {
    const parsed = JSON.parse(args) as Record<string, unknown>;
    const entries = Object.entries(parsed);
    if (entries.length === 0) {
      return (
        <span
          className="text-xs italic"
          style={{ color: "var(--text-muted)" }}
        >
          No arguments
        </span>
      );
    }
    return (
      <div className="space-y-2">
        {entries.map(([key, value]) => (
          <div key={key}>
            <div
              className="text-xs font-semibold mb-0.5"
              style={{
                fontFamily: "var(--font-mono), ui-monospace, monospace",
                color: "var(--text-muted)",
              }}
            >
              {key}
            </div>
            <ArgValue value={value} />
          </div>
        ))}
      </div>
    );
  } catch {
    return (
      <pre
        className="text-xs overflow-x-auto whitespace-pre-wrap break-all"
        style={{
          fontFamily: "var(--font-mono), ui-monospace, monospace",
          color: "var(--text-secondary)",
        }}
      >
        {args}
      </pre>
    );
  }
}

interface ToolCallProps {
  name: string;
  arguments: string;
  result?: string | undefined;
  isError?: boolean | undefined;
}

/** Collapsible tool call card with colored left accent border. */
export const ToolCall = memo(function ToolCall({
  name,
  arguments: args,
  result,
  isError,
}: ToolCallProps) {
  const [expanded, setExpanded] = useState(false);

  const accentColor =
    result === undefined
      ? "var(--accent)"    // running / pending
      : isError
        ? "var(--error)"
        : "var(--success)";

  return (
    <div
      className="mx-4 my-1.5 rounded-lg overflow-hidden bg-[var(--bg-surface)]"
      style={{
        borderLeft: `3px solid ${accentColor}`,
        boxShadow: "0 1px 2px var(--shadow-msg)",
      }}
    >
      <button
        onClick={() => { setExpanded(!expanded); }}
        className="w-full text-left px-3 py-2 flex items-center gap-2 hover:bg-[var(--bg-subtle)] transition-colors"
        style={{ transitionDuration: "var(--duration-fast)" }}
      >
        <ChevronIcon expanded={expanded} />
        <span
          className="text-[var(--accent)] font-semibold text-sm"
          style={{ fontFamily: "var(--font-mono), ui-monospace, monospace" }}
        >
          {name}
        </span>
        <span className="text-[var(--text-muted)] text-xs truncate">
          {summarizeArgs(args)}
        </span>
        {result !== undefined && (
          <span
            className="ml-auto text-xs font-medium px-1.5 py-0.5 rounded-full"
            style={{
              background: isError
                ? "oklch(from var(--error) l c h / 0.12)"
                : "oklch(from var(--success) l c h / 0.12)",
              color: accentColor,
            }}
          >
            {isError ? "ERR" : "OK"}
          </span>
        )}
      </button>

      <div
        className="grid transition-[grid-template-rows]"
        style={{
          gridTemplateRows: expanded ? "1fr" : "0fr",
          transitionDuration: "var(--duration-normal)",
          transitionTimingFunction: "var(--ease-spring)",
        }}
      >
        <div className="overflow-hidden">
          <div className="border-t border-[var(--border-dim)] px-3 py-2 space-y-2">
            <ToolArguments args={args} />
            {result !== undefined && (
              <div>
                <div className="text-xs text-[var(--text-muted)] mb-1">
                  Result
                </div>
                <pre
                  className="text-xs overflow-x-auto whitespace-pre-wrap break-all max-h-64 overflow-y-auto"
                  style={{
                    fontFamily: "var(--font-mono), ui-monospace, monospace",
                    color: isError ? "var(--error)" : "var(--text-secondary)",
                  }}
                >
                  {result}
                </pre>
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
});

