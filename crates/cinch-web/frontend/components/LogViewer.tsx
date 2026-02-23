"use client";

import { memo, useCallback, useRef, useEffect } from "react";
import { useAgentState } from "@/hooks/useAgentState";
import type { LogLevel, LogLine } from "@/lib/types";

/** Color for each log level using new CSS variables. */
function levelColor(level: LogLevel): string {
  switch (level) {
    case "Error":
      return "var(--error)";
    case "Warn":
      return "var(--warning)";
    case "Info":
      return "var(--accent)";
    case "Debug":
      return "var(--text-muted)";
    case "Trace":
      return "var(--text-muted)";
  }
}

/** Faint row background for error/warn rows. */
function rowBg(level: LogLevel): string | undefined {
  switch (level) {
    case "Error":
      return "oklch(from var(--error) l c h / 0.06)";
    case "Warn":
      return "oklch(from var(--warning) l c h / 0.06)";
    case "Info":
    case "Debug":
    case "Trace":
      return undefined;
  }
}

/** Level badge label for alignment. */
function levelLabel(level: LogLevel): string {
  switch (level) {
    case "Error":
      return "ERR ";
    case "Warn":
      return "WARN";
    case "Info":
      return "INFO";
    case "Debug":
      return "DBUG";
    case "Trace":
      return "TRCE";
  }
}

/** Memoized single log row. */
const LogRow = memo(function LogRow({ line }: { line: LogLine }) {
  const bg = rowBg(line.level);
  return (
    <div
      className="flex gap-2 leading-5 px-2 py-px rounded-sm"
      style={bg ? { background: bg } : undefined}
    >
      <span className="text-[var(--text-muted)] shrink-0 select-none">
        {line.time}
      </span>
      <span
        className="shrink-0 font-semibold select-none"
        style={{ color: levelColor(line.level) }}
      >
        {levelLabel(line.level)}
      </span>
      <span className="text-[var(--text-secondary)] break-all">
        {line.message}
      </span>
    </div>
  );
});

/**
 * Scrollable tracing log panel with level-based row tinting.
 */
export function LogViewer(): React.ReactNode {
  const { state } = useAgentState();
  const containerRef = useRef<HTMLDivElement>(null);
  const autoScrollRef = useRef(true);
  const scrollRafRef = useRef(0);

  const handleScroll = useCallback(() => {
    const el = containerRef.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    autoScrollRef.current = atBottom;
  }, []);

  useEffect(() => {
    if (autoScrollRef.current && containerRef.current) {
      cancelAnimationFrame(scrollRafRef.current);
      scrollRafRef.current = requestAnimationFrame(() => {
        if (containerRef.current) {
          containerRef.current.scrollTop = containerRef.current.scrollHeight;
        }
      });
    }
  }, [state.logs.length]);

  useEffect(() => {
    return () => {
      cancelAnimationFrame(scrollRafRef.current);
    };
  }, []);

  if (state.logs.length === 0) {
    return (
      <div className="p-4 text-sm text-[var(--text-muted)]">
        Logs will appear here as the agent runs.
      </div>
    );
  }

  return (
    <div
      ref={containerRef}
      onScroll={handleScroll}
      className="h-full overflow-y-auto p-2 text-xs space-y-px"
      style={{ fontFamily: "var(--font-mono), ui-monospace, monospace" }}
    >
      {state.logs.map((line, i) => (
        <LogRow key={i} line={line} />
      ))}
    </div>
  );
}
