"use client";

import { useState, useEffect, useCallback } from "react";
import { useAgentState } from "@/hooks/useAgentState";
import type { QuestionResponse } from "@/lib/types";

/** SVG radial countdown ring for timeout. */
function TimeoutRing({ remaining, total }: { remaining: number; total: number }): React.ReactNode {
  const r = 14;
  const circumference = 2 * Math.PI * r;
  const progress = total > 0 ? remaining / total : 0;
  const offset = circumference * (1 - progress);

  return (
    <div className="flex items-center gap-2 text-sm text-[var(--text-muted)]">
      <svg width="34" height="34" viewBox="0 0 34 34" className="shrink-0 -rotate-90">
        <circle
          cx="17" cy="17" r={r}
          fill="none"
          stroke="var(--border-dim)"
          strokeWidth="2.5"
        />
        <circle
          cx="17" cy="17" r={r}
          fill="none"
          stroke="var(--accent)"
          strokeWidth="2.5"
          strokeLinecap="round"
          strokeDasharray={circumference}
          strokeDashoffset={offset}
          style={{ transition: "stroke-dashoffset 1s linear" }}
        />
      </svg>
      <span className="tabular-nums">{Math.ceil(remaining)}s</span>
    </div>
  );
}

/**
 * Modal overlay for human-in-the-loop questions.
 * Frosted glass backdrop, card with accent-bordered choices.
 */
export function QuestionModal(): React.ReactNode {
  const { state, sendAnswer } = useAgentState();
  const question = state.activeQuestion;

  const [cursor, setCursor] = useState(0);
  const [editing, setEditing] = useState(false);
  const [editText, setEditText] = useState("");

  // Track total timeout for the ring proportion.
  const [totalTimeout, setTotalTimeout] = useState<number | null>(null);

  // Reset cursor when a new question appears.
  const currentPrompt = question && !question.done ? question.question.prompt : undefined;
  const [prevPrompt, setPrevPrompt] = useState<string | undefined>(undefined);
  if (currentPrompt !== prevPrompt) {
    setPrevPrompt(currentPrompt);
    if (currentPrompt !== undefined) {
      setCursor(0);
      setEditing(false);
      setEditText("");
      setTotalTimeout(question?.remaining_secs ?? null);
    }
  }

  const submit = useCallback(
    (response: QuestionResponse) => {
      sendAnswer(response);
    },
    [sendAnswer],
  );

  // Keyboard navigation.
  useEffect(() => {
    if (!question || question.done || editing) return;

    function handleKey(e: KeyboardEvent): void {
      if (!question) return;
      const count = question.question.choices.length;

      switch (e.key) {
        case "ArrowUp":
        case "k":
          e.preventDefault();
          setCursor((c) => (c - 1 + count) % count);
          break;
        case "ArrowDown":
        case "j":
          e.preventDefault();
          setCursor((c) => (c + 1) % count);
          break;
        case "Enter": {
          e.preventDefault();
          const selected = question.question.choices[cursor];
          if (selected === undefined) break;
          if (question.question.editable) {
            setEditing(true);
            setEditText(selected.body);
          } else {
            submit({ Selected: cursor });
          }
          break;
        }
        case "Escape":
          e.preventDefault();
          submit("Skipped");
          break;
      }
    }

    window.addEventListener("keydown", handleKey);
    return () => { window.removeEventListener("keydown", handleKey); };
  }, [question, cursor, editing, submit]);

  if (!question || question.done) return null;

  const { choices, prompt, editable, max_edit_length } = question.question;

  return (
    <div
      className="fixed inset-0 flex items-center justify-center z-50"
      style={{
        background: "oklch(0 0 0 / 0.20)",
        backdropFilter: "blur(16px) saturate(0.5)",
        WebkitBackdropFilter: "blur(16px) saturate(0.5)",
        animation: "message-appear var(--duration-normal) var(--ease-out) both",
      }}
    >
      <div
        className="max-w-2xl w-full mx-4 p-6 rounded-2xl border border-[var(--border)]"
        style={{
          background: "var(--bg-surface)",
          boxShadow: "0 4px 24px var(--shadow-msg), 0 0 0 1px var(--border-dim)",
        }}
      >
        <div className="flex items-start justify-between gap-4 mb-4">
          <h2 className="text-lg font-semibold text-[var(--text-primary)]">
            {prompt}
          </h2>
          {question.remaining_secs !== null && totalTimeout !== null && (
            <TimeoutRing
              remaining={question.remaining_secs}
              total={totalTimeout}
            />
          )}
        </div>

        {editing ? (
          /* Edit mode */
          <div className="space-y-3">
            <textarea
              autoFocus
              value={editText}
              onChange={(e) => { setEditText(e.target.value); }}
              maxLength={max_edit_length ?? undefined}
              className="w-full h-32 rounded-xl p-3 text-sm text-[var(--text-primary)] resize-none border focus:outline-none transition-colors"
              style={{
                background: "var(--bg-overlay)",
                borderColor: "var(--border)",
              }}
            />
            {max_edit_length && (
              <p
                className="text-xs"
                style={{
                  color: editText.length > max_edit_length ? "var(--error)" : "var(--text-muted)",
                }}
              >
                {editText.length}/{max_edit_length}
              </p>
            )}
            <div className="flex gap-2">
              <button
                onClick={() =>
                  { submit({ SelectedEdited: { index: cursor, edited_text: editText } }); }
                }
                className="px-4 py-2 rounded-lg text-sm font-medium text-white transition-opacity hover:opacity-90"
                style={{ background: "var(--accent)" }}
              >
                Submit
              </button>
              <button
                onClick={() => { setEditing(false); }}
                className="px-4 py-2 rounded-lg text-sm text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-subtle)]"
                style={{ background: "var(--bg-overlay)" }}
              >
                Back
              </button>
            </div>
          </div>
        ) : (
          /* Selection mode */
          <div className="space-y-1.5">
            {choices.map((choice, i) => (
              <button
                key={i}
                onClick={() => {
                  if (editable) {
                    setCursor(i);
                    setEditing(true);
                    setEditText(choice.body);
                  } else {
                    submit({ Selected: i });
                  }
                }}
                className="w-full text-left px-3 py-2.5 rounded-xl transition-all"
                style={{
                  borderLeft: i === cursor ? "3px solid var(--accent)" : "3px solid transparent",
                  background: i === cursor
                    ? "oklch(from var(--accent) l c h / 0.08)"
                    : "transparent",
                  transform: i === cursor ? "translateY(-1px)" : "translateY(0)",
                  transitionDuration: "var(--duration-fast)",
                }}
                onMouseEnter={() => { setCursor(i); }}
              >
                <div className="flex items-center justify-between">
                  <span className="text-sm font-medium text-[var(--text-primary)]">
                    {choice.label}
                  </span>
                  {choice.metadata && (
                    <span className="text-xs text-[var(--text-muted)]">
                      {choice.metadata}
                    </span>
                  )}
                </div>
                <p className="text-xs text-[var(--text-secondary)] mt-0.5 line-clamp-2">
                  {choice.body}
                </p>
              </button>
            ))}
          </div>
        )}

        {!editing && (
          <div className="mt-4 flex items-center justify-between">
            <button
              onClick={() => { submit("Skipped"); }}
              className="text-sm text-[var(--text-muted)] hover:text-[var(--text-secondary)] transition-colors"
            >
              Skip (Esc)
            </button>
            <span className="text-xs text-[var(--text-muted)]">
              {"\u2191\u2193"} navigate &middot; Enter select
            </span>
          </div>
        )}
      </div>
    </div>
  );
}
