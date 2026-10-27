import { EnvironmentId, ThreadId } from "@coddy/contracts";
import { renderToStaticMarkup } from "react-dom/server";
import * as React from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

/**
 * toast.tsx is a heavy SSR-only React surface. We render it with
 * `renderToStaticMarkup`, using a partial `react` mock to capture `useEffect`
 * bodies so the `ThreadToastVisibleAutoDismiss` timer machinery can be
 * exercised against a fake window/document. Base UI's `Toast.*` primitives are
 * replaced with capture-mocks that render their children, so the whole nested
 * body tree executes and interactive host elements (dismiss button, copy
 * button) can be located and invoked directly.
 */
const harness = vi.hoisted(() => {
  const state = {
    effects: [] as Array<() => void | (() => void)>,
    reset() {
      state.effects.length = 0;
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

const ui = vi.hoisted(() => {
  const registry = {
    entries: [] as Array<{ kind: string; props: Record<string, unknown> }>,
    reset() {
      registry.entries.length = 0;
    },
    record(kind: string, props: unknown) {
      if (props && typeof props === "object") {
        registry.entries.push({ kind, props: props as Record<string, unknown> });
      }
    },
    filter(kind: string) {
      return registry.entries.filter((entry) => entry.kind === kind).map((entry) => entry.props);
    },
  };
  return registry;
});

const testState = vi.hoisted(() => ({
  toasts: [] as Array<Record<string, unknown>>,
  routeTarget: null as Record<string, unknown> | null,
  draftSession: null as Record<string, unknown> | null,
  isCopied: false,
  copyToClipboard: (() => {}) as (text: string) => void,
  managers: [] as Array<{ add: ReturnType<typeof vi.fn>; close: ReturnType<typeof vi.fn> }>,
}));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  const useEffect = (effect: () => void | (() => void)) => {
    harness.effects.push(effect);
  };
  return {
    ...actual,
    useEffect: useEffect as typeof actual.useEffect,
  };
});

vi.mock("@base-ui/react/toast", () => {
  const createToastManager = () => {
    const manager = { add: vi.fn((options: unknown) => options), close: vi.fn() };
    testState.managers.push(manager);
    return manager;
  };
  const passthrough =
    (kind: string, tag = "div") =>
    (props: Record<string, unknown>) => {
      ui.record(kind, props);
      const { children } = props as { children?: React.ReactNode };
      const forwarded: Record<string, unknown> = {};
      for (const [key, value] of Object.entries(props)) {
        if (
          key === "className" ||
          key === "style" ||
          key.startsWith("data-") ||
          key.startsWith("aria-")
        ) {
          forwarded[key] = value;
        }
      }
      return React.createElement(tag, forwarded, children);
    };
  return {
    Toast: {
      createToastManager,
      Provider: (props: { children?: React.ReactNode }) =>
        React.createElement(React.Fragment, null, props.children),
      Portal: passthrough("Portal"),
      Viewport: passthrough("Viewport"),
      Root: passthrough("Root"),
      Content: passthrough("Content"),
      Positioner: passthrough("Positioner"),
      Title: passthrough("Title"),
      Description: passthrough("Description"),
      Action: passthrough("Action", "button"),
      useToastManager: () => ({ toasts: testState.toasts }),
    },
  };
});

vi.mock("@tanstack/react-router", () => ({
  useParams: (opts: { select?: (params: Record<string, unknown>) => unknown }) =>
    opts.select ? opts.select({}) : {},
}));

vi.mock("~/components/ui/button", () => ({
  buttonVariants: () => "button-variant",
}));

vi.mock("~/composerDraftStore", () => ({
  useComposerDraftStore: (
    selector: (store: { getDraftSession: (id: unknown) => unknown }) => unknown,
  ) => selector({ getDraftSession: () => testState.draftSession }),
}));

vi.mock("~/hooks/useCopyToClipboard", () => ({
  useCopyToClipboard: () => ({
    copyToClipboard: (text: string) => testState.copyToClipboard(text),
    isCopied: testState.isCopied,
  }),
}));

vi.mock("~/threadRoutes", () => ({
  resolveThreadRouteTarget: () => testState.routeTarget,
}));

vi.mock("./tooltip", () => ({
  Tooltip: (props: { children?: React.ReactNode }) => {
    ui.record("Tooltip", props);
    return React.createElement(React.Fragment, null, props.children);
  },
  TooltipTrigger: (props: { render?: React.ReactNode; children?: React.ReactNode }) => {
    ui.record("TooltipTrigger", props);
    return React.createElement("span", null, props.render, props.children);
  },
  TooltipPopup: (props: { children?: React.ReactNode }) => {
    ui.record("TooltipPopup", props);
    return React.createElement("div", null, props.children);
  },
}));

import { ToastProvider, AnchoredToastProvider, toastManager, anchoredToastManager } from "./toast";

// ── Fake window/document for the auto-dismiss effect ──────────────────────────
interface FakeTimer {
  id: number;
  cb: () => void;
  delay: number;
}
let winListeners: Map<string, (event?: unknown) => void>;
let docListeners: Map<string, (event?: unknown) => void>;
let timers: FakeTimer[];
let clearedTimeouts: number[];
let docState: { visibilityState: string; focused: boolean };

function installFakeEnv() {
  winListeners = new Map();
  docListeners = new Map();
  timers = [];
  clearedTimeouts = [];
  docState = { visibilityState: "visible", focused: true };
  let nextTimerId = 1;
  vi.stubGlobal("window", {
    setTimeout: (cb: () => void, delay: number) => {
      const id = nextTimerId++;
      timers.push({ id, cb, delay });
      return id;
    },
    clearTimeout: (id: number) => {
      clearedTimeouts.push(id);
    },
    addEventListener: (type: string, handler: (event?: unknown) => void) => {
      winListeners.set(type, handler);
    },
    removeEventListener: (type: string) => {
      winListeners.delete(type);
    },
  });
  vi.stubGlobal("document", {
    get visibilityState() {
      return docState.visibilityState;
    },
    hasFocus: () => docState.focused,
    addEventListener: (type: string, handler: (event?: unknown) => void) => {
      docListeners.set(type, handler);
    },
    removeEventListener: (type: string) => {
      docListeners.delete(type);
    },
  });
}

// ── Element-tree walking to reach host handlers inside recorded children ─────
function collectElements(node: unknown, out: React.ReactElement[]): void {
  if (node === null || node === undefined || typeof node !== "object") return;
  if (Array.isArray(node)) {
    for (const child of node) collectElements(child, out);
    return;
  }
  if (React.isValidElement(node)) {
    out.push(node);
    const props = node.props as Record<string, unknown>;
    collectElements(props["children"], out);
    collectElements(props["render"], out);
  }
}

function findHostByAria(label: string): Record<string, unknown> | null {
  for (const entry of ui.entries) {
    const elements: React.ReactElement[] = [];
    collectElements(entry.props["children"], elements);
    collectElements(entry.props["render"], elements);
    for (const element of elements) {
      const props = element.props as Record<string, unknown>;
      if (props["aria-label"] === label) return props;
    }
  }
  return null;
}

// ── Fixtures ─────────────────────────────────────────────────────────────────
let toastSeq = 0;
function makeToast(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  toastSeq += 1;
  return {
    id: `toast-${toastSeq}`,
    title: "A title",
    ...overrides,
  };
}

function renderProvider(element: React.ReactElement): string {
  ui.reset();
  harness.effects.length = 0;
  return renderToStaticMarkup(element);
}

beforeEach(() => {
  harness.reset();
  ui.reset();
  testState.toasts = [];
  testState.routeTarget = null;
  testState.draftSession = null;
  testState.isCopied = false;
  testState.copyToClipboard = vi.fn();
  for (const manager of testState.managers) {
    manager.add.mockClear();
    manager.close.mockClear();
  }
  installFakeEnv();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("module wiring", () => {
  it("creates two independent toast managers", () => {
    expect(typeof toastManager.add).toBe("function");
    expect(typeof toastManager.close).toBe("function");
    expect(toastManager).not.toBe(anchoredToastManager);
  });
});

describe("ToastProvider rendering", () => {
  it("renders an empty viewport when there are no toasts", () => {
    const markup = renderProvider(<ToastProvider />);
    expect(markup).toContain('data-slot="toast-viewport"');
    expect(markup).toContain('data-position="top-right"');
  });

  it("renders a basic toast with its title and description slots", () => {
    testState.toasts = [makeToast({ type: "info", description: "hello" })];
    const markup = renderProvider(<ToastProvider />);
    expect(markup).toContain('data-slot="toast-title"');
    expect(markup).toContain('data-slot="toast-description"');
    expect(markup).toContain('data-slot="toast-icon"');
    expect(markup).toContain('aria-label="Dismiss notification"');
    // No trailing controls → the wider end padding is applied.
    expect(markup).toContain("pr-10");
  });

  it("renders a leading icon in place of the type icon when provided", () => {
    testState.toasts = [
      makeToast({ type: "success", data: { leadingIcon: <span data-testid="lead" /> } }),
    ];
    const markup = renderProvider(<ToastProvider />);
    expect(markup).toContain('data-testid="lead"');
    expect(markup).toContain('data-slot="toast-icon"');
  });

  it("renders without any icon when the toast has no type and no leading icon", () => {
    testState.toasts = [makeToast({ description: "plain" })];
    const markup = renderProvider(<ToastProvider />);
    expect(markup).not.toContain('data-slot="toast-icon"');
  });

  it("clamps long error descriptions but leaves short ones unclamped", () => {
    testState.toasts = [makeToast({ type: "error", description: "x".repeat(200) })];
    expect(renderProvider(<ToastProvider />)).toContain("line-clamp-4");

    testState.toasts = [makeToast({ type: "error", description: "short" })];
    expect(renderProvider(<ToastProvider />)).not.toContain("line-clamp-4");
  });

  it("shows the copy-error control and tighter padding for string error descriptions", () => {
    testState.toasts = [makeToast({ type: "error", description: "boom" })];
    const markup = renderProvider(<ToastProvider />);
    expect(markup).toContain('aria-label="Copy error"');
    expect(markup).toContain("pr-6");
  });

  it("suppresses the copy-error control when hideCopyButton is set", () => {
    testState.toasts = [
      makeToast({ type: "error", description: "boom", data: { hideCopyButton: true } }),
    ];
    const markup = renderProvider(<ToastProvider />);
    expect(markup).not.toContain('aria-label="Copy error"');
  });

  it("renders additional actions, a secondary action, and a primary action", () => {
    testState.toasts = [
      makeToast({
        type: "info",
        actionProps: { children: "Primary" },
        data: {
          actionLayout: "stacked-end",
          additionalActions: [{ id: "a1", props: { children: "Extra", className: "extra" } }],
          secondaryActionProps: { children: "Second", className: "sec" },
          secondaryActionVariant: "outline",
          actionVariant: "destructive",
        },
      }),
    ];
    const markup = renderProvider(<ToastProvider />);
    expect(markup).toContain("Extra");
    expect(markup).toContain("Second");
    expect(markup).toContain('data-slot="toast-action"');
    expect(markup).toContain("Primary");
  });

  it("hides collapsed content for toasts behind the front-most one", () => {
    testState.toasts = [
      makeToast({ type: "info", height: 40 }),
      makeToast({ type: "info", height: 40 }),
    ];
    const markup = renderProvider(<ToastProvider />);
    expect(markup).toContain("not-data-expanded:opacity-0");
  });
});

describe("ToastProvider positions", () => {
  it.each(["top-left", "top-center", "bottom-right", "bottom-left", "bottom-center"] as const)(
    "renders position %s",
    (position) => {
      testState.toasts = [makeToast({ type: "info" })];
      const markup = renderProvider(<ToastProvider position={position} />);
      expect(markup).toContain(`data-position="${position}"`);
    },
  );
});

describe("dismiss + copy handlers", () => {
  it("closes the toast (and calls onClose) via the corner dismiss button", () => {
    const onClose = vi.fn();
    testState.toasts = [makeToast({ id: "toast-dismiss", type: "info", data: { onClose } })];
    renderProvider(<ToastProvider />);

    const dismiss = findHostByAria("Dismiss notification");
    expect(dismiss).not.toBeNull();
    (dismiss!["onClick"] as () => void)();
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(toastManager.close).toHaveBeenCalledWith("toast-dismiss");
  });

  it("invokes copyToClipboard from the copy-error control", () => {
    const copy = vi.fn();
    testState.copyToClipboard = copy;
    testState.toasts = [makeToast({ type: "error", description: "the error text" })];
    renderProvider(<ToastProvider />);

    const copyButton = findHostByAria("Copy error");
    expect(copyButton).not.toBeNull();
    (copyButton!["onClick"] as () => void)();
    expect(copy).toHaveBeenCalledWith("the error text");
  });

  it("shows the copied state label when isCopied is true", () => {
    testState.isCopied = true;
    testState.toasts = [makeToast({ type: "error", description: "err" })];
    const markup = renderProvider(<ToastProvider />);
    expect(markup).toContain('aria-label="Copied error"');
  });
});

describe("thread-scoped rendering", () => {
  const environmentId = EnvironmentId.make("environment-1");
  const threadId = ThreadId.make("thread-1");

  it("hides a thread-scoped toast when no thread is active", () => {
    testState.routeTarget = null;
    testState.toasts = [
      makeToast({ type: "info", data: { threadRef: { environmentId, threadId } } }),
    ];
    const markup = renderProvider(<ToastProvider />);
    expect(markup).not.toContain('data-slot="toast-title"');
  });

  it("shows a thread-scoped toast for the active server thread", () => {
    testState.routeTarget = { kind: "server", threadRef: { environmentId, threadId } };
    testState.toasts = [
      makeToast({ type: "info", data: { threadRef: { environmentId, threadId } } }),
    ];
    const markup = renderProvider(<ToastProvider />);
    expect(markup).toContain('data-slot="toast-title"');
  });

  it("resolves the active thread from a draft route target", () => {
    testState.routeTarget = { kind: "draft", draftId: "draft-1" };
    testState.draftSession = { environmentId, threadId };
    testState.toasts = [
      makeToast({ type: "info", data: { threadRef: { environmentId, threadId } } }),
    ];
    const markup = renderProvider(<ToastProvider />);
    expect(markup).toContain('data-slot="toast-title"');
  });

  it("treats a draft route without a session as no active thread", () => {
    testState.routeTarget = { kind: "draft", draftId: "draft-1" };
    testState.draftSession = null;
    testState.toasts = [
      makeToast({ type: "info", data: { threadRef: { environmentId, threadId } } }),
    ];
    const markup = renderProvider(<ToastProvider />);
    expect(markup).not.toContain('data-slot="toast-title"');
  });
});

describe("auto-dismiss effect", () => {
  function renderWithAutoDismiss(dismissAfterVisibleMs: number | undefined) {
    testState.toasts = [
      makeToast({ id: `auto-${toastSeq}`, type: "info", data: { dismissAfterVisibleMs } }),
    ];
    return renderProvider(<ToastProvider />);
  }

  it("does nothing when there is no dismiss delay", () => {
    renderWithAutoDismiss(undefined);
    const cleanups = harness.runEffects();
    // No timers scheduled and no listeners registered by the auto-dismiss effect.
    expect(timers).toHaveLength(0);
    for (const cleanup of cleanups) cleanup();
  });

  it("schedules a timer while visible+focused and closes the toast when it fires", () => {
    testState.toasts = [
      makeToast({ id: "auto-close", type: "info", data: { dismissAfterVisibleMs: 4000 } }),
    ];
    renderProvider(<ToastProvider />);

    const cleanups = harness.runEffects();
    // The auto-dismiss effect registered visibility/focus listeners and armed a timer.
    expect(docListeners.has("visibilitychange")).toBe(true);
    expect(winListeners.has("focus")).toBe(true);
    expect(winListeners.has("blur")).toBe(true);
    expect(timers.length).toBeGreaterThanOrEqual(1);

    // Fire the armed timeout → the toast is closed via the manager.
    timers[timers.length - 1]!.cb();
    expect(toastManager.close).toHaveBeenCalledWith("auto-close");

    for (const cleanup of cleanups) cleanup();
    // Cleanup removed the listeners.
    expect(docListeners.has("visibilitychange")).toBe(false);
  });

  it("pauses the timer when the window loses focus and resumes on focus", () => {
    testState.toasts = [
      makeToast({ id: "auto-pause", type: "info", data: { dismissAfterVisibleMs: 5000 } }),
    ];
    renderProvider(<ToastProvider />);
    const cleanups = harness.runEffects();

    const initialTimerCount = timers.length;
    expect(initialTimerCount).toBeGreaterThanOrEqual(1);

    // Lose focus → the sync handler pauses (clears the timer, no close).
    docState.focused = false;
    winListeners.get("blur")!();
    expect(clearedTimeouts.length).toBeGreaterThanOrEqual(1);
    expect(toastManager.close).not.toHaveBeenCalled();

    // Regain focus → a fresh timer is armed.
    docState.focused = true;
    winListeners.get("focus")!();
    expect(timers.length).toBeGreaterThan(initialTimerCount);

    for (const cleanup of cleanups) cleanup();
  });

  it("does not arm a timer while the document is hidden", () => {
    docState.visibilityState = "hidden";
    testState.toasts = [
      makeToast({ id: "auto-hidden", type: "info", data: { dismissAfterVisibleMs: 5000 } }),
    ];
    renderProvider(<ToastProvider />);
    harness.runEffects();
    expect(timers).toHaveLength(0);
  });

  it("prunes stale timeout bookkeeping for toasts no longer present", () => {
    // A first render with an auto-dismiss toast populates the module-level map.
    testState.toasts = [
      makeToast({ id: "auto-prune", type: "info", data: { dismissAfterVisibleMs: 5000 } }),
    ];
    renderProvider(<ToastProvider />);
    harness.runEffects();

    // Re-render with a different toast set; the viewport cleanup effect prunes the map.
    testState.toasts = [makeToast({ id: "auto-other", type: "info" })];
    renderProvider(<ToastProvider />);
    // Running the viewport effect should not throw.
    expect(() => harness.runEffects()).not.toThrow();
  });
});

describe("AnchoredToastProvider", () => {
  it("renders nothing for a toast without an anchor", () => {
    testState.toasts = [makeToast({ type: "info", positionerProps: {} })];
    const markup = renderProvider(<AnchoredToastProvider />);
    expect(markup).toContain('data-slot="toast-viewport-anchored"');
    expect(markup).not.toContain('data-slot="toast-popup"');
  });

  it("renders a full anchored toast with a dismiss control", () => {
    const onClose = vi.fn();
    testState.toasts = [
      makeToast({
        id: "anchored-1",
        type: "error",
        description: "anchored error",
        positionerProps: { anchor: { x: 1 }, sideOffset: 8 },
        data: { onClose },
      }),
    ];
    const markup = renderProvider(<AnchoredToastProvider />);
    expect(markup).toContain('data-slot="toast-popup"');
    expect(markup).toContain('aria-label="Dismiss notification"');
    expect(markup).toContain("rounded-lg");

    const dismiss = findHostByAria("Dismiss notification");
    (dismiss!["onClick"] as () => void)();
    expect(onClose).toHaveBeenCalled();
    expect(anchoredToastManager.close).toHaveBeenCalledWith("anchored-1");
  });

  it("renders a compact tooltip-style anchored toast", () => {
    testState.toasts = [
      makeToast({
        id: "anchored-tooltip",
        title: "tip",
        positionerProps: { anchor: { x: 2 } },
        data: { tooltipStyle: true },
      }),
    ];
    const markup = renderProvider(<AnchoredToastProvider />);
    expect(markup).toContain("rounded-md");
    expect(markup).toContain('data-slot="toast-title"');
    // Tooltip style omits the dismiss orb.
    expect(markup).not.toContain('aria-label="Dismiss notification"');
  });
});
