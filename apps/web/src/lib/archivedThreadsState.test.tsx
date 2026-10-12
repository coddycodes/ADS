// @vitest-environment happy-dom

import type { EnvironmentId } from "@bibcode/contracts";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  started: 0,
  inFlight: 0,
  maxInFlight: 0,
  release: [] as Array<() => void>,
}));

// Each read of the archived snapshot is a request that stays in flight until released.
vi.mock("../state/orchestration", async () => {
  const Effect = await import("effect/Effect");
  const { Atom } = await import("effect/unstable/reactivity");
  const request = Effect.suspend(() => {
    h.started += 1;
    h.inFlight += 1;
    h.maxInFlight = Math.max(h.maxInFlight, h.inFlight);
    return Effect.promise(
      () =>
        new Promise((resolve) => {
          h.release.push(() =>
            resolve({ snapshotSequence: h.started, projects: [], threads: [], updatedAt: "" }),
          );
        }),
    ).pipe(
      Effect.ensuring(
        Effect.sync(() => {
          h.inFlight -= 1;
        }),
      ),
    );
  });
  const family = Atom.family((_environmentId: string) => Atom.make(request));
  return {
    orchestrationEnvironment: {
      archivedShellSnapshot: ({ environmentId }: { environmentId: string }) =>
        family(environmentId),
    },
  };
});

import { AppAtomRegistryProvider, resetAppAtomRegistryForTests } from "../rpc/atomRegistry";
import { useArchivedThreadSnapshots } from "./archivedThreadsState";

const ENVIRONMENT_IDS: ReadonlyArray<EnvironmentId> = ["primary" as EnvironmentId];

/** A consumer; a fresh but equal environment list on every render, as callers may pass. */
function ArchivedList(props: { readonly optOut?: boolean }) {
  const ids = [...ENVIRONMENT_IDS];
  useArchivedThreadSnapshots(ids, props.optOut ? { revalidateOnMount: false } : undefined);
  return null;
}

let container: HTMLDivElement;
let root: Root;

/** Renders `keepers` opted-out consumers (like the lifecycle reconciler) and `lists` default ones. */
function render(consumers: { readonly keepers: number; readonly lists: number }): void {
  act(() => {
    root.render(
      <AppAtomRegistryProvider>
        {Array.from({ length: consumers.keepers }, (_, index) => (
          <ArchivedList key={`keeper-${index}`} optOut />
        ))}
        {Array.from({ length: consumers.lists }, (_, index) => (
          <ArchivedList key={`list-${index}`} />
        ))}
      </AppAtomRegistryProvider>,
    );
  });
}

async function settle(): Promise<void> {
  await act(async () => {
    for (const release of h.release.splice(0)) release();
    await Promise.resolve();
  });
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  resetAppAtomRegistryForTests();
  h.started = 0;
  h.inFlight = 0;
  h.maxInFlight = 0;
  h.release = [];
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => {
    root.unmount();
  });
  container.remove();
});

describe("useArchivedThreadSnapshots", () => {
  it("refetches once when a list opens over a settled snapshot", async () => {
    // The lifecycle reconciler keeps the snapshot loaded and opts out of mount refetches.
    render({ keepers: 1, lists: 0 });
    await settle();
    expect(h.started).toBe(1);

    render({ keepers: 1, lists: 1 });
    expect(h.started).toBe(2);
    await settle();
    // Re-rendering the open list does not refetch.
    render({ keepers: 1, lists: 1 });
    render({ keepers: 1, lists: 1 });
    expect(h.started).toBe(2);
    expect(h.maxInFlight).toBe(1);
  });

  it("never refetches for consumers that opt out", async () => {
    render({ keepers: 1, lists: 0 });
    await settle();
    render({ keepers: 2, lists: 0 });
    render({ keepers: 2, lists: 0 });
    expect(h.started).toBe(1);
  });

  it("joins a request already in flight instead of starting a second one", async () => {
    render({ keepers: 1, lists: 0 });
    expect(h.inFlight).toBe(1);

    // The Settings list and the reopen menu open while the first request runs.
    render({ keepers: 1, lists: 2 });
    expect(h.started).toBe(1);
    await settle();
    expect(h.started).toBe(1);
    expect(h.maxInFlight).toBe(1);
  });

  it("starts one request when two lists open together over a settled snapshot", async () => {
    render({ keepers: 1, lists: 0 });
    await settle();

    render({ keepers: 1, lists: 2 });
    expect(h.started).toBe(2);
    expect(h.maxInFlight).toBe(1);
  });
});
