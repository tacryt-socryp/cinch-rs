"use client";

import { useState, useCallback, useEffect } from "react";
import { useAgentSocket } from "@/hooks/useAgentSocket";
import { AgentContext } from "@/hooks/useAgentState";
import { StatusBar } from "@/components/StatusBar";
import { ChatStream } from "@/components/ChatStream";
import { ChatInput } from "@/components/ChatInput";
import { InspectorPanel } from "@/components/InspectorPanel";
import { QuestionModal } from "@/components/QuestionModal";
import { ConnectionStatus } from "@/components/ConnectionStatus";

/**
 * Resolve the backend URL for the WebSocket connection.
 */
function getBackendUrl(): string {
  if (typeof window === "undefined") return "http://127.0.0.1:3001";

  const meta = document.querySelector('meta[name="cinch-backend"]');
  if (meta) {
    const content = meta.getAttribute("content");
    if (content) return content;
  }

  const { protocol, hostname, port } = window.location;
  if (port === "3000") {
    return `${protocol}//${hostname}:3001`;
  }
  return window.location.origin;
}

export default function AgentPage(): React.ReactNode {
  const [inspectorOpen, setInspectorOpen] = useState(false);
  const backendUrl = getBackendUrl();
  const agentSocket = useAgentSocket(backendUrl);

  const toggleInspector = useCallback(() => {
    setInspectorOpen((v) => !v);
  }, []);

  // Keyboard shortcuts.
  useEffect(() => {
    function handleKey(e: KeyboardEvent): void {
      // Cmd/Ctrl+I — toggle inspector
      if ((e.metaKey || e.ctrlKey) && e.key === "i") {
        e.preventDefault();
        toggleInspector();
      }
      // Cmd/Ctrl+K — focus chat input
      if ((e.metaKey || e.ctrlKey) && e.key === "k") {
        e.preventDefault();
        const input = document.querySelector<HTMLTextAreaElement>("textarea");
        input?.focus();
      }
    }

    window.addEventListener("keydown", handleKey);
    return () => { window.removeEventListener("keydown", handleKey); };
  }, [toggleInspector]);

  return (
    <AgentContext.Provider value={agentSocket}>
      <StatusBar />

      <div className="flex flex-1 overflow-hidden">
        {/* Main chat area */}
        <main className="flex-1 overflow-hidden flex flex-col">
          <div className="flex-1 overflow-hidden">
            <ChatStream />
          </div>
          <ChatInput />
        </main>

        {/* Inspector panel (slides in from right) */}
        <InspectorPanel
          open={inspectorOpen}
          onClose={() => { setInspectorOpen(false); }}
        />
      </div>

      {/* Inspector toggle button (fixed) */}
      <button
        onClick={toggleInspector}
        className="fixed bottom-6 right-6 z-20 w-9 h-9 flex items-center justify-center rounded-full border transition-all"
        style={{
          background: inspectorOpen ? "var(--accent)" : "var(--bg-surface)",
          borderColor: inspectorOpen ? "var(--accent)" : "var(--border)",
          color: inspectorOpen ? "white" : "var(--text-muted)",
          boxShadow: "0 2px 8px var(--shadow-msg)",
        }}
        title={`${inspectorOpen ? "Close" : "Open"} inspector (Cmd+I)`}
      >
        <svg width="16" height="16" viewBox="0 0 16 16" fill="none" aria-hidden="true">
          <rect x="1" y="1" width="14" height="14" rx="2" stroke="currentColor" strokeWidth="1.5" />
          <line x1="10" y1="1" x2="10" y2="15" stroke="currentColor" strokeWidth="1.5" />
        </svg>
      </button>

      <QuestionModal />
      <ConnectionStatus />
    </AgentContext.Provider>
  );
}
