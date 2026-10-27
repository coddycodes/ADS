import { describe, expect, it } from "vite-plus/test";
import type { GitManagerSignalEvent } from "@coddy/contracts";
import { AsyncResult, Atom, AtomRegistry } from "effect/reactivity";

import { createSignalWithDegradedFocusRefresh } from "./gitManagerRefresh.ts";

function harness(watcherDegraded?: boolean) {
  const signal = Atom.make(
    AsyncResult.success<GitManagerSignalEvent>({
      cwd: "/repo",
      generation: 1,
      ...(watcherDegraded === undefined ? {} : { watcherDegraded }),
    }),
  );
  const activity = Atom.make<AsyncResult.AsyncResult<number>>(AsyncResult.initial<number>());
  const focusRefresh = Atom.make(0);
  const atom = createSignalWithDegradedFocusRefresh(signal, activity, focusRefresh);
  const registry = AtomRegistry.make();
  const unmount = registry.mount(atom);
  let sequence = 0;
  const wake = () => registry.set(activity, AsyncResult.success(++sequence));
  return { signal, atom, registry, unmount, wake, count: () => registry.get(focusRefresh) };
}

describe("Git Manager degraded focus platform port", () => {
  it("invalidates once per application-active wakeup across duplicate consumers", () => {
    const h = harness(true);
    h.registry.mount(h.atom);
    expect(h.count()).toBe(0);
    h.wake();
    expect(h.count()).toBe(1);
    h.wake();
    expect(h.count()).toBe(2);
    h.registry.dispose();
  });

  it.each([false, undefined])(
    "ignores application-active wakeups for healthy or legacy health (%s)",
    (health) => {
      const h = harness(health);
      h.wake();
      expect(h.count()).toBe(0);
      h.registry.dispose();
    },
  );

  it("uses the current signal health without treating health changes as wakeups", () => {
    const h = harness(false);
    h.registry.set(
      h.signal,
      AsyncResult.success({ cwd: "/repo", generation: 1, watcherDegraded: true }),
    );
    expect(h.count()).toBe(0);
    h.wake();
    expect(h.count()).toBe(1);
    h.registry.set(
      h.signal,
      AsyncResult.success({ cwd: "/repo", generation: 2, watcherDegraded: false }),
    );
    h.wake();
    expect(h.count()).toBe(1);
    expect(h.registry.get(h.atom)).toMatchObject({
      _tag: "Success",
      value: { cwd: "/repo", generation: 2, watcherDegraded: false },
    });
    h.registry.dispose();
  });
});
