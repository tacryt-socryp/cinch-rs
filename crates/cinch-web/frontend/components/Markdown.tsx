"use client";

import { memo, useState, useCallback, type ComponentPropsWithoutRef } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

/** Copy icon. */
function CopyIcon(): React.ReactNode {
  return (
    <svg width="14" height="14" viewBox="0 0 14 14" fill="none" aria-hidden="true">
      <rect x="4" y="4" width="8" height="8" rx="1.5" stroke="currentColor" strokeWidth="1.2" />
      <path d="M10 4V2.5A1.5 1.5 0 008.5 1h-6A1.5 1.5 0 001 2.5v6A1.5 1.5 0 002.5 10H4" stroke="currentColor" strokeWidth="1.2" />
    </svg>
  );
}

/** Copy-to-clipboard button. */
function CopyButton({ text }: { text: string }): React.ReactNode {
  const [copied, setCopied] = useState(false);

  const handleCopy = useCallback(() => {
    void navigator.clipboard.writeText(text).then(() => {
      setCopied(true);
      setTimeout(() => { setCopied(false); }, 1500);
    });
  }, [text]);

  return (
    <button
      onClick={handleCopy}
      className="flex items-center gap-1 px-1.5 py-0.5 rounded text-xs transition-colors hover:bg-[var(--bg-subtle)]"
      style={{ color: copied ? "var(--success)" : "var(--text-muted)" }}
      title="Copy to clipboard"
    >
      {copied ? "Copied" : <CopyIcon />}
    </button>
  );
}

/** Custom code block with language label and copy button. */
function CodeBlock({
  children,
  className,
}: ComponentPropsWithoutRef<"code"> & { inline?: boolean }): React.ReactNode {
  const match = /language-(\w+)/.exec(className ?? "");
  const lang = match?.[1];
  const raw = Array.isArray(children)
    ? (children as string[]).join("")
    : typeof children === "string" ? children : "";
  const code = raw.replace(/\n$/, "");

  // Inline code
  if (!lang && !code.includes("\n")) {
    return (
      <code
        className="px-1.5 py-0.5 rounded text-sm"
        style={{
          background: "var(--bg-subtle)",
          fontFamily: "var(--font-mono), ui-monospace, monospace",
        }}
      >
        {children}
      </code>
    );
  }

  // Block code
  return (
    <div className="my-2 rounded-lg overflow-hidden" style={{ background: "var(--bg-overlay)" }}>
      <div
        className="flex items-center justify-between px-3 py-1.5 border-b"
        style={{ borderColor: "var(--border-dim)" }}
      >
        <span className="text-xs text-[var(--text-muted)]">{lang ?? "text"}</span>
        <CopyButton text={code} />
      </div>
      <pre className="p-3 overflow-x-auto text-sm leading-relaxed" style={{ fontFamily: "var(--font-mono), ui-monospace, monospace" }}>
        <code>{code}</code>
      </pre>
    </div>
  );
}

/**
 * Markdown renderer wrapping react-markdown with GFM support
 * and custom styled renderers for code, tables, links, etc.
 */
export const Markdown = memo(function Markdown({ content }: { content: string }) {
  return (
    <div className="prose-agent">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          code: CodeBlock,
          a: ({ href, children }) => (
            <a
              href={href}
              target="_blank"
              rel="noopener noreferrer"
              className="underline-offset-2 hover:underline transition-colors"
              style={{ color: "var(--accent)" }}
            >
              {children}
            </a>
          ),
          blockquote: ({ children }) => (
            <blockquote
              className="my-2 pl-3 italic text-[var(--text-muted)]"
              style={{ borderLeft: "3px solid var(--accent)" }}
            >
              {children}
            </blockquote>
          ),
          table: ({ children }) => (
            <div className="my-2 overflow-x-auto rounded-lg border border-[var(--border-dim)]">
              <table className="w-full text-sm">{children}</table>
            </div>
          ),
          thead: ({ children }) => (
            <thead style={{ background: "var(--bg-subtle)" }}>{children}</thead>
          ),
          th: ({ children }) => (
            <th className="px-3 py-1.5 text-left text-xs font-semibold text-[var(--text-secondary)] border-b border-[var(--border-dim)]">
              {children}
            </th>
          ),
          td: ({ children }) => (
            <td className="px-3 py-1.5 text-[var(--text-primary)] border-b border-[var(--border-dim)]">
              {children}
            </td>
          ),
          ul: ({ children }) => (
            <ul className="my-1 ml-4 list-disc text-[var(--text-primary)] space-y-0.5">{children}</ul>
          ),
          ol: ({ children }) => (
            <ol className="my-1 ml-4 list-decimal text-[var(--text-primary)] space-y-0.5">{children}</ol>
          ),
          p: ({ children }) => (
            <p className="my-1.5 leading-relaxed">{children}</p>
          ),
          hr: () => (
            <hr className="my-3 border-[var(--border-dim)]" />
          ),
          h1: ({ children }) => (
            <h1 className="text-xl font-semibold text-[var(--text-primary)] mt-4 mb-2">{children}</h1>
          ),
          h2: ({ children }) => (
            <h2 className="text-lg font-semibold text-[var(--text-primary)] mt-3 mb-1.5">{children}</h2>
          ),
          h3: ({ children }) => (
            <h3 className="text-base font-semibold text-[var(--text-primary)] mt-2 mb-1">{children}</h3>
          ),
        }}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
});
