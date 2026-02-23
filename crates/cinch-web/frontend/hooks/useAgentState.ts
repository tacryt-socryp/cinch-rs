"use client";

import { createContext, useContext } from "react";
import type { AgentState, QuestionResponse } from "@/lib/types";
import { INITIAL_STATE } from "@/lib/types";

export interface AgentContextValue {
  state: AgentState;
  connected: boolean;
  sendAnswer: (response: QuestionResponse) => void;
  sendChat: (message: string) => void;
  sendQuit: () => void;
}

const noop = (): void => { /* no-op default */ };

export const AgentContext = createContext<AgentContextValue>({
  state: INITIAL_STATE,
  connected: false,
  sendAnswer: noop,
  sendChat: noop,
  sendQuit: noop,
});

/**
 * Read the agent state from the nearest AgentContext provider.
 *
 * All chat UI components use this hook instead of accepting props,
 * keeping the component tree clean.
 */
export function useAgentState(): AgentContextValue {
  return useContext(AgentContext);
}
