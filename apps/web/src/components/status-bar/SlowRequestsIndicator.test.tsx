// @vitest-environment happy-dom

import { DEFAULT_CLIENT_SETTINGS } from "@bibcode/contracts/settings";
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { AppAtomRegistryProvider } from "../../rpc/atomRegistry";
import {
  acknowledgeRpcRequest,
  resetRequestLatencyStateForTests,
  SLOW_RPC_ACK_THRESHOLD_MS,
  trackRpcRequestSent,
} from "../../rpc/requestLatencyState";
import { formatTimestamp } from "../../timestampFormat";
import { AppStatusBarView, SlowRequestsStatusBar } from "./AppStatusBar";
import { SlowRequestsIndicator } from "./SlowRequestsIndicator";

// Each latency-hook consumer gets its own counter, including the hosted bar and indicator shell.
const subscription = vi.hoisted(() => ({ readers: new Map<string, { renders: number }>() }));
const environmentState = vi.hoisted(() => ({
  environments: [] as Array<{ environmentId: string; label: string }>,
  reads: 0,
}));

vi.mock("./ProfilerRecordingSegment", () => ({ ProfilerRecordingSegment: () => null }));

vi.mock("../../state/environments", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../state/environments")>()),
  useEnvironments: () => {
    environmentState.reads += 1;
    return { environments: environmentState.environments };
  },
}));

vi.mock("../../rpc/requestLatencyState", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../rpc/requestLatencyState")>();
  const { useId } = await import("react");
  function useRenderCounter() {
    const id = useId();
    let counter = subscription.readers.get(id);
    if (counter === undefined) {
      counter = { renders: 0 };
      subscription.readers.set(id, counter);
    }
    counter.renders += 1;
  }
  return {
    ...actual,
    useSlowRpcAckRequests: () => {
      useRenderCounter();
      return actual.useSlowRpcAckRequests();
    },
    useHasSlowRpcAckRequests: () => {
      useRenderCounter();
      return actual.useHasSlowRpcAckRequests();
    },
  };
});

const FIRST_STARTED_AT = "2026-09-26T10:00:00.000Z";
const SECOND_STARTED_AT = "2026-09-26T10:00:04.000Z";

const mounted: Array<{ root: Root; container: HTMLDivElement }> = [];

async function render(children: ReactNode): Promise<HTMLDivElement> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  mounted.push({ root, container });
  await act(async () => {
    root.render(<AppAtomRegistryProvider>{children}</AppAtomRegistryProvider>);
  });
  return container;
}

async function rerender(children: ReactNode): Promise<void> {
  const entry = mounted.at(-1);
  if (entry === undefined) throw new Error("Nothing is mounted to re-render.");
  await act(async () => {
    entry.root.render(<AppAtomRegistryProvider>{children}</AppAtomRegistryProvider>);
  });
}

function statusBarView(
  terminalCount: number,
  { showResourceUsage = false }: { readonly showResourceUsage?: boolean } = {},
): ReactNode {
  return (
    <AppStatusBarView
      usage={null}
      diagnostics={{ diagnostics: null, queryError: null }}
      localDiagnostics={null}
      terminalCount={terminalCount}
      showResourceUsage={showResourceUsage}
      onRefresh={() => undefined}
    />
  );
}

/**
 * Sends requests through the real latency tracker and lets the slow threshold pass
 * without an acknowledgement. Each request starts at its own fixed time.
 */
function sendUnansweredRequests(
  requests: ReadonlyArray<{
    readonly id: string;
    readonly method: string;
    readonly environmentId: string;
    readonly startedAt: string;
  }>,
): void {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
  try {
    for (const request of requests) {
      vi.setSystemTime(new Date(request.startedAt));
      trackRpcRequestSent(request.id, {
        method: request.method,
        environmentId: request.environmentId,
      });
    }
    act(() => {
      vi.advanceTimersByTime(SLOW_RPC_ACK_THRESHOLD_MS);
    });
  } finally {
    vi.useRealTimers();
  }
}

async function finish(requestId: string): Promise<void> {
  await act(async () => {
    acknowledgeRpcRequest(requestId);
  });
}

async function settle(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

/** Lets animation-frame work run, such as Base UI moving or returning focus. */
async function nextFrames(): Promise<void> {
  await act(async () => {
    for (let frame = 0; frame < 3; frame += 1) {
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    }
  });
}

function oneSlowRequest(): void {
  sendUnansweredRequests([
    {
      id: "one",
      method: "projects.listEntries",
      environmentId: "primary",
      startedAt: FIRST_STARTED_AT,
    },
  ]);
}

function indicator(container: ParentNode = document): HTMLButtonElement | null {
  return (
    Array.from(container.querySelectorAll<HTMLButtonElement>("button")).find((button) =>
      /slow request/.test(button.textContent ?? ""),
    ) ?? null
  );
}

function requireIndicator(container: ParentNode = document): HTMLButtonElement {
  const button = indicator(container);
  if (button === null) throw new Error("The slow-request indicator was not rendered.");
  return button;
}

function openList(): HTMLElement | null {
  return (
    Array.from(document.querySelectorAll<HTMLElement>('[data-slot="popover-popup"]')).find(
      (popup) => !popup.closest('[data-slot="popover-positioner"]')?.hasAttribute("hidden"),
    ) ?? null
  );
}

function announcement(container: ParentNode): string | null {
  return container.querySelector('[role="status"]')?.textContent ?? null;
}

async function click(target: HTMLElement): Promise<void> {
  await act(async () => {
    target.click();
  });
  await settle();
}

async function pressEnter(target: HTMLButtonElement): Promise<void> {
  await act(async () => {
    const keydown = new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true });
    target.dispatchEvent(keydown);
    target.dispatchEvent(
      new KeyboardEvent("keyup", { key: "Enter", bubbles: true, cancelable: true }),
    );
    if (!keydown.defaultPrevented) {
      target.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true, detail: 0 }));
    }
  });
  await settle();
}

async function pressEscape(target: Element): Promise<void> {
  await act(async () => {
    target.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
    );
    target.dispatchEvent(
      new KeyboardEvent("keyup", { key: "Escape", bubbles: true, cancelable: true }),
    );
  });
  await settle();
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  resetRequestLatencyStateForTests();
  subscription.readers.clear();
  environmentState.environments = [
    { environmentId: "primary", label: "Local" },
    { environmentId: "remote-1", label: "Luna" },
  ];
  environmentState.reads = 0;
});

afterEach(async () => {
  for (const entry of mounted.splice(0)) {
    await act(async () => entry.root.unmount());
    entry.container.remove();
  }
  resetRequestLatencyStateForTests();
  vi.useRealTimers();
  vi.restoreAllMocks();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("SlowRequestsIndicator", () => {
  it("stays hidden while no request is slow", async () => {
    const container = await render(<SlowRequestsIndicator />);
    expect(indicator(container)).toBeNull();

    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
    try {
      trackRpcRequestSent("pending", { method: "git.status", environmentId: "primary" });
      act(() => {
        vi.advanceTimersByTime(SLOW_RPC_ACK_THRESHOLD_MS - 1);
      });
    } finally {
      vi.useRealTimers();
    }

    expect(indicator(container)).toBeNull();
    expect(announcement(container)).toBe("");
  });

  it("counts one slow request in the singular and several in the plural", async () => {
    const container = await render(<SlowRequestsIndicator />);

    sendUnansweredRequests([
      {
        id: "one",
        method: "projects.listEntries",
        environmentId: "primary",
        startedAt: FIRST_STARTED_AT,
      },
    ]);
    const trigger = requireIndicator(container);
    expect(trigger.tagName).toBe("BUTTON");
    expect(trigger.type).toBe("button");
    expect(trigger.textContent).toBe("1 slow request");

    sendUnansweredRequests([
      { id: "two", method: "git.status", environmentId: "primary", startedAt: SECOND_STARTED_AT },
    ]);
    expect(requireIndicator(container).textContent).toBe("2 slow requests");
  });

  it("lists each slow request's name, environment label, and start time", async () => {
    const container = await render(<SlowRequestsIndicator />);
    sendUnansweredRequests([
      {
        id: "one",
        method: "projects.listEntries",
        environmentId: "primary",
        startedAt: FIRST_STARTED_AT,
      },
      { id: "two", method: "git.status", environmentId: "remote-1", startedAt: SECOND_STARTED_AT },
    ]);

    expect(environmentState.reads).toBe(0);
    await click(requireIndicator(container));
    expect(environmentState.reads).toBeGreaterThan(0);

    const list = openList();
    if (list === null) throw new Error("The slow-request list did not open.");
    const items = Array.from(list.querySelectorAll("li"));
    expect(items.map((item) => item.querySelector("time")?.getAttribute("dateTime"))).toEqual([
      FIRST_STARTED_AT,
      SECOND_STARTED_AT,
    ]);
    expect(items[0]?.querySelector("div")?.textContent).toBe("projects.listEntries · Local");
    expect(items[0]?.querySelector("time")?.textContent).toBe(
      formatTimestamp(FIRST_STARTED_AT, DEFAULT_CLIENT_SETTINGS.timestampFormat),
    );
    expect(items[1]?.querySelector("div")?.textContent).toBe("git.status · Luna");
    expect(items[1]?.querySelector("time")?.textContent).toBe(
      formatTimestamp(SECOND_STARTED_AT, DEFAULT_CLIENT_SETTINGS.timestampFormat),
    );

    await pressEscape(list);
    const readsAfterClosing = environmentState.reads;
    await finish("two");
    expect(environmentState.reads).toBe(readsAfterClosing);
  });

  it.each([
    { environmentId: "missing", label: undefined },
    { environmentId: "blank", label: "" },
    { environmentId: "whitespace", label: "   " },
  ])(
    "falls back to the environment id for $environmentId labels",
    async ({ environmentId, label }) => {
      if (label !== undefined) environmentState.environments.push({ environmentId, label });
      const container = await render(<SlowRequestsIndicator />);
      sendUnansweredRequests([
        { id: "one", method: "git.status", environmentId, startedAt: FIRST_STARTED_AT },
      ]);

      await click(requireIndicator(container));

      expect(openList()?.querySelector("li > div")?.textContent).toBe(
        `git.status · ${environmentId}`,
      );
    },
  );

  it("disappears when the slow requests finish", async () => {
    const container = await render(<SlowRequestsIndicator />);
    sendUnansweredRequests([
      {
        id: "one",
        method: "projects.listEntries",
        environmentId: "primary",
        startedAt: FIRST_STARTED_AT,
      },
      { id: "two", method: "git.status", environmentId: "primary", startedAt: SECOND_STARTED_AT },
    ]);

    await finish("one");
    expect(requireIndicator(container).textContent).toBe("1 slow request");

    await finish("two");
    expect(indicator(container)).toBeNull();
  });

  it("closes an open list when the last slow request finishes", async () => {
    const container = await render(<SlowRequestsIndicator />);
    sendUnansweredRequests([
      {
        id: "one",
        method: "projects.listEntries",
        environmentId: "primary",
        startedAt: FIRST_STARTED_AT,
      },
    ]);
    await click(requireIndicator(container));
    expect(openList()).not.toBeNull();

    await finish("one");

    expect(indicator(container)).toBeNull();
    expect(openList()).toBeNull();
  });

  it("names the list with its heading and describes the wait", async () => {
    const container = await render(<SlowRequestsIndicator />);
    oneSlowRequest();

    await click(requireIndicator(container));

    const list = openList();
    if (list === null) throw new Error("The slow-request list did not open.");
    expect(list.hasAttribute("aria-label")).toBe(false);
    const heading = document.getElementById(list.getAttribute("aria-labelledby") ?? "");
    expect(heading?.tagName).toBe("H2");
    expect(heading?.textContent).toBe("Slow requests");
    const description = document.getElementById(list.getAttribute("aria-describedby") ?? "");
    expect(description?.textContent).toBe("Waiting more than 15 seconds for a response.");
  });

  it("opens from the keyboard, closes on Escape, and returns focus to the trigger", async () => {
    const container = await render(<SlowRequestsIndicator />);
    oneSlowRequest();
    const trigger = requireIndicator(container);

    trigger.focus();
    expect(document.activeElement).toBe(trigger);
    await pressEnter(trigger);
    await nextFrames();
    const list = openList();
    if (list === null) throw new Error("Enter did not open the slow-request list.");
    expect(trigger.getAttribute("aria-expanded")).toBe("true");
    // Focus moves into the list, so a screen reader starts reading it.
    expect(list.contains(document.activeElement)).toBe(true);

    await pressEscape(document.activeElement ?? trigger);

    expect(openList()).toBeNull();
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(trigger);
  });

  it("announces the first slow request once, not on every count change", async () => {
    const container = await render(<SlowRequestsIndicator />);
    const region = container.querySelector('[role="status"]');
    expect(region?.getAttribute("aria-live")).toBe("polite");
    expect(announcement(container)).toBe("");

    sendUnansweredRequests([
      {
        id: "one",
        method: "projects.listEntries",
        environmentId: "primary",
        startedAt: FIRST_STARTED_AT,
      },
    ]);
    expect(announcement(container)).toBe("Some requests are slow");
    // The same live region stays mounted, so screen readers hear changes to it.
    expect(container.querySelector('[role="status"]')).toBe(region);

    sendUnansweredRequests([
      { id: "two", method: "git.status", environmentId: "primary", startedAt: SECOND_STARTED_AT },
    ]);
    expect(announcement(container)).toBe("Some requests are slow");

    await finish("one");
    expect(announcement(container)).toBe("Some requests are slow");

    await finish("two");
    expect(announcement(container)).toBe("");
    expect(container.querySelector('[role="status"]')).toBe(region);
  });

  it("does not re-render the live-region shell on count changes while its count label updates", async () => {
    const container = await render(<SlowRequestsIndicator />);
    const shell = subscription.readers.values().next().value;
    if (shell === undefined) throw new Error("The indicator did not subscribe to latency state.");
    expect(shell.renders).toBe(1);

    oneSlowRequest();
    expect(shell.renders).toBe(2);
    expect(requireIndicator(container).textContent).toBe("1 slow request");

    sendUnansweredRequests([
      { id: "two", method: "git.status", environmentId: "remote-1", startedAt: SECOND_STARTED_AT },
    ]);
    expect(requireIndicator(container).textContent).toBe("2 slow requests");
    expect(shell.renders).toBe(2);

    await finish("one");
    expect(requireIndicator(container).textContent).toBe("1 slow request");
    expect(shell.renders).toBe(2);

    await finish("two");
    expect(indicator(container)).toBeNull();
    expect(shell.renders).toBe(3);
  });

  it("appears in the status bar", async () => {
    const container = await render(statusBarView(0));
    const statusBar = container.querySelector('[data-testid="app-status-bar"]');
    if (statusBar === null) throw new Error("The status bar was not rendered.");
    expect(indicator(statusBar)).toBeNull();

    sendUnansweredRequests([
      {
        id: "one",
        method: "projects.listEntries",
        environmentId: "primary",
        startedAt: FIRST_STARTED_AT,
      },
    ]);

    expect(requireIndicator(statusBar).textContent).toBe("1 slow request");
  });

  it("does not re-render when the rest of the status bar re-renders", async () => {
    await render(statusBarView(0));
    sendUnansweredRequests([
      {
        id: "one",
        method: "projects.listEntries",
        environmentId: "primary",
        startedAt: FIRST_STARTED_AT,
      },
    ]);
    const shell = subscription.readers.values().next().value;
    if (shell === undefined) throw new Error("The indicator did not subscribe to latency state.");
    const rendersWithSlowRequest = shell.renders;

    // The status bar re-renders on its own polling, for example when the terminal count changes.
    await rerender(statusBarView(1));
    await rerender(statusBarView(2));

    expect(shell.renders).toBe(rendersWithSlowRequest);
    expect(requireIndicator().textContent).toBe("1 slow request");
  });
});

describe("focus when the indicator leaves", () => {
  it("moves focus to the next status bar item when the last request finishes inside the open list", async () => {
    await render(statusBarView(0, { showResourceUsage: true }));
    oneSlowRequest();
    const trigger = requireIndicator();
    trigger.focus();
    await pressEnter(trigger);
    await nextFrames();
    expect(openList()?.contains(document.activeElement)).toBe(true);
    const focus = vi.spyOn(HTMLElement.prototype, "focus");

    await finish("one");
    await nextFrames();

    const resources = document.querySelector<HTMLButtonElement>(
      '[aria-label^="Combined monitored resources"]',
    );
    expect(resources).not.toBeNull();
    expect(document.activeElement).toBe(resources);
    const focusCall = focus.mock.contexts.findIndex((element) => element === resources);
    expect(focus.mock.calls[focusCall]).toEqual([{ preventScroll: true }]);
  });

  it("moves focus to the control before the indicator when nothing follows it in the bar", async () => {
    await render(
      <>
        <button type="button">Last control</button>
        <SlowRequestsStatusBar />
      </>,
    );
    oneSlowRequest();
    const trigger = requireIndicator();
    trigger.focus();
    const focus = vi.spyOn(HTMLElement.prototype, "focus");

    await finish("one");
    await nextFrames();

    expect(document.activeElement?.textContent).toBe("Last control");
    const focusCall = focus.mock.contexts.findIndex(
      (element) => element === document.activeElement,
    );
    expect(focus.mock.calls[focusCall]).toEqual([]);
  });

  it("leaves focus alone when the indicator did not have it", async () => {
    await render(
      <>
        <button type="button">Elsewhere</button>
        {statusBarView(0, { showResourceUsage: true })}
      </>,
    );
    oneSlowRequest();
    const elsewhere = Array.from(document.querySelectorAll("button")).find(
      (button) => button.textContent === "Elsewhere",
    );
    if (elsewhere === undefined) throw new Error("The other control was not rendered.");
    elsewhere.focus();

    await finish("one");
    await nextFrames();

    expect(document.activeElement).toBe(elsewhere);
  });
});

describe("SlowRequestsStatusBar", () => {
  it("re-renders only when slowness starts or ends", async () => {
    const container = await render(<SlowRequestsStatusBar />);
    const bar = subscription.readers.values().next().value;
    if (bar === undefined) throw new Error("The hosted bar did not subscribe to latency state.");
    expect(bar.renders).toBe(1);

    oneSlowRequest();
    expect(bar.renders).toBe(2);

    sendUnansweredRequests([
      { id: "two", method: "git.status", environmentId: "remote-1", startedAt: SECOND_STARTED_AT },
    ]);
    expect(requireIndicator(container).textContent).toBe("2 slow requests");
    expect(bar.renders).toBe(2);

    await finish("one");
    expect(requireIndicator(container).textContent).toBe("1 slow request");
    expect(bar.renders).toBe(2);

    await finish("two");
    expect(indicator(container)).toBeNull();
    expect(bar.renders).toBe(3);
  });

  it("takes no space until a request is slow, then shows the indicator", async () => {
    const container = await render(<SlowRequestsStatusBar />);
    const bar = container.querySelector<HTMLElement>('[data-testid="slow-requests-status-bar"]');
    if (bar === null) throw new Error("The hosted status bar was not rendered.");
    const region = bar.querySelector('[role="status"]');
    expect(bar.hasAttribute("data-collapsed")).toBe(true);
    expect(indicator(bar)).toBeNull();
    expect(region?.textContent).toBe("");

    oneSlowRequest();

    expect(bar.hasAttribute("data-collapsed")).toBe(false);
    expect(requireIndicator(bar).textContent).toBe("1 slow request");
    // The live region stays mounted while the bar collapses and opens.
    expect(bar.querySelector('[role="status"]')).toBe(region);
    expect(region?.textContent).toBe("Some requests are slow");

    await finish("one");

    expect(bar.hasAttribute("data-collapsed")).toBe(true);
    expect(indicator(bar)).toBeNull();
    expect(bar.querySelector('[role="status"]')).toBe(region);
  });
});
