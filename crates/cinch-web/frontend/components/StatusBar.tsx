"use client";

import { useAgentState } from "@/hooks/useAgentState";

/** Format a token count to a compact string (e.g. 1.2k, 45k). */
function formatTokens(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

/** Top status rail — 40px frosted glass bar. */
export function StatusBar(): React.ReactNode {
  const { state, connected } = useAgentState();
  const ctxPct = Math.round(state.contextPct * 100);
  const totalTokens = state.totalPromptTokens + state.totalCompletionTokens;

  const ctxColor =
    ctxPct >= 80
      ? "text-[var(--error)]"
      : ctxPct >= 60
        ? "text-[var(--warning)]"
        : "text-[var(--text-secondary)]";

  return (
    <header
      className="flex items-center gap-3 px-4 h-10 border-b border-[var(--border-dim)] text-sm shrink-0 sticky top-0 z-30"
      style={{
        background: "oklch(from var(--bg-surface) l c h / 0.8)",
        backdropFilter: "blur(12px)",
        WebkitBackdropFilter: "blur(12px)",
      }}
    >
      {/* Connection dot */}
      <span
        className={`inline-block w-2 h-2 rounded-full shrink-0 ${connected ? "bg-[var(--success)]" : "bg-[var(--error)]"}`}
        style={connected ? { boxShadow: "0 0 6px var(--success)" } : undefined}
        title={connected ? "Connected" : "Disconnected"}
      />

      {/* Phase pill */}
      <span
        className="px-2.5 py-0.5 rounded-full text-xs font-medium truncate max-w-48"
        style={{
          background: "oklch(from var(--accent) l c h / 0.10)",
          color: "var(--accent)",
        }}
      >
        {state.phase}
      </span>

      {/* Model name (center, muted) */}
      {state.model && (
        <span className="text-[var(--text-muted)] text-xs truncate max-w-48 ml-auto mr-auto hidden sm:block">
          {state.model}
        </span>
      )}

      {/* Right cluster */}
      <div className="flex items-center gap-3 ml-auto">
        {/* Round counter */}
        <span
          className="text-xs tabular-nums"
          style={{ fontFamily: "var(--font-mono), ui-monospace, monospace" }}
        >
          <span className="text-[var(--text-primary)]">{state.round}</span>
        </span>

        {/* Context % */}
        <span className={`text-xs tabular-nums ${ctxColor}`}>
          {ctxPct}%
        </span>

        {/* Token count */}
        {totalTokens > 0 && (
          <span
            className="text-xs text-[var(--text-muted)] tabular-nums"
            title={`Prompt: ${String(state.totalPromptTokens)} | Completion: ${String(state.totalCompletionTokens)}`}
          >
            {formatTokens(totalTokens)} tok
          </span>
        )}

        {/* Cycle */}
        {state.cycle > 0 && (
          <span className="text-xs text-[var(--text-secondary)] tabular-nums">
            C{state.cycle}
          </span>
        )}

        {/* Running indicator */}
        {state.running ? (
          <div className="w-12 h-1 rounded-full overflow-hidden bg-[var(--bg-subtle)]">
            <div className="shimmer-bar w-full h-full rounded-full" />
          </div>
        ) : (
          <span className="text-xs text-[var(--text-muted)]">Idle</span>
        )}
      </div>

      {/* Extension fields */}
      {state.extension &&
        Object.entries(state.extension).map(([key, value]) => (
          <span
            key={key}
            className="px-2 py-0.5 rounded-full text-xs bg-[var(--bg-subtle)] text-[var(--text-secondary)]"
          >
            {key}: {String(value)}
          </span>
        ))}
    </header>
  );
}
