"use client";

import { memo } from "react";

/** Renders the in-progress streaming text buffer with breathing dot indicator. */
export const StreamingText = memo(function StreamingText({
  buffer,
}: {
  buffer: string;
}) {
  if (!buffer) return null;

  return (
    <div className="px-4 py-2 whitespace-pre-wrap text-[var(--text-primary)] message-appear">
      {buffer}
      <span className="streaming-dots inline-flex ml-1 text-[var(--accent)]" aria-label="Streaming">
        <span>&#8226;</span>
        <span>&#8226;</span>
        <span>&#8226;</span>
      </span>
    </div>
  );
});
