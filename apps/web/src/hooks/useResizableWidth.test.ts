// @vitest-environment happy-dom

import type { PointerEvent as ReactPointerEvent } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const hooks = vi.hoisted(() => {
  let cursor = 0;
  let stateSlots = new Map<number, unknown>();
  let refSlots = new Map<number, { current: unknown }>();

  return {
    beginRender(): void {
      cursor = 0;
    },
    reset(): void {
      cursor = 0;
      stateSlots = new Map();
      refSlots = new Map();
    },
    useCallback<T>(callback: T): T {
      cursor += 1;
      return callback;
    },
    useRef<T>(initialValue: T): { current: T } {
      const index = cursor;
      cursor += 1;
      const existing = refSlots.get(index);
      if (existing !== undefined) {
        return existing as { current: T };
      }
      const ref = { current: initialValue };
      refSlots.set(index, ref);
      return ref;
    },
    useState<T>(initialValue: T | (() => T)): [T, (next: T | ((value: T) => T)) => void] {
      const index = cursor;
      cursor += 1;
      if (!stateSlots.has(index)) {
        stateSlots.set(
          index,
          typeof initialValue === "function" ? (initialValue as () => T)() : initialValue,
        );
      }
      return [
        stateSlots.get(index) as T,
        (next) => {
          const previous = stateSlots.get(index) as T;
          stateSlots.set(
            index,
            typeof next === "function" ? (next as (value: T) => T)(previous) : next,
          );
        },
      ];
    },
  };
});

const storage = vi.hoisted(() => ({
  getError: null as unknown,
  setError: null as unknown,
  stored: null as number | null,
  writes: [] as Array<{ key: string; value: number; schema: unknown }>,
}));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  return {
    ...actual,
    useCallback: hooks.useCallback,
    useRef: hooks.useRef,
    useState: hooks.useState,
  };
});

vi.mock("./useLocalStorage", () => ({
  getLocalStorageItem: () => {
    if (storage.getError !== null) {
      throw storage.getError;
    }
    return storage.stored;
  },
  setLocalStorageItem: (key: string, value: number, schema: unknown) => {
    if (storage.setError !== null) {
      throw storage.setError;
    }
    storage.writes.push({ key, value, schema });
  },
}));

import {
  type ResizableWidthHandlers,
  type UseResizableWidthOptions,
  useResizableWidth,
} from "./useResizableWidth";

interface AnimationFrames {
  readonly callbacks: Map<number, FrameRequestCallback>;
  readonly cancelled: number[];
  run(id: number): void;
}

interface PointerTarget {
  readonly element: HTMLElement;
  readonly setPointerCapture: ReturnType<typeof vi.fn>;
  readonly hasPointerCapture: ReturnType<typeof vi.fn>;
  readonly releasePointerCapture: ReturnType<typeof vi.fn>;
}

function installAnimationFrames(): AnimationFrames {
  let nextId = 1;
  const callbacks = new Map<number, FrameRequestCallback>();
  const cancelled: number[] = [];
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
    const id = nextId;
    nextId += 1;
    callbacks.set(id, callback);
    return id;
  });
  vi.stubGlobal("cancelAnimationFrame", (id: number) => {
    cancelled.push(id);
    callbacks.delete(id);
  });
  return {
    callbacks,
    cancelled,
    run(id) {
      const callback = callbacks.get(id);
      if (callback === undefined) {
        throw new Error(`Animation frame ${id} is not pending.`);
      }
      callbacks.delete(id);
      callback(0);
    },
  };
}

function pointerTarget(
  overrides: {
    captureError?: Error;
    captured?: boolean;
    releaseError?: Error;
  } = {},
): PointerTarget {
  const element = document.createElement("div");
  const setPointerCapture = vi.fn((_pointerId: number) => {
    if (overrides.captureError) throw overrides.captureError;
  });
  const hasPointerCapture = vi.fn((_pointerId: number) => overrides.captured ?? true);
  const releasePointerCapture = vi.fn((_pointerId: number) => {
    if (overrides.releaseError) throw overrides.releaseError;
  });
  Object.assign(element, { setPointerCapture, hasPointerCapture, releasePointerCapture });
  return { element, setPointerCapture, hasPointerCapture, releasePointerCapture };
}

function pointerEvent(
  target: HTMLElement,
  overrides: Partial<{
    button: number;
    clientX: number;
    pointerId: number;
  }> = {},
): ReactPointerEvent<HTMLElement> {
  return {
    button: 0,
    clientX: 100,
    pointerId: 7,
    currentTarget: target,
    preventDefault: vi.fn(),
    stopPropagation: vi.fn(),
    ...overrides,
  } as unknown as ReactPointerEvent<HTMLElement>;
}

const baseOptions: UseResizableWidthOptions = {
  storageKey: "panel-width",
  defaultWidth: 320,
  minWidth: 200,
  maxWidth: 600,
  edge: "right",
};

function renderHook(overrides: Partial<UseResizableWidthOptions> = {}): {
  readonly width: number;
  readonly handlers: ResizableWidthHandlers;
} {
  hooks.beginRender();
  return useResizableWidth({ ...baseOptions, ...overrides });
}

function beginDrag(
  handlers: ResizableWidthHandlers,
  target: PointerTarget = pointerTarget(),
  eventOverrides: Partial<{ button: number; clientX: number; pointerId: number }> = {},
): PointerTarget {
  handlers.onPointerDown(pointerEvent(target.element, eventOverrides));
  return target;
}

beforeEach(() => {
  hooks.reset();
  storage.getError = null;
  storage.setError = null;
  storage.stored = null;
  storage.writes = [];
  document.body.style.removeProperty("cursor");
  document.body.style.removeProperty("user-select");
  installAnimationFrames();
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("useResizableWidth initialization", () => {
  it("uses the default width when rendered without a window", () => {
    vi.stubGlobal("window", undefined);

    expect(renderHook().width).toBe(320);
  });

  it.each([
    [450, 450],
    [100, 200],
    [900, 600],
    [Number.NaN, 320],
  ] as const)("clamps stored width %s to %s", (stored, expected) => {
    storage.stored = stored;

    expect(renderHook().width).toBe(expected);
  });

  it("falls back and reports storage read failures", () => {
    const error = new Error("storage unavailable");
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    storage.getError = error;

    expect(renderHook().width).toBe(320);
    expect(consoleError).toHaveBeenCalledWith("Could not read persisted panel width.", error);
  });
});

describe("useResizableWidth pointer handling", () => {
  it("ignores non-primary buttons", () => {
    const result = renderHook();
    const target = beginDrag(result.handlers, pointerTarget(), { button: 1 });

    expect(target.setPointerCapture).not.toHaveBeenCalled();
    expect(document.body.style.cursor).toBe("");
  });

  it("stops when pointer capture fails", () => {
    const result = renderHook();
    const target = pointerTarget({ captureError: new Error("capture unavailable") });
    const event = pointerEvent(target.element);

    result.handlers.onPointerDown(event);

    expect(event.preventDefault).toHaveBeenCalledTimes(1);
    expect(event.stopPropagation).toHaveBeenCalledTimes(1);
    expect(document.body.style.cursor).toBe("");
  });

  it.each([
    ["right", 145, 365],
    ["left", 55, 365],
    ["right", -500, 200],
    ["right", 900, 600],
  ] as const)("resizes from the %s edge to %s", (edge, clientX, expected) => {
    const frames = installAnimationFrames();
    const first = renderHook({ edge });
    const target = beginDrag(first.handlers);
    const move = pointerEvent(target.element, { clientX });

    first.handlers.onPointerMove(move);
    first.handlers.onPointerMove(pointerEvent(target.element, { clientX }));

    expect(move.preventDefault).toHaveBeenCalledTimes(1);
    expect(frames.callbacks.size).toBe(1);
    frames.run([...frames.callbacks.keys()][0]!);
    expect(renderHook({ edge }).width).toBe(expected);
  });

  it("ignores moves and releases from a different pointer", () => {
    const frames = installAnimationFrames();
    const result = renderHook();
    const target = beginDrag(result.handlers);

    result.handlers.onPointerMove(pointerEvent(target.element, { pointerId: 8 }));
    result.handlers.onPointerUp(pointerEvent(target.element, { pointerId: 8 }));
    result.handlers.onPointerCancel(pointerEvent(target.element, { pointerId: 8 }));

    expect(frames.callbacks.size).toBe(0);
    expect(storage.writes).toEqual([]);
    expect(document.body.style.cursor).toBe("col-resize");
  });

  it("persists once on pointer up and releases captured state", () => {
    const frames = installAnimationFrames();
    const first = renderHook();
    const target = beginDrag(first.handlers);
    first.handlers.onPointerMove(pointerEvent(target.element, { clientX: 180 }));
    const frameId = [...frames.callbacks.keys()][0]!;

    first.handlers.onPointerUp(pointerEvent(target.element));

    expect(frames.cancelled).toEqual([frameId]);
    expect(target.hasPointerCapture).toHaveBeenCalledWith(7);
    expect(target.releasePointerCapture).toHaveBeenCalledWith(7);
    expect(document.body.style.cursor).toBe("");
    expect(document.body.style.userSelect).toBe("");
    expect(storage.writes).toHaveLength(1);
    expect(storage.writes[0]).toMatchObject({ key: "panel-width", value: 400 });
    expect(renderHook().width).toBe(400);
  });

  it("rolls a cancelled drag back without persisting", () => {
    const frames = installAnimationFrames();
    storage.stored = 410;
    const first = renderHook();
    const target = beginDrag(first.handlers);
    first.handlers.onPointerMove(pointerEvent(target.element, { clientX: 170 }));
    const frameId = [...frames.callbacks.keys()][0]!;

    first.handlers.onPointerCancel(pointerEvent(target.element));

    expect(frames.cancelled).toEqual([frameId]);
    expect(storage.writes).toEqual([]);
    expect(renderHook().width).toBe(410);
  });

  it("tolerates already-released and throwing pointer capture state", () => {
    const noCapture = pointerTarget({ captured: false });
    const first = renderHook();
    beginDrag(first.handlers, noCapture);
    first.handlers.onPointerUp(pointerEvent(noCapture.element));
    expect(noCapture.releasePointerCapture).not.toHaveBeenCalled();

    hooks.reset();
    const releaseError = pointerTarget({ releaseError: new Error("already released") });
    const second = renderHook();
    beginDrag(second.handlers, releaseError);
    expect(() => second.handlers.onPointerUp(pointerEvent(releaseError.element))).not.toThrow();
  });

  it("reports persistence failures but keeps the final width", () => {
    const error = new Error("quota exceeded");
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    storage.setError = error;
    const first = renderHook();
    const target = beginDrag(first.handlers);
    first.handlers.onPointerMove(pointerEvent(target.element, { clientX: 130 }));

    first.handlers.onPointerUp(pointerEvent(target.element));

    expect(consoleError).toHaveBeenCalledWith("Could not persist panel width.", error);
    expect(renderHook().width).toBe(350);
  });

  it("drops an animation callback after the drag is released", () => {
    const frames = installAnimationFrames();
    const first = renderHook();
    const target = beginDrag(first.handlers);
    first.handlers.onPointerMove(pointerEvent(target.element, { clientX: 140 }));
    const callback = frames.callbacks.get([...frames.callbacks.keys()][0]!)!;
    first.handlers.onPointerCancel(pointerEvent(target.element));

    expect(() => callback(0)).not.toThrow();
    expect(renderHook().width).toBe(320);
  });
});
