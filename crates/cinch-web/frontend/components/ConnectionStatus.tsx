"use client";

import { useEffect, useRef, useSyncExternalStore, useCallback } from "react";
import { useAgentState } from "@/hooks/useAgentState";

type Phase = "hidden" | "disconnected" | "reconnected" | "exiting";

/**
 * Toast notification for connection status.
 * Slides in from top-right when disconnected, auto-dismisses after reconnection.
 *
 * Uses a ref-based state machine to avoid setState-in-effect lint issues.
 * The phase ref is subscribed via useSyncExternalStore to trigger re-renders.
 */
export function ConnectionStatus(): React.ReactNode {
  const { connected } = useAgentState();

  const phaseRef = useRef<Phase>("hidden");
  const listenersRef = useRef(new Set<() => void>());
  const prevConnectedRef = useRef(true);

  const subscribe = useCallback((cb: () => void) => {
    listenersRef.current.add(cb);
    return () => { listenersRef.current.delete(cb); };
  }, []);

  const getSnapshot = useCallback(() => phaseRef.current, []);

  const setPhase = useCallback((p: Phase) => {
    phaseRef.current = p;
    listenersRef.current.forEach((cb) => { cb(); });
  }, []);

  const phase = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);

  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    let exitTimer: ReturnType<typeof setTimeout> | undefined;

    const wasConnected = prevConnectedRef.current;
    prevConnectedRef.current = connected;

    if (!connected) {
      setPhase("disconnected");
    } else if (!wasConnected) {
      // Just reconnected
      setPhase("reconnected");
      timer = setTimeout(() => {
        setPhase("exiting");
      }, 2000);
      exitTimer = setTimeout(() => {
        setPhase("hidden");
      }, 2350);
    }

    return () => {
      if (timer !== undefined) clearTimeout(timer);
      if (exitTimer !== undefined) clearTimeout(exitTimer);
    };
  }, [connected, setPhase]);

  if (phase === "hidden") return null;

  const isConnected = phase === "reconnected" || phase === "exiting";

  return (
    <div
      className="fixed top-14 right-4 z-50 flex items-center gap-2 px-4 py-2.5 rounded-xl border text-sm"
      style={{
        background: "var(--bg-surface)",
        borderColor: isConnected ? "var(--success)" : "var(--error)",
        boxShadow: "0 4px 16px var(--shadow-msg)",
        animation: phase === "exiting"
          ? "toast-exit var(--duration-slow) var(--ease-spring) forwards"
          : "toast-enter var(--duration-slow) var(--ease-spring) both",
      }}
    >
      {isConnected ? (
        <>
          <span
            className="inline-block w-2 h-2 rounded-full"
            style={{ background: "var(--success)" }}
          />
          <span className="text-[var(--text-primary)]">Connected</span>
        </>
      ) : (
        <>
          <svg
            className="animate-spin"
            width="14"
            height="14"
            viewBox="0 0 14 14"
            fill="none"
            aria-hidden="true"
          >
            <circle
              cx="7" cy="7" r="5.5"
              stroke="var(--border)"
              strokeWidth="2"
            />
            <path
              d="M12.5 7a5.5 5.5 0 00-5.5-5.5"
              stroke="var(--error)"
              strokeWidth="2"
              strokeLinecap="round"
            />
          </svg>
          <span className="text-[var(--text-primary)]">
            Reconnecting&hellip;
          </span>
        </>
      )}
    </div>
  );
}
