/**
 * Behavior tests for the desktop SSH password prompt dialog.
 *
 * The component has no exported helpers, so we exercise it through the
 * instrumented-hooks pattern (see files/FilePreviewPanel.test.tsx): React's
 * stateful hooks are mocked so state can be seeded per scenario and effects are
 * captured, and the component is invoked as a plain function. Rather than
 * `renderToStaticMarkup` (which would try to run the real dialog/button
 * children), we walk the returned React element tree as data to reach the
 * native `<form onSubmit>`, the `Dialog` `onOpenChange`, and the button/input
 * handlers, then invoke them directly. All host APIs live on `window`
 * (`desktopBridge`, `requestAnimationFrame`, `setInterval`), so a window stub is
 * installed per test.
 */
import type { DesktopSshPasswordPromptRequest } from "@coddy/contracts";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

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
    useId: (() => "ssh-form-id") as typeof actual.useId,
  };
});

import { SshPasswordPromptDialog } from "./SshPasswordPromptDialog";
import { Dialog } from "../ui/dialog";
import { Button } from "../ui/button";
import { Input } from "../ui/input";

const FIXED_NOW = 1_700_000_000_000;
const isEmptyArray = (value: unknown) => Array.isArray(value) && value.length === 0;
const isNumber = (value: unknown) => typeof value === "number";

interface RenderedElement {
  readonly type: unknown;
  readonly props: Record<string, unknown>;
}

function isElement(node: unknown): node is RenderedElement {
  return (
    typeof node === "object" &&
    node !== null &&
    "type" in node &&
    "props" in node &&
    typeof (node as RenderedElement).props === "object"
  );
}

function walk(node: unknown, out: RenderedElement[]): void {
  if (Array.isArray(node)) {
    for (const child of node) walk(child, out);
    return;
  }
  if (!isElement(node)) return;
  out.push(node);
  walk(node.props.children, out);
}

function elementsOf(child: RenderedElement): RenderedElement[] {
  const out: RenderedElement[] = [];
  walk(child, out);
  return out;
}

function makeRequest(
  overrides: Partial<DesktopSshPasswordPromptRequest> = {},
): DesktopSshPasswordPromptRequest {
  return {
    requestId: "req-1",
    destination: "example.com",
    username: "alice",
    prompt: "alice@example.com's password:",
    expiresAt: new Date(FIXED_NOW + 60_000).toISOString(),
    ...overrides,
  };
}

interface WindowStub {
  desktopBridge?: {
    onSshPasswordPrompt?: (
      listener: (request: DesktopSshPasswordPromptRequest) => void,
    ) => () => void;
    resolveSshPasswordPrompt?: (requestId: string, password: string | null) => Promise<void>;
  };
  requestAnimationFrame: (callback: (time: number) => void) => number;
  cancelAnimationFrame: (handle: number) => void;
  setInterval: (callback: () => void, ms: number) => number;
  clearInterval: (handle: number) => void;
}

let windowStub: WindowStub;
let promptListener: ((request: DesktopSshPasswordPromptRequest) => void) | null;
let onSshPasswordPromptUnsub: ReturnType<typeof vi.fn<() => void>>;
let resolveSshPasswordPrompt: ReturnType<
  typeof vi.fn<(requestId: string, password: string | null) => Promise<void>>
>;
let intervalCallbacks: Array<() => void>;
let rafCallbacks: Array<(time: number) => void>;
let cancelledFrames: number[];
let clearedIntervals: number[];

async function flush(): Promise<void> {
  for (let index = 0; index < 5; index += 1) {
    await Promise.resolve();
  }
}

beforeEach(() => {
  harness.reset();
  promptListener = null;
  onSshPasswordPromptUnsub = vi.fn();
  resolveSshPasswordPrompt = vi.fn(() => Promise.resolve());
  intervalCallbacks = [];
  rafCallbacks = [];
  cancelledFrames = [];
  clearedIntervals = [];
  windowStub = {
    desktopBridge: {
      onSshPasswordPrompt: (listener) => {
        promptListener = listener;
        return onSshPasswordPromptUnsub;
      },
      resolveSshPasswordPrompt: (requestId, password) =>
        resolveSshPasswordPrompt(requestId, password),
    },
    requestAnimationFrame: (callback) => {
      rafCallbacks.push(callback);
      return rafCallbacks.length;
    },
    cancelAnimationFrame: (handle) => {
      cancelledFrames.push(handle);
    },
    setInterval: (callback) => {
      intervalCallbacks.push(callback);
      return intervalCallbacks.length;
    },
    clearInterval: (handle) => {
      clearedIntervals.push(handle);
    },
  };
  vi.stubGlobal("window", windowStub);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

/** Invoke the top component; returns its resolved element output (or null). */
function renderTop(): RenderedElement | null {
  const output = (SshPasswordPromptDialog as unknown as () => RenderedElement | null)();
  return output;
}

/**
 * Seed a non-empty queue and render the active prompt, supplying our own
 * `onRemove` spy so removal is observable independently of the parent reducer.
 */
function renderActive(
  request: DesktopSshPasswordPromptRequest,
  onRemove: (requestId: string) => void,
): RenderedElement {
  harness.seedState(isEmptyArray, [request]);
  harness.seedState(isNumber, FIXED_NOW);
  const top = renderTop();
  if (!top) throw new Error("expected an active prompt element");
  const activeType = top.type as (props: unknown) => RenderedElement;
  return activeType({ request, onRemove });
}

function buttonsOf(els: RenderedElement[]): RenderedElement[] {
  return els.filter((element) => element.type === Button);
}

describe("queue subscription", () => {
  it("subscribes to bridge prompts and enqueues incoming requests", () => {
    // Empty queue -> the component renders nothing but still wires the effect.
    harness.seedState(isNumber, FIXED_NOW);
    const output = renderTop();
    expect(output).toBeNull();

    const cleanups = harness.runEffects();
    expect(promptListener).toBeTypeOf("function");

    const request = makeRequest();
    promptListener!(request);
    const enqueue = harness.setStateCalls.find((call) => Array.isArray(call.applied));
    expect(enqueue?.applied).toEqual([request]);

    for (const cleanup of cleanups) cleanup();
    expect(onSshPasswordPromptUnsub).toHaveBeenCalledTimes(1);
  });

  it("is inert when the desktop bridge is unavailable", () => {
    delete windowStub.desktopBridge;
    renderTop();
    const cleanups = harness.runEffects();
    expect(cleanups).toHaveLength(0);
    expect(promptListener).toBeNull();
  });

  it("removes the head request when its id is dismissed and leaves others intact", () => {
    harness.seedState(isEmptyArray, [makeRequest({ requestId: "req-1" })]);
    harness.seedState(isNumber, FIXED_NOW);
    const top = renderTop();
    const onRemove = top!.props.onRemove as (requestId: string) => void;

    onRemove("req-1");
    const removal = harness.setStateCalls.find((call) => typeof call.next === "function");
    expect(removal?.applied).toEqual([]);

    // A non-matching id is a no-op (the queue is returned unchanged).
    harness.setStateCalls.length = 0;
    onRemove("other");
    const noop = harness.setStateCalls.find((call) => typeof call.next === "function");
    expect(noop?.applied).toEqual([makeRequest({ requestId: "req-1" })]);
  });
});

describe("active prompt rendering", () => {
  it("labels the target with the username when present", () => {
    const els = elementsOf(renderActive(makeRequest({ username: "alice" }), vi.fn()));
    const code = els.find((element) => element.type === "code");
    expect(code?.props.children).toBe("alice@example.com");
  });

  it("labels the target with only the destination when the username is absent", () => {
    const els = elementsOf(renderActive(makeRequest({ username: null }), vi.fn()));
    const code = els.find((element) => element.type === "code");
    expect(code?.props.children).toBe("example.com");
  });

  it("shows a Cancel affordance and an enabled input while the prompt is live", () => {
    const els = elementsOf(renderActive(makeRequest(), vi.fn()));
    const [cancel] = buttonsOf(els);
    expect(cancel?.props.children).toBe("Cancel");
    const input = els.find((element) => element.type === Input);
    expect(input?.props.disabled).toBe(false);
  });

  it("shows a Dismiss affordance and disables inputs once expired", () => {
    const els = elementsOf(
      renderActive(makeRequest({ expiresAt: new Date(FIXED_NOW - 1_000).toISOString() }), vi.fn()),
    );
    const [cancel, submit] = buttonsOf(els);
    expect(cancel?.props.children).toBe("Dismiss");
    expect(submit?.props.disabled).toBe(true);
    const input = els.find((element) => element.type === Input);
    expect(input?.props.disabled).toBe(true);
  });

  it("omits the countdown label when the expiry timestamp is unparseable", () => {
    // remainingMs === null path: no timer span is rendered.
    const els = elementsOf(renderActive(makeRequest({ expiresAt: "not-a-date" }), vi.fn()));
    const timerSpan = els.find(
      (element) => element.type === "span" && element.props.children === "Expired",
    );
    expect(timerSpan).toBeUndefined();
  });
});

describe("input handling", () => {
  it("routes input changes into the password state", () => {
    const els = elementsOf(renderActive(makeRequest(), vi.fn()));
    const input = els.find((element) => element.type === Input)!;
    (input.props.onChange as (event: { target: { value: string } }) => void)({
      target: { value: "hunter2" },
    });
    const passwordUpdate = harness.setStateCalls.find((call) => call.applied === "hunter2");
    expect(passwordUpdate).toBeDefined();
  });
});

describe("respond flow", () => {
  it("submits the password and removes the prompt on success", async () => {
    const onRemove = vi.fn();
    harness.seedState((initial) => initial === "", "hunter2");
    const els = elementsOf(renderActive(makeRequest(), onRemove));
    const form = els.find((element) => element.type === "form")!;

    const preventDefault = vi.fn();
    (form.props.onSubmit as (event: { preventDefault: () => void }) => void)({ preventDefault });
    expect(preventDefault).toHaveBeenCalledTimes(1);
    await flush();

    expect(resolveSshPasswordPrompt).toHaveBeenCalledWith("req-1", "hunter2");
    expect(onRemove).toHaveBeenCalledWith("req-1");
  });

  it("cancelling a live prompt responds with a null password", async () => {
    const onRemove = vi.fn();
    const els = elementsOf(renderActive(makeRequest(), onRemove));
    const cancel = buttonsOf(els)[0]!;

    (cancel.props.onClick as () => void)();
    await flush();
    expect(resolveSshPasswordPrompt).toHaveBeenCalledWith("req-1", null);
    expect(onRemove).toHaveBeenCalledWith("req-1");
  });

  it("closing the dialog while live cancels the prompt", async () => {
    const onRemove = vi.fn();
    const els = elementsOf(renderActive(makeRequest(), onRemove));
    const dialog = els.find((element) => element.type === Dialog)!;

    (dialog.props.onOpenChange as (open: boolean) => void)(false);
    await flush();
    expect(resolveSshPasswordPrompt).toHaveBeenCalledWith("req-1", null);
    // Re-opening is ignored.
    resolveSshPasswordPrompt.mockClear();
    (dialog.props.onOpenChange as (open: boolean) => void)(true);
    expect(resolveSshPasswordPrompt).not.toHaveBeenCalled();
  });

  it("dismissing an expired prompt removes it without contacting the bridge", () => {
    const onRemove = vi.fn();
    const els = elementsOf(
      renderActive(makeRequest({ expiresAt: new Date(FIXED_NOW - 1_000).toISOString() }), onRemove),
    );
    const dismiss = buttonsOf(els)[0]!;

    (dismiss.props.onClick as () => void)();
    expect(onRemove).toHaveBeenCalledWith("req-1");
    expect(resolveSshPasswordPrompt).not.toHaveBeenCalled();
  });

  it("blocks submitting a password after the prompt expired", async () => {
    harness.seedState((initial) => initial === "", "hunter2");
    const els = elementsOf(
      renderActive(makeRequest({ expiresAt: new Date(FIXED_NOW - 1_000).toISOString() }), vi.fn()),
    );
    const form = els.find((element) => element.type === "form")!;

    (form.props.onSubmit as (event: { preventDefault: () => void }) => void)({
      preventDefault: vi.fn(),
    });
    await flush();
    expect(resolveSshPasswordPrompt).not.toHaveBeenCalled();
    const errored = harness.setStateCalls.find(
      (call) => call.applied === "This SSH password prompt expired. Try connecting again.",
    );
    expect(errored).toBeDefined();
  });

  it("ignores re-entrant responses while one is already in flight", async () => {
    harness.seedState((initial) => initial === "", "hunter2");
    const els = elementsOf(renderActive(makeRequest(), vi.fn()));
    const form = els.find((element) => element.type === "form")!;
    // The second useRef is the in-flight guard.
    const respondingGuard = harness.refs.find((ref) => ref.current === false)!;
    respondingGuard.current = true;

    (form.props.onSubmit as (event: { preventDefault: () => void }) => void)({
      preventDefault: vi.fn(),
    });
    await flush();
    expect(resolveSshPasswordPrompt).not.toHaveBeenCalled();
  });

  it("surfaces friendly text for expiry-shaped failures and drops the prompt on cancel failures", async () => {
    // Failure while submitting a password -> mapped to a friendly expiry message.
    resolveSshPasswordPrompt.mockRejectedValueOnce(new Error("prompt is no longer pending"));
    harness.seedState((initial) => initial === "", "hunter2");
    const onRemove = vi.fn();
    let els = elementsOf(renderActive(makeRequest(), onRemove));
    let form = els.find((element) => element.type === "form")!;
    (form.props.onSubmit as (event: { preventDefault: () => void }) => void)({
      preventDefault: vi.fn(),
    });
    await flush();
    expect(
      harness.setStateCalls.some(
        (call) => call.applied === "This SSH password prompt expired. Try connecting again.",
      ),
    ).toBe(true);
    expect(onRemove).not.toHaveBeenCalled();

    // Failure while cancelling (null password) -> the prompt is removed anyway.
    harness.reset();
    resolveSshPasswordPrompt.mockRejectedValueOnce(new Error("bridge down"));
    const cancelRemove = vi.fn();
    els = elementsOf(renderActive(makeRequest(), cancelRemove));
    const cancel = buttonsOf(els)[0]!;
    (cancel.props.onClick as () => void)();
    await flush();
    expect(cancelRemove).toHaveBeenCalledWith("req-1");
  });

  it("passes through a generic error message and a non-Error fallback", async () => {
    harness.seedState((initial) => initial === "", "hunter2");
    resolveSshPasswordPrompt.mockRejectedValueOnce(new Error("network unreachable"));
    let els = elementsOf(renderActive(makeRequest(), vi.fn()));
    let form = els.find((element) => element.type === "form")!;
    (form.props.onSubmit as (event: { preventDefault: () => void }) => void)({
      preventDefault: vi.fn(),
    });
    await flush();
    expect(harness.setStateCalls.some((call) => call.applied === "network unreachable")).toBe(true);

    harness.reset();
    resolveSshPasswordPrompt.mockRejectedValueOnce("weird non-error");
    harness.seedState((initial) => initial === "", "hunter2");
    els = elementsOf(renderActive(makeRequest(), vi.fn()));
    form = els.find((element) => element.type === "form")!;
    (form.props.onSubmit as (event: { preventDefault: () => void }) => void)({
      preventDefault: vi.fn(),
    });
    await flush();
    expect(
      harness.setStateCalls.some((call) => call.applied === "SSH password prompt failed."),
    ).toBe(true);
  });
});

describe("active prompt effects", () => {
  it("focuses and selects the input on the next animation frame", () => {
    renderActive(makeRequest(), vi.fn());
    const focus = vi.fn();
    const select = vi.fn();
    // The first useRef is the input ref.
    harness.refs[0]!.current = { focus, select };

    const cleanups = harness.runEffects();
    expect(rafCallbacks).toHaveLength(1);
    rafCallbacks[0]!(0);
    expect(focus).toHaveBeenCalledTimes(1);
    expect(select).toHaveBeenCalledTimes(1);

    for (const cleanup of cleanups) cleanup();
    expect(cancelledFrames).toHaveLength(1);
  });

  it("ticks the countdown on an interval and clears it on cleanup", () => {
    renderActive(makeRequest(), vi.fn());
    const cleanups = harness.runEffects();
    expect(intervalCallbacks).toHaveLength(1);

    harness.setStateCalls.length = 0;
    intervalCallbacks[0]!();
    const tick = harness.setStateCalls.find((call) => typeof call.applied === "number");
    expect(tick).toBeDefined();

    for (const cleanup of cleanups) cleanup();
    expect(clearedIntervals).toHaveLength(1);
  });
});
