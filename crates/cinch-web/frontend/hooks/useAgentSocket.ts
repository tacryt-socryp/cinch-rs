"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import type { AgentState, QuestionResponse } from "@/lib/types";
import { INITIAL_STATE } from "@/lib/types";
import { applyMessage, type WsServerMessage } from "@/lib/protocol";

const MAX_RETRY_DELAY = 16_000;

/**
 * Central hook managing the WebSocket connection to the cinch-web backend.
 *
 * Returns the current agent state (updated in real-time), connection status,
 * and callbacks for sending answers and quit requests.
 */
export function useAgentSocket(backendUrl: string): {
  state: AgentState;
  connected: boolean;
  sendAnswer: (response: QuestionResponse) => void;
  sendChat: (message: string) => void;
  sendQuit: () => void;
} {
  const [state, setState] = useState<AgentState>(INITIAL_STATE);
  const [connected, setConnected] = useState(false);
  const wsRef = useRef<WebSocket | null>(null);
  const retryDelayRef = useRef(1000);

  useEffect(() => {
    let cancelled = false;
    let timeoutId: ReturnType<typeof setTimeout> | null = null;

    function connect(): void {
      if (cancelled) return;

      const wsUrl = backendUrl.replace(/^http/, "ws") + "/ws";
      const ws = new WebSocket(wsUrl);
      wsRef.current = ws;

      ws.onopen = () => {
        setConnected(true);
        retryDelayRef.current = 1000;
      };

      ws.onmessage = (event: MessageEvent) => {
        try {
          const msg = JSON.parse(event.data as string) as WsServerMessage;
          setState((prev) => applyMessage(prev, msg));
        } catch {
          // Ignore malformed messages.
        }
      };

      ws.onclose = () => {
        setConnected(false);
        wsRef.current = null;

        if (!cancelled) {
          const delay = Math.min(retryDelayRef.current, MAX_RETRY_DELAY);
          retryDelayRef.current = delay * 2;
          timeoutId = setTimeout(connect, delay);
        }
      };

      ws.onerror = () => {
        // onclose will fire after onerror — reconnect handled there.
      };
    }

    connect();

    return () => {
      cancelled = true;
      if (timeoutId !== null) clearTimeout(timeoutId);
      wsRef.current?.close();
    };
  }, [backendUrl]);

  const sendAnswer = useCallback((response: QuestionResponse) => {
    const ws = wsRef.current;
    if (ws?.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: "answer", response }));
    }
  }, []);

  const sendChat = useCallback((message: string) => {
    const ws = wsRef.current;
    if (ws?.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: "chat", message }));
    }
  }, []);

  const sendQuit = useCallback(() => {
    const ws = wsRef.current;
    if (ws?.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: "quit" }));
    }
  }, []);

  return { state, connected, sendAnswer, sendChat, sendQuit };
}
