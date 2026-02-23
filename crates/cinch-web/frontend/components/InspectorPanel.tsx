"use client";

import { useCallback, useEffect } from "react";
import { useAgentState } from "@/hooks/useAgentState";
import { LogViewer } from "./LogViewer";

/** Close icon for the panel header. */
function CloseIcon(): React.ReactNode {
  return (
    <svg width="14" height="14" viewBox="0 0 14 14" fill="none" aria-hidden="true">
      <path
        d="M3 3l8 8m0-8l-8 8"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
    </svg>
  );
}

/**
 * Right-side inspector panel showing agent logs.
 * Slides in/out with a transform transition.
 */
export function InspectorPanel({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}): React.ReactNode {
  const { state } = useAgentState();

  // Close on Escape.
  const handleKeyDown = useCallback(
    (e: KeyboardEvent) => {
      if (e.key === "Escape" && open) {
        onClose();
      }
    },
    [open, onClose],
  );

  useEffect(() => {
    window.addEventListener("keydown", handleKeyDown);
    return () => { window.removeEventListener("keydown", handleKeyDown); };
  }, [handleKeyDown]);

  return (
    <aside
      className="border-l border-[var(--border)] overflow-hidden flex flex-col bg-[var(--bg-base)] shrink-0 transition-[width,opacity]"
      style={{
        width: open ? "35%" : "0",
        minWidth: open ? "400px" : "0",
        opacity: open ? 1 : 0,
        transitionDuration: "var(--duration-normal)",
        transitionTimingFunction: "var(--ease-spring)",
      }}
    >
      {/* Header */}
      <div className="flex items-center justify-between px-3 py-2 border-b border-[var(--border-dim)] bg-[var(--bg-surface)] shrink-0">
        <span className="px-2.5 py-1 text-xs font-medium text-[var(--text-primary)]">
          Logs
        </span>
        <div className="flex items-center gap-2">
          <span className="text-xs text-[var(--text-muted)] tabular-nums">
            {state.logs.length}
          </span>
          <button
            onClick={onClose}
            className="p-1 rounded-md text-[var(--text-muted)] hover:text-[var(--text-secondary)] hover:bg-[var(--bg-subtle)] transition-colors"
            title="Close inspector (Esc)"
          >
            <CloseIcon />
          </button>
        </div>
      </div>

      {/* Logs content */}
      <div className="flex-1 overflow-hidden">
        <LogViewer />
      </div>
    </aside>
  );
}
