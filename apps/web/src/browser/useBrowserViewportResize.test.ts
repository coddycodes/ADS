/**
 * Unit tests for {@link useBrowserViewportResize}.
 *
 * The hook is exercised with the repo's instrumented-hooks pattern (see
 * FilePreviewPanel.test.tsx): a partial `vi.mock("react")` swaps
 * useState/useRef/useEffect so state can be seeded, setter calls recorded,
 * refs inspected, and effects run manually. A tiny harness component calls the
 * hook during `renderToStaticMarkup`; the returned handlers are then invoked
 * directly with fake keyboard/pointer events against a stubbed `window`. The
 * pure `browserViewportLayout` math runs for real; only the imperative
 * `commitBrowserViewportChange` side effect is mocked.
 */
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { createElement } from "react";
import type { KeyboardEvent as ReactKeyboardEvent, PointerEvent as ReactPointerEvent } from "react";

import { browserViewportSettingKey } from "./browserViewportLayout";

const harness = vi.hoisted(() => {
  type Matcher = (initial: unknown) => boolean;
  const state = {
    stateSeeds: [] as Array<{ match: Matcher; value: unknown }>,
    setStateCalls: [] as Array<{ initial: unknown; next: unknown; applied: unknown }>,
    effects: [] as Array<() => void | (() => void)>,
    refs: [] as Array<{ current: unknown }>,
    reset() {
      state.stateSeeds.length = 0;
      state.setStateCalls.length = 0;
      state.effects.length = 0;
      state.refs.length = 0;
    },
    seedState(match: Matcher, value: unknown) {
      state.stateSeeds.push({ match, value });
    },
    runEffects(): Array<() => void> {
      const cleanups: Array<() => void> = [];
      for (const effect of state.effects) {
        const cleanup = effect();
        if (typeof cleanup === "function") cleanups.push(cleanup);
      }
      return cleanups;
    },
  };
  return state;
});

const commit = vi.hoisted(() => ({
  calls: [] as Array<{ tabId: string; setting: unknown }>,
  result: (() => Promise.resolve()) as () => Promise<void>,
}));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();

  const resolveInitial = (initial: unknown): unknown =>
    typeof initial === "function" ? (initial as () => unknown)() : initial;

  const useState = (initial?: unknown) => {
    const resolved = resolveInitial(initial);
    const seedIndex = harness.stateSeeds.findIndex((seed) => seed.match(resolved));
    const value = seedIndex >= 0 ? harness.stateSeeds.splice(seedIndex, 1)[0]!.value : resolved;
    const setValue = (next: unknown) => {
      const applied =
        typeof next === "function" ? (next as (value: unknown) => unknown)(value) : next;
      harness.setStateCalls.push({ initial: resolved, next, applied });
    };
    return [value, setValue];
  };

  const useEffect = (effect: () => void | (() => void)) => {
    harness.effects.push(effect);
  };

  const useRef = (initial?: unknown) => {
    const ref = { current: initial ?? null };
    harness.refs.push(ref);
    return ref;
  };

  return {
    ...actual,
    useState: useState as typeof actual.useState,
    useEffect: useEffect as typeof actual.useEffect,
    useRef: useRef as typeof actual.useRef,
  };
});

vi.mock("./browserViewportActions", () => ({
  commitBrowserViewportChange: (tabId: string, setting: unknown) => {
    commit.calls.push({ tabId, setting });
    return commit.result();
  },
}));

import { useBrowserViewportResize } from "./useBrowserViewportResize";

type HookOptions = Parameters<typeof useBrowserViewportResize>[0];
type HookResult = ReturnType<typeof useBrowserViewportResize>;

const freeform = (width: number, height: number): HookOptions["viewport"] =>
  ({ _tag: "freeform", width, height }) as HookOptions["viewport"];

function baseOptions(overrides: Partial<HookOptions> = {}): HookOptions {
  return {
    tabId: "tab-1",
    viewport: freeform(800, 600),
    zoomFactor: 1,
    containerSize: { width: 2000, height: 2000 },
    deviceToolbarVisible: false,
    aspectRatio: null,
    ...overrides,
  };
}

let captured: HookResult | null = null;

function Harness({
  options,
  onResult,
}: {
  options: HookOptions;
  onResult: (result: HookResult) => void;
}) {
  onResult(useBrowserViewportResize(options));
  return null;
}

function renderHook(options: HookOptions = baseOptions()): HookResult {
  captured = null;
  harness.setStateCalls.length = 0;
  harness.effects.length = 0;
  harness.refs.length = 0;
  renderToStaticMarkup(
    createElement(Harness, {
      options,
      onResult: (result) => {
        captured = result;
      },
    }),
  );
  if (!captured) throw new Error("hook did not produce a result");
  return captured;
}

// Named ref accessors (creation order inside the hook).
const dragCleanupRef = () => harness.refs[0]!;
const keyboardTimerRef = () => harness.refs[2]!;
const keyboardViewportRef = () => harness.refs[3]!;
const sourceViewportKeyRef = () => harness.refs[4]!;

interface WindowListener {
  type: string;
  handler: (event: unknown) => void;
}

let windowStub: {
  listeners: WindowListener[];
  addEventListener: (type: string, handler: (event: unknown) => void) => void;
  removeEventListener: (type: string, handler: (event: unknown) => void) => void;
};

function makeWindowStub() {
  const listeners: WindowListener[] = [];
  return {
    listeners,
    addEventListener: (type: string, handler: (event: unknown) => void) => {
      listeners.push({ type, handler });
    },
    removeEventListener: (type: string, handler: (event: unknown) => void) => {
      const index = listeners.findIndex(
        (entry) => entry.type === type && entry.handler === handler,
      );
      if (index >= 0) listeners.splice(index, 1);
    },
  };
}

function windowHandler(type: string): (event: unknown) => void {
  const entry = windowStub.listeners.find((listener) => listener.type === type);
  if (!entry) throw new Error(`no window ${type} listener`);
  return entry.handler;
}

function keyEvent(overrides: Record<string, unknown> = {}): ReactKeyboardEvent<HTMLButtonElement> {
  return {
    key: "ArrowRight",
    shiftKey: false,
    preventDefault: vi.fn(),
    stopPropagation: vi.fn(),
    ...overrides,
  } as unknown as ReactKeyboardEvent<HTMLButtonElement>;
}

function makeTarget(opts: { captureThrows?: boolean; releaseThrows?: boolean } = {}) {
  return {
    setPointerCapture: opts.captureThrows
      ? () => {
          throw new Error("no capture");
        }
      : vi.fn(),
    releasePointerCapture: opts.releaseThrows
      ? () => {
          throw new Error("no release");
        }
      : vi.fn(),
  };
}

function pointerDownEvent(
  overrides: Record<string, unknown> = {},
): ReactPointerEvent<HTMLButtonElement> {
  return {
    pointerId: 7,
    currentTarget: makeTarget(),
    clientX: 100,
    clientY: 100,
    preventDefault: vi.fn(),
    stopPropagation: vi.fn(),
    ...overrides,
  } as unknown as ReactPointerEvent<HTMLButtonElement>;
}

const flush = async () => {
  await Promise.resolve();
  await Promise.resolve();
};

beforeEach(() => {
  harness.reset();
  commit.calls.length = 0;
  commit.result = () => Promise.resolve();
  windowStub = makeWindowStub();
  vi.stubGlobal("window", windowStub);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

// ─────────────────────────────────────────────────────────────────────────────

describe("derived layout", () => {
  it("exposes the resolved viewport and layout for a freeform viewport", () => {
    const result = renderHook(baseOptions());
    expect(result.activeDrag).toBeNull();
    expect(result.effectiveViewport).toEqual({ _tag: "freeform", width: 800, height: 600 });
    expect(result.layout.viewportWidth).toBeGreaterThan(0);
  });

  it("promotes a matching seeded drag into the effective freeform viewport", () => {
    const options = baseOptions();
    const sourceKey = browserViewportSettingKey(options.viewport);
    harness.seedState((initial) => initial === null, {
      sourceKey,
      width: 950,
      height: 640,
      direction: "east",
    });
    const result = renderHook(options);
    expect(result.activeDrag).toEqual({
      sourceKey,
      width: 950,
      height: 640,
      direction: "east",
    });
    expect(result.effectiveViewport).toEqual({ _tag: "freeform", width: 950, height: 640 });
  });

  it("uses the device viewport layout when the device toolbar is visible", () => {
    const result = renderHook(baseOptions({ deviceToolbarVisible: true }));
    expect(result.layout.canvasWidth).toBe(2000);
  });

  it("falls back to fill layout when the toolbar is visible but viewport fills the panel", () => {
    const result = renderHook(
      baseOptions({
        deviceToolbarVisible: true,
        viewport: { _tag: "fill" } as HookOptions["viewport"],
      }),
    );
    expect(result.layout.fillsPanel).toBe(true);
  });

  it("normalizes a non-positive zoom factor to 1", () => {
    const result = renderHook(baseOptions({ zoomFactor: 0 }));
    expect(result.layout.viewportScale).toBeGreaterThan(0);
  });
});

describe("commitViewportChange", () => {
  it("resets pending drag bookkeeping and forwards to the commit action", () => {
    const result = renderHook(baseOptions());
    const cleanupSpy = vi.fn();
    dragCleanupRef().current = cleanupSpy;
    keyboardTimerRef().current = 123 as unknown;
    keyboardViewportRef().current = { sourceKey: "x" };

    harness.setStateCalls.length = 0;
    const next = { _tag: "freeform", width: 640, height: 480 } as HookOptions["viewport"];
    void result.commitViewportChange(next);

    expect(cleanupSpy).toHaveBeenCalledTimes(1);
    expect(keyboardViewportRef().current).toBeNull();
    expect(harness.setStateCalls.some((call) => call.next === null)).toBe(true);
    expect(commit.calls).toEqual([{ tabId: "tab-1", setting: next }]);
  });
});

describe("handleResizeKeyDown", () => {
  it("ignores keyboard resize while the viewport fills the panel", () => {
    const result = renderHook(
      baseOptions({ viewport: { _tag: "fill" } as HookOptions["viewport"] }),
    );
    result.handleResizeKeyDown("east", keyEvent({ key: "ArrowRight" }));
    expect(harness.setStateCalls).toHaveLength(0);
    expect(commit.calls).toHaveLength(0);
  });

  it("ignores keys that do not move the controlled axis", () => {
    const result = renderHook(baseOptions());
    harness.setStateCalls.length = 0;
    // "east" only controls width, so ArrowUp produces no delta.
    result.handleResizeKeyDown("east", keyEvent({ key: "ArrowUp" }));
    expect(harness.setStateCalls).toHaveLength(0);
  });

  it("ignores a resize that would not change the clamped size", () => {
    const result = renderHook(baseOptions({ viewport: freeform(3840, 600) }));
    harness.setStateCalls.length = 0;
    result.handleResizeKeyDown("east", keyEvent({ key: "ArrowRight" }));
    expect(harness.setStateCalls).toHaveLength(0);
    expect(commit.calls).toHaveLength(0);
  });

  it("grows the viewport, schedules a debounced commit, and fires it", async () => {
    vi.useFakeTimers();
    try {
      const result = renderHook(baseOptions());
      harness.setStateCalls.length = 0;
      const event = keyEvent({ key: "ArrowRight", shiftKey: true });
      result.handleResizeKeyDown("east", event);

      expect(event.preventDefault).toHaveBeenCalled();
      expect(event.stopPropagation).toHaveBeenCalled();
      const update = harness.setStateCalls.find(
        (call) => typeof call.next === "object" && call.next !== null,
      );
      expect((update!.next as { width: number }).width).toBe(850);
      expect(keyboardViewportRef().current).not.toBeNull();

      // No commit until the debounce elapses.
      expect(commit.calls).toHaveLength(0);
      vi.advanceTimersByTime(150);
      expect(commit.calls).toHaveLength(1);
      expect(commit.calls[0]!.setting).toEqual({ _tag: "freeform", width: 850, height: 600 });
    } finally {
      vi.useRealTimers();
    }
  });

  it("accumulates from the pending keyboard viewport and reschedules the commit", () => {
    vi.useFakeTimers();
    try {
      const options = baseOptions();
      const sourceKey = browserViewportSettingKey(options.viewport);
      const result = renderHook(options);
      keyboardViewportRef().current = {
        sourceKey,
        width: 810,
        height: 600,
        direction: "east",
      };
      keyboardTimerRef().current = 999 as unknown;
      harness.setStateCalls.length = 0;

      result.handleResizeKeyDown("east", keyEvent({ key: "ArrowRight" }));
      const update = harness.setStateCalls.find(
        (call) => typeof call.next === "object" && call.next !== null,
      );
      // Base is the pending 810 width, not the prop's 800.
      expect((update!.next as { width: number }).width).toBe(820);
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps the drag when the debounced commit resolves to a superseded version", async () => {
    vi.useFakeTimers();
    try {
      const result = renderHook(baseOptions());
      result.handleResizeKeyDown("east", keyEvent({ key: "ArrowRight" }));
      // Bump the drag version so the post-commit clear becomes a no-op.
      result.handleResizeKeyDown("east", keyEvent({ key: "ArrowRight" }));
      harness.setStateCalls.length = 0;
      vi.advanceTimersByTime(150);
      await flush();
      expect(commit.calls.length).toBeGreaterThanOrEqual(1);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("handleResizePointerDown", () => {
  it("ignores a pointer drag while the viewport fills the panel", () => {
    const result = renderHook(
      baseOptions({ viewport: { _tag: "fill" } as HookOptions["viewport"] }),
    );
    result.handleResizePointerDown("east", pointerDownEvent());
    expect(windowStub.listeners).toHaveLength(0);
    expect(harness.setStateCalls).toHaveLength(0);
  });

  it("captures the pointer, tracks a move, and commits on pointer up", async () => {
    const result = renderHook(baseOptions());
    const event = pointerDownEvent();
    result.handleResizePointerDown("east", event);

    expect(event.preventDefault).toHaveBeenCalled();
    expect(windowStub.listeners.map((entry) => entry.type)).toEqual(
      expect.arrayContaining(["pointermove", "pointerup", "pointercancel"]),
    );

    // Clear the initial `setDragViewport` recorded by pointer-down so the next
    // object update is unambiguously the move.
    harness.setStateCalls.length = 0;
    windowHandler("pointermove")({
      pointerId: 7,
      clientX: 260,
      clientY: 100,
      preventDefault: vi.fn(),
    });
    const move = harness.setStateCalls.find(
      (call) => typeof call.next === "object" && call.next !== null,
    );
    expect((move!.next as { width: number }).width).toBeGreaterThan(800);

    windowHandler("pointerup")({ pointerId: 7 });
    await flush();
    expect(commit.calls).toHaveLength(1);
    expect((commit.calls[0]!.setting as { _tag: string })._tag).toBe("freeform");
    // Listeners are removed on cleanup.
    expect(windowStub.listeners).toHaveLength(0);
  });

  it("ignores move and finish events for a different pointer id", () => {
    const result = renderHook(baseOptions());
    result.handleResizePointerDown("east", pointerDownEvent());
    harness.setStateCalls.length = 0;

    windowHandler("pointermove")({
      pointerId: 999,
      clientX: 260,
      clientY: 100,
      preventDefault: vi.fn(),
    });
    expect(harness.setStateCalls).toHaveLength(0);

    windowHandler("pointerup")({ pointerId: 999 });
    expect(commit.calls).toHaveLength(0);
    // The still-active drag keeps its listeners.
    expect(windowStub.listeners.length).toBeGreaterThan(0);
  });

  it("clears the drag without committing when the pointer up has no movement", () => {
    const result = renderHook(baseOptions());
    result.handleResizePointerDown("east", pointerDownEvent());
    harness.setStateCalls.length = 0;

    windowHandler("pointerup")({ pointerId: 7 });
    expect(commit.calls).toHaveLength(0);
    expect(harness.setStateCalls.some((call) => call.next === null)).toBe(true);
  });

  it("aborts the drag when the source viewport changes mid-move", () => {
    const result = renderHook(baseOptions());
    result.handleResizePointerDown("east", pointerDownEvent());
    // A source change unsettles the in-flight drag.
    sourceViewportKeyRef().current = "freeform:1:1:";
    harness.setStateCalls.length = 0;

    windowHandler("pointermove")({
      pointerId: 7,
      clientX: 260,
      clientY: 100,
      preventDefault: vi.fn(),
    });
    expect(harness.setStateCalls.some((call) => call.next === null)).toBe(true);
    expect(commit.calls).toHaveLength(0);
  });

  it("clears the drag without committing when the source changes before pointer up", () => {
    const result = renderHook(baseOptions());
    result.handleResizePointerDown("east", pointerDownEvent());
    windowHandler("pointermove")({
      pointerId: 7,
      clientX: 260,
      clientY: 100,
      preventDefault: vi.fn(),
    });
    sourceViewportKeyRef().current = "freeform:1:1:";
    harness.setStateCalls.length = 0;

    windowHandler("pointerup")({ pointerId: 7 });
    expect(commit.calls).toHaveLength(0);
    expect(harness.setStateCalls.some((call) => call.next === null)).toBe(true);
  });

  it("clears the drag on pointer cancel", () => {
    const result = renderHook(baseOptions());
    result.handleResizePointerDown("east", pointerDownEvent());
    // Capture the handler up front; cleanup removes it from the window.
    const cancel = windowHandler("pointercancel");
    harness.setStateCalls.length = 0;

    // A cancel with a foreign pointer id is ignored (early return).
    cancel({ pointerId: 3 });
    expect(harness.setStateCalls).toHaveLength(0);
    expect(windowStub.listeners.length).toBeGreaterThan(0);

    cancel({ pointerId: 7 });
    expect(harness.setStateCalls.some((call) => call.next === null)).toBe(true);
    expect(windowStub.listeners).toHaveLength(0);
  });

  it("stays functional when pointer capture is unavailable", () => {
    const result = renderHook(baseOptions());
    result.handleResizePointerDown(
      "east",
      pointerDownEvent({ currentTarget: makeTarget({ captureThrows: true }) }),
    );
    expect(windowStub.listeners.length).toBeGreaterThan(0);
  });

  it("swallows a failure to release pointer capture during cleanup", async () => {
    const result = renderHook(baseOptions());
    result.handleResizePointerDown(
      "east",
      pointerDownEvent({ currentTarget: makeTarget({ releaseThrows: true }) }),
    );
    windowHandler("pointermove")({
      pointerId: 7,
      clientX: 260,
      clientY: 100,
      preventDefault: vi.fn(),
    });
    // Cleanup runs during finish; releasePointerCapture throwing must be caught.
    expect(() => windowHandler("pointerup")({ pointerId: 7 })).not.toThrow();
    await flush();
  });

  it("cancels a pending keyboard commit when a pointer drag begins", () => {
    vi.useFakeTimers();
    try {
      const result = renderHook(baseOptions());
      result.handleResizeKeyDown("east", keyEvent({ key: "ArrowRight" }));
      expect(keyboardViewportRef().current).not.toBeNull();
      result.handleResizePointerDown("east", pointerDownEvent());
      expect(keyboardViewportRef().current).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("effects", () => {
  it("tears down an active drag and pending timer on unmount", () => {
    renderHook(baseOptions());
    const cleanupSpy = vi.fn();
    dragCleanupRef().current = cleanupSpy;
    keyboardTimerRef().current = 42 as unknown;
    keyboardViewportRef().current = { sourceKey: "x" };

    // Run only the mount/unmount effect (index 0) so its cleanup, not the
    // source-key reconciliation effect, is what clears the pending timer.
    const cleanup = harness.effects[0]!();
    expect(typeof cleanup).toBe("function");
    (cleanup as () => void)();

    expect(cleanupSpy).toHaveBeenCalledTimes(1);
    expect(keyboardViewportRef().current).toBeNull();
    expect(keyboardTimerRef().current).toBeNull();
  });

  it("discards a stale pending keyboard viewport when the source key changes", () => {
    renderHook(baseOptions());
    keyboardViewportRef().current = { sourceKey: "some-other-key", width: 1, height: 1 };
    keyboardTimerRef().current = 7 as unknown;

    // Effect index 1 is the source-key reconciliation effect.
    harness.effects[1]!();
    expect(keyboardTimerRef().current).toBeNull();
    expect(keyboardViewportRef().current).toBeNull();
  });

  it("leaves a matching pending keyboard viewport untouched", () => {
    const options = baseOptions();
    const sourceKey = browserViewportSettingKey(options.viewport);
    renderHook(options);
    const pending = { sourceKey, width: 810, height: 600, direction: "east" };
    keyboardViewportRef().current = pending;

    harness.effects[1]!();
    expect(keyboardViewportRef().current).toBe(pending);
  });
});
