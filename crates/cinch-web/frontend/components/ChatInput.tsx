"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { useAgentState } from "@/hooks/useAgentState";

/** Arrow-up send icon (inline SVG). */
function SendIcon(): React.ReactNode {
  return (
    <svg width="18" height="18" viewBox="0 0 18 18" fill="none" aria-hidden="true">
      <path
        d="M9 14V4m0 0L4.5 8.5M9 4l4.5 4.5"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

/** Floating bottom-pinned chat input with auto-expanding textarea. */
export function ChatInput(): React.ReactNode {
  const { connected, state, sendChat } = useAgentState();
  const [value, setValue] = useState("");
  const [focused, setFocused] = useState(false);
  const inputRef = useRef<HTMLTextAreaElement>(null);

  const canSend = connected && value.trim().length > 0 && !state.running;

  // Auto-resize textarea height.
  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${String(Math.min(el.scrollHeight, 160))}px`;
  }, [value]);

  const submit = useCallback(() => {
    const trimmed = value.trim();
    if (!trimmed || !connected) return;
    sendChat(trimmed);
    setValue("");
    inputRef.current?.focus();
  }, [value, connected, sendChat]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        submit();
      }
    },
    [submit],
  );

  return (
    <div className="px-4 pb-4 pt-2 shrink-0">
      {/* Running shimmer bar */}
      {state.running && (
        <div className="shimmer-bar mx-auto max-w-3xl rounded-full mb-2" />
      )}

      <div
        className="relative max-w-3xl mx-auto rounded-2xl border transition-shadow"
        style={{
          background: "var(--bg-surface)",
          borderColor: focused ? "var(--accent)" : "var(--border)",
          boxShadow: focused
            ? "0 0 0 2px var(--accent-glow), 0 2px 8px var(--shadow-msg)"
            : "0 2px 8px var(--shadow-msg)",
        }}
      >
        <div className="flex items-end gap-2 p-2">
          <textarea
            ref={inputRef}
            value={value}
            onChange={(e) => { setValue(e.target.value); }}
            onKeyDown={handleKeyDown}
            onFocus={() => { setFocused(true); }}
            onBlur={() => { setFocused(false); }}
            placeholder={
              !connected
                ? "Disconnected..."
                : state.running
                  ? "Agent is running..."
                  : "Send a message..."
            }
            disabled={!connected}
            rows={1}
            className="flex-1 resize-none bg-transparent px-2 py-1.5 text-sm text-[var(--text-primary)] placeholder:text-[var(--text-muted)] focus:outline-none disabled:opacity-50"
            style={{ minHeight: "1.75rem", maxHeight: "10rem" }}
          />
          <button
            onClick={submit}
            disabled={!canSend}
            title="Send message (Enter)"
            className="shrink-0 w-8 h-8 flex items-center justify-center rounded-full text-white transition-all disabled:cursor-not-allowed"
            style={{
              background: canSend ? "var(--accent)" : "var(--bg-subtle)",
              color: canSend ? "white" : "var(--text-muted)",
              opacity: canSend ? 1 : 0.5,
              transform: canSend ? "scale(1)" : "scale(0.9)",
            }}
          >
            <SendIcon />
          </button>
        </div>

        {/* Keyboard hint */}
        {focused && !state.running && (
          <div className="absolute -bottom-5 left-1/2 -translate-x-1/2 text-[10px] text-[var(--text-muted)] select-none">
            Enter &crarr;
          </div>
        )}
      </div>
    </div>
  );
}
