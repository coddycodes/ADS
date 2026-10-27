import type { ScopedThreadRef } from "@coddy/contracts";
import { create } from "zustand";

import { readPreparedConnection } from "~/state/session";

/** Who asked: an agent's `open`, a command's `$BROWSER`, or the user's own link click. */
export type OpenPromptSource = "agent" | "command" | "link";

export interface OpenPrompt {
  readonly id: string;
  readonly source: OpenPromptSource;
  readonly url: string;
  readonly threadRef: ScopedThreadRef;
  /** The environment's name, as the unreachable notices show it. */
  readonly environmentLabel: string;
  /** The browser blocked the new tab the last attempt opened. */
  readonly blocked: boolean;
  /**
   * How the prompt names the asking thread (its quoted title), set when that
   * thread isn't on screen; Open shows that thread first.
   */
  readonly threadTitle?: string;
}

export const useOpenPromptStore = create<{ prompts: readonly OpenPrompt[] }>(() => ({
  prompts: [],
}));
let nextPromptId = 0;

/** Queues an address that needs a click to open, because this browser tab has no user activation. */
export function enqueueOpenPrompt(prompt: {
  readonly id?: string;
  readonly source: OpenPromptSource;
  readonly url: string;
  readonly threadRef: ScopedThreadRef;
  readonly blocked?: boolean;
  readonly threadTitle?: string | undefined;
}): void {
  const id = prompt.id ?? `prompt-${++nextPromptId}`;
  const queued: OpenPrompt = {
    id,
    source: prompt.source,
    url: prompt.url,
    threadRef: prompt.threadRef,
    environmentLabel:
      readPreparedConnection(prompt.threadRef.environmentId)?.label ?? "this environment",
    blocked: prompt.blocked ?? false,
    ...(prompt.threadTitle === undefined ? {} : { threadTitle: prompt.threadTitle }),
  };
  useOpenPromptStore.setState(({ prompts }) =>
    prompts.some((p) => p.id === id) ? { prompts } : { prompts: [...prompts, queued] },
  );
}

export function removeOpenPrompt(id: string): void {
  useOpenPromptStore.setState(({ prompts }) => ({ prompts: prompts.filter((p) => p.id !== id) }));
}

/** Keeps a prompt whose new tab the browser blocked, so Open can be retried. */
export function markOpenPromptBlocked(id: string): void {
  useOpenPromptStore.setState(({ prompts }) => ({
    prompts: prompts.map((p) => (p.id === id ? { ...p, blocked: true } : p)),
  }));
}

/** Test seam. */
export function resetOpenPromptsForTests(): void {
  useOpenPromptStore.setState({ prompts: [] });
}
