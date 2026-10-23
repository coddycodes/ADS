// @vitest-environment happy-dom

/**
 * Which interrupt-only producer failures reach the attached terminal's error.
 *
 * The attach producer is a stream atom. A stream atom publishes a failure only while its
 * lifetime is live, so the one interrupt-only failure that reaches the terminal is the
 * producer's own stream ending interrupted: the RPC client does that to a closing
 * session's requests, and the server when it shuts the session down. Reattaching, or
 * switching or closing the terminal, disposes the old lifetime before interrupting its
 * fiber, so no failure is published.
 */
import { EMPTY_TERMINAL_ATTACH_SNAPSHOT } from "@bibcode/client-runtime/state/terminal";
import { EnvironmentId, ThreadId, type TerminalAttachInput } from "@bibcode/contracts";
import { RegistryContext } from "@effect/atom-react";
import { AsyncResult, Atom, AtomRegistry } from "effect/reactivity";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  /** Per terminal: the producer's pending read, and the producer atom that awaits it. */
  producers: new Map<string, { readonly atom: unknown; readonly sessionClosed: () => void }>(),
  /** How many times each terminal's producer read started. */
  starts: new Map<string, number>(),
  snapshot: null as unknown,
  metadata: null as unknown,
}));

vi.mock("./terminal", async () => {
  const Cause = await import("effect/Cause");
  const Deferred = await import("effect/Deferred");
  const Effect = await import("effect/Effect");
  const Stream = await import("effect/Stream");
  const { Atom } = await import("effect/reactivity");
  const producerFor = (terminalId: string) => {
    const existing = harness.producers.get(terminalId);
    if (existing !== undefined) return existing;
    const pending = Deferred.makeUnsafe<void>();
    // The RPC client resumes a closing session's pending stream with an interrupt: the
    // stream ends with an interrupt-only cause while its atom is still mounted.
    const atom = Atom.make(
      Stream.fromEffect(
        Effect.sync(() =>
          harness.starts.set(terminalId, (harness.starts.get(terminalId) ?? 0) + 1),
        ).pipe(
          Effect.andThen(Deferred.await(pending)),
          Effect.andThen(Effect.failCause(Cause.interrupt())),
        ),
      ),
    );
    const producer = { atom, sessionClosed: () => Deferred.doneUnsafe(pending, Effect.void) };
    harness.producers.set(terminalId, producer);
    return producer;
  };
  return {
    terminalEnvironment: {
      attachProducer: ({ input }: { input: { terminalId: string } }) =>
        producerFor(input.terminalId).atom,
      attachSnapshot: () => harness.snapshot,
      metadata: () => harness.metadata,
    },
  };
});

import { useAttachedTerminalSession } from "./terminalSessions";

const environmentId = EnvironmentId.make("environment-terminal-producer");
const threadId = ThreadId.make("thread-terminal-producer");
const DROPPED = "The connection dropped before the result arrived.";

function TerminalProbe({ terminalId }: { readonly terminalId: string }) {
  const session = useAttachedTerminalSession({
    environmentId,
    terminal: { threadId, terminalId } as TerminalAttachInput,
  });
  return (
    <>
      <output>{session.error ?? "no error"}</output>
      <button type="button" onClick={session.reattach}>
        Reattach
      </button>
    </>
  );
}

let container: HTMLDivElement;
let root: Root;
let registry: AtomRegistry.AtomRegistry;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  harness.producers.clear();
  harness.starts.clear();
  harness.snapshot = Atom.make(EMPTY_TERMINAL_ATTACH_SNAPSHOT);
  harness.metadata = Atom.make(AsyncResult.success([]));
  registry = AtomRegistry.make();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  registry.dispose();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

async function renderTerminal(terminalId: string): Promise<void> {
  await act(async () =>
    root.render(
      <RegistryContext.Provider value={registry}>
        <TerminalProbe terminalId={terminalId} />
      </RegistryContext.Provider>,
    ),
  );
}

const shownError = () => container.querySelector("output")?.textContent;

describe("attached terminal producer interrupts", () => {
  it("names a producer interrupted by its closing session as a dropped connection", async () => {
    await renderTerminal("1");
    expect(shownError()).toBe("no error");

    await act(async () => harness.producers.get("1")!.sessionClosed());
    await vi.waitFor(() => expect(shownError()).toBe(DROPPED));
  });

  it("shows no error when a reattach, a switch or a close interrupts the producer", async () => {
    await renderTerminal("1");
    // Reattach refreshes the producer, interrupting the running read and starting another.
    await act(async () => container.querySelector("button")!.click());
    await vi.waitFor(() => expect(harness.starts.get("1")).toBe(2));
    await act(async () => {});
    expect(shownError()).toBe("no error");
    // Switching disposes the first terminal's producer; closing disposes the second's.
    await renderTerminal("2");
    await act(async () => root.render(null));
    await act(async () => new Promise((resolve) => setTimeout(resolve, 20)));
    await renderTerminal("2");
    await vi.waitFor(() => expect(harness.starts.get("2")).toBe(2));
    expect(shownError()).toBe("no error");
  });
});
