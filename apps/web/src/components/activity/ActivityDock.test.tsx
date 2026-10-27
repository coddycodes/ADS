// @vitest-environment happy-dom

import type {
  ActivityActorSummary,
  ActivitySnapshot,
  ActivityWorkItemSummary,
} from "@coddy/contracts";
import { act, type ReactElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { ActivityDock, type ActivityDockProps } from "./ActivityDock";

const FIXED_NOW = "2026-07-22T20:10:00.000Z";
const mounted: Array<{ readonly root: Root; readonly container: HTMLDivElement }> = [];
const initialInnerWidth = window.innerWidth;

function actor(id: string, name = id): ActivityActorSummary {
  return {
    _tag: "actor",
    id,
    name,
    status: "running",
    summary: null,
    startedAt: "2026-07-22T20:00:00.000Z",
    updatedAt: "2026-07-22T20:09:00.000Z",
    terminalAt: null,
    parentActorId: null,
    role: null,
    providerType: null,
  } as unknown as ActivityActorSummary;
}

function workItem(id: string, name = id): ActivityWorkItemSummary {
  return {
    _tag: "workItem",
    id,
    name,
    status: "running",
    summary: null,
    startedAt: "2026-07-22T20:05:00.000Z",
    updatedAt: "2026-07-22T20:09:00.000Z",
    terminalAt: null,
    ownerActorId: null,
    workKind: "process",
    command: null,
    cwd: null,
  } as unknown as ActivityWorkItemSummary;
}

function snapshot(overrides: Partial<ActivitySnapshot> = {}): ActivitySnapshot {
  return {
    protocolVersion: 2,
    scopeId: "scope-1",
    scope: { _tag: "thread", threadId: "thread-1" },
    revision: 1,
    provider: "codex",
    providerInstanceId: null,
    capabilities: {
      actors: true,
      attributedActivity: true,
      backgroundWork: true,
      historyRecovery: "full",
      terminalObservation: false,
      targetedActorCancellation: false,
    },
    observationState: "live",
    sections: {
      subagents: { state: "live", message: null, retryable: false },
      backgroundTasks: { state: "live", message: null, retryable: false },
    },
    counts: {
      subagents: { active: 2, done: 1 },
      backgroundTasks: { active: 1, done: 1 },
    },
    actors: [],
    workItems: [],
    actorsHasMore: false,
    workItemsHasMore: false,
    control: { scopeId: "scope-1", revision: 0, actors: [], operations: [] },
    updatedAt: "2026-07-22T20:09:00.000Z",
    ...overrides,
  } as unknown as ActivitySnapshot;
}

function props(overrides: Partial<ActivityDockProps> = {}): ActivityDockProps {
  return {
    snapshot: snapshot(),
    expanded: false,
    compact: false,
    onExpandedChange: vi.fn(),
    onOpenSection: vi.fn(),
    now: FIXED_NOW,
    ...overrides,
  };
}

function withoutSnapshotField(field: keyof ActivitySnapshot): unknown {
  const result = { ...snapshot() } as Record<string, unknown>;
  Reflect.deleteProperty(result, field);
  return result;
}

function propsWithoutNow(overrides: Partial<ActivityDockProps> = {}): ActivityDockProps {
  const result = props(overrides);
  Reflect.deleteProperty(result, "now");
  return result;
}

async function mount(element: ReactElement): Promise<HTMLDivElement> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  mounted.push({ root, container });
  await act(async () => root.render(element));
  return container;
}

async function rerender(container: HTMLDivElement, element: ReactElement): Promise<void> {
  const entry = mounted.find((candidate) => candidate.container === container);
  if (entry === undefined) {
    throw new Error("Cannot rerender an unmounted ActivityDock test tree.");
  }
  await act(async () => entry.root.render(element));
}

async function pressTab(from: HTMLElement, to: HTMLElement): Promise<KeyboardEvent> {
  const event = new KeyboardEvent("keydown", {
    key: "Tab",
    bubbles: true,
    cancelable: true,
  });
  await act(async () => {
    from.dispatchEvent(event);
    if (!event.defaultPrevented) {
      to.focus();
    }
    from.dispatchEvent(new KeyboardEvent("keyup", { key: "Tab", bubbles: true }));
  });
  return event;
}

async function activateNativeButton(
  target: HTMLButtonElement,
  key: "Enter" | " ",
): Promise<KeyboardEvent> {
  const keydown = new KeyboardEvent("keydown", {
    key,
    bubbles: true,
    cancelable: true,
  });
  await act(async () => {
    target.dispatchEvent(keydown);
    target.dispatchEvent(new KeyboardEvent("keyup", { key, bubbles: true }));
    if (!keydown.defaultPrevented) {
      target.dispatchEvent(new MouseEvent("click", { bubbles: true, detail: 0 }));
    }
  });
  return keydown;
}

function installReducedMotionPreference(matches: boolean): void {
  vi.spyOn(window, "matchMedia").mockImplementation(
    (query: string) =>
      ({
        matches: query === "(prefers-reduced-motion: reduce)" && matches,
        media: query,
        onchange: null,
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
        addListener: vi.fn(),
        removeListener: vi.fn(),
        dispatchEvent: vi.fn(() => true),
      }) as unknown as MediaQueryList,
  );
}

function installLiveReducedMotionPreference(initialMatches: boolean): {
  readonly listenerCount: () => number;
  readonly set: (matches: boolean) => void;
} {
  const query = "(prefers-reduced-motion: reduce)";
  const nativeMatchMedia = window.matchMedia.bind(window);
  const listeners = new Set<EventListener>();
  let matches = initialMatches;
  const mediaQuery = {
    get matches() {
      return matches;
    },
    media: query,
    onchange: null,
    addEventListener: (type: string, listener: EventListenerOrEventListenerObject) => {
      if (type === "change") listeners.add(listener as EventListener);
    },
    removeEventListener: (type: string, listener: EventListenerOrEventListenerObject) => {
      if (type === "change") listeners.delete(listener as EventListener);
    },
    addListener: (listener: (event: MediaQueryListEvent) => void) => {
      listeners.add(listener as EventListener);
    },
    removeListener: (listener: (event: MediaQueryListEvent) => void) => {
      listeners.delete(listener as EventListener);
    },
    dispatchEvent: (event: Event) => {
      for (const listener of listeners) listener(event);
      return !event.defaultPrevented;
    },
  } as MediaQueryList;
  vi.spyOn(window, "matchMedia").mockImplementation((media: string) =>
    media === query ? mediaQuery : nativeMatchMedia(media),
  );
  return {
    listenerCount: () => listeners.size,
    set: (nextMatches: boolean) => {
      matches = nextMatches;
      const event = new Event("change") as MediaQueryListEvent;
      Object.defineProperties(event, {
        matches: { value: nextMatches },
        media: { value: query },
      });
      mediaQuery.dispatchEvent(event);
    },
  };
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

afterEach(async () => {
  for (const entry of mounted.splice(0)) {
    await act(async () => entry.root.unmount());
    entry.container.remove();
  }
  Object.defineProperty(window, "innerWidth", {
    configurable: true,
    value: initialInnerWidth,
  });
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("ActivityDock", () => {
  it("renders no DOM when neither activity section is visible", async () => {
    const invisible = snapshot({
      capabilities: {
        actors: false,
        attributedActivity: false,
        backgroundWork: false,
        historyRecovery: "none",
        terminalObservation: false,
        targetedActorCancellation: false,
      },
      sections: {
        subagents: { state: "unsupported", message: null, retryable: false },
        backgroundTasks: { state: "unsupported", message: null, retryable: false },
      },
      counts: {
        subagents: { active: 0, done: 0 },
        backgroundTasks: { active: 0, done: 0 },
      },
    });

    const container = await mount(<ActivityDock {...props({ snapshot: invisible })} />);

    expect(container.childElementCount).toBe(0);
  });

  it.each([
    ["no loaded actors", [], false],
    ["one loaded actor", [actor("actor-a")], false],
    ["three loaded actors", [actor("actor-a"), actor("actor-b"), actor("actor-c")], false],
    ["truncated actors", [actor("actor-a"), actor("actor-b"), actor("actor-c")], true],
    ["count-only actors", [], true],
  ])(
    "renders one scope provider glyph regardless of %s",
    async (_caseName, actors, actorsHasMore) => {
      const container = await mount(
        <ActivityDock
          {...props({
            snapshot: snapshot({
              counts: {
                subagents: { active: 1, done: 2 },
                backgroundTasks: { active: 0, done: 0 },
              },
              actors,
              actorsHasMore,
            }),
          })}
        />,
      );

      expect(container.querySelectorAll("[data-activity-provider-glyph]")).toHaveLength(1);
      expect(container.querySelectorAll("[data-activity-glyph]")).toHaveLength(0);
      expect(container.textContent).not.toContain("+2");
      expect(container.textContent).toContain("Active 1");
      expect(container.textContent).toContain("Done 2");
    },
  );

  it("renders independent visible section buttons with exact counts and collapses before opening", async () => {
    const calls: string[] = [];
    const onExpandedChange = vi.fn((next: boolean) => calls.push(`expanded:${next}`));
    const onOpenSection = vi.fn((section: string) => calls.push(`open:${section}`));
    const container = await mount(
      <ActivityDock
        {...props({
          expanded: true,
          onExpandedChange,
          onOpenSection,
          snapshot: snapshot({
            actors: [actor("actor-a")],
            workItems: [workItem("work-a")],
          }),
        })}
      />,
    );

    const subagents = container.querySelector<HTMLButtonElement>(
      'button[data-activity-section="subagents"]',
    );
    const backgroundTasks = container.querySelector<HTMLButtonElement>(
      'button[data-activity-section="backgroundTasks"]',
    );
    expect(subagents?.textContent).toContain("Subagents");
    expect(subagents?.textContent).toContain("Active 2");
    expect(subagents?.textContent).toContain("Done 1");
    expect(backgroundTasks?.textContent).toContain("Background tasks");
    expect(backgroundTasks?.textContent).toContain("Active 1");
    expect(backgroundTasks?.textContent).toContain("Done 1");

    await act(async () => subagents?.click());
    expect(calls).toEqual(["expanded:false", "open:subagents"]);

    calls.length = 0;
    await act(async () => backgroundTasks?.click());
    expect(calls).toEqual(["expanded:false", "open:backgroundTasks"]);
  });

  it("separates expanded section counts from elapsed metadata", async () => {
    const container = await mount(
      <ActivityDock
        {...props({
          expanded: true,
          snapshot: snapshot({
            actors: [actor("actor-a")],
            workItems: [workItem("work-a")],
          }),
        })}
      />,
    );

    const subagentsPrimary = container.querySelector<HTMLElement>(
      '[data-activity-section-primary="subagents"]',
    );
    const subagentsCopy = container.querySelector<HTMLElement>(
      '[data-activity-section-copy="subagents"]',
    );
    const subagentsMetadata = container.querySelector<HTMLElement>(
      '[data-activity-section-metadata="subagents"]',
    );
    const backgroundTasksPrimary = container.querySelector<HTMLElement>(
      '[data-activity-section-primary="backgroundTasks"]',
    );
    const backgroundTasksCopy = container.querySelector<HTMLElement>(
      '[data-activity-section-copy="backgroundTasks"]',
    );
    const backgroundTasksMetadata = container.querySelector<HTMLElement>(
      '[data-activity-section-metadata="backgroundTasks"]',
    );

    expect(container.querySelectorAll("[data-activity-provider-glyph]")).toHaveLength(1);
    expect(container.querySelectorAll("[data-activity-glyph]")).toHaveLength(0);
    expect(subagentsPrimary?.textContent).toContain("Subagents");
    expect(subagentsPrimary?.textContent).toContain("Active 2");
    expect(subagentsPrimary?.textContent).toContain("Done 1");
    expect(subagentsMetadata?.textContent).toBe("10m");
    expect(subagentsPrimary?.contains(subagentsMetadata!)).toBe(false);
    expect(backgroundTasksPrimary?.textContent).toContain("Background tasks");
    expect(backgroundTasksPrimary?.textContent).toContain("Active 1");
    expect(backgroundTasksPrimary?.textContent).toContain("Done 1");
    expect(backgroundTasksMetadata?.textContent).toBe("5m");
    expect(backgroundTasksPrimary?.contains(backgroundTasksMetadata!)).toBe(false);

    for (const [copy, primary, metadata] of [
      [subagentsCopy, subagentsPrimary, subagentsMetadata],
      [backgroundTasksCopy, backgroundTasksPrimary, backgroundTasksMetadata],
    ] as const) {
      expect(copy?.className).toContain("flex-col");
      expect(primary?.parentElement).toBe(copy);
      expect(metadata?.parentElement).toBe(copy);
    }
  });

  it("retains stale/reconnecting counts and labels the card as stale", async () => {
    const container = await mount(
      <ActivityDock
        {...props({
          expanded: true,
          snapshot: snapshot({
            capabilities: {
              actors: true,
              attributedActivity: true,
              backgroundWork: true,
              historyRecovery: "bounded",
              terminalObservation: false,
              targetedActorCancellation: false,
            },
            observationState: "reconnecting",
            sections: {
              subagents: { state: "live", message: null, retryable: false },
              backgroundTasks: { state: "live", message: null, retryable: false },
            },
            counts: {
              subagents: { active: 2, done: 5 },
              backgroundTasks: { active: 1, done: 1 },
            },
          }),
        })}
      />,
    );

    const card = container.querySelector('[aria-label="Activity data stale"]');
    expect(card).not.toBeNull();
    expect(card?.textContent).toContain("Active 2");
    expect(card?.textContent).toContain("Done 5");
  });

  it.each([
    ["null snapshot", null],
    ["undefined snapshot", undefined],
    ["missing capabilities", withoutSnapshotField("capabilities")],
    ["null capabilities", { ...snapshot(), capabilities: null }],
    ["missing sections", withoutSnapshotField("sections")],
    ["null sections", { ...snapshot(), sections: null }],
    ["missing counts", withoutSnapshotField("counts")],
    ["null counts", { ...snapshot(), counts: null }],
    ["missing actors", withoutSnapshotField("actors")],
    ["null actors", { ...snapshot(), actors: null }],
    ["missing work items", withoutSnapshotField("workItems")],
    ["null work items", { ...snapshot(), workItems: null }],
  ])("renders no DOM for a structurally unusable %s", async (_label, malformed) => {
    const container = await mount(
      <ActivityDock
        {...props({
          snapshot: malformed as ActivitySnapshot,
        })}
      />,
    );

    expect(container.childElementCount).toBe(0);
  });

  it("sanitizes malformed leaves and filters malformed records without throwing", async () => {
    const malformed = {
      ...snapshot(),
      provider: { unexpected: true },
      observationState: "unexpected",
      updatedAt: 42,
      capabilities: {
        actors: true,
        attributedActivity: "yes",
        backgroundWork: true,
        historyRecovery: "unexpected",
        terminalObservation: null,
      },
      sections: {
        subagents: { state: "unexpected", message: 42, retryable: "yes" },
        backgroundTasks: { state: "live", message: null, retryable: false },
      },
      counts: {
        subagents: { active: Number.NaN, done: "2" },
        backgroundTasks: { active: -4, done: 3.8 },
      },
      actors: [null, {}, { ...actor("wrong-id"), id: 42 }, actor("valid-actor", "Valid")],
      workItems: [undefined, { id: "missing-fields" }, workItem("valid-work", "Valid work")],
    } as unknown as ActivitySnapshot;

    const container = await mount(
      <ActivityDock {...propsWithoutNow({ expanded: true, snapshot: malformed })} />,
    );

    expect(container.querySelector('[data-activity-section="subagents"]')).toBeNull();
    expect(container.querySelector('[data-activity-section="backgroundTasks"]')).not.toBeNull();
    expect(container.querySelector('[data-activity-glyph="valid-actor"]')).toBeNull();
    expect(container.textContent).toContain("Active 0");
    expect(container.textContent).toContain("Done 3");
    expect(container.textContent).not.toContain("NaN");
    expect(container.textContent).not.toContain("Infinity");
    expect(
      container.querySelector('[data-activity-section-metadata="backgroundTasks"]')?.textContent,
    ).toBe("0s");

    await rerender(container, <ActivityDock {...propsWithoutNow({ snapshot: malformed })} />);
    expect(container.querySelector('[data-activity-provider-glyph="unknown"]')).not.toBeNull();
  });

  it("marks only an errored background section when the global observation is live", async () => {
    const container = await mount(
      <ActivityDock
        {...props({
          expanded: true,
          snapshot: snapshot({
            observationState: "live",
            sections: {
              subagents: { state: "live", message: null, retryable: false },
              backgroundTasks: {
                state: "error",
                message: "Background process history failed.",
                retryable: true,
              },
            },
          }),
        })}
      />,
    );
    const subagents = container.querySelector<HTMLButtonElement>(
      'button[data-activity-section="subagents"]',
    );
    const backgroundTasks = container.querySelector<HTMLButtonElement>(
      'button[data-activity-section="backgroundTasks"]',
    );
    const liveRegion = container.querySelector<HTMLElement>('[aria-live="polite"]');

    expect(container.querySelector('[aria-label="Activity data stale"]')).toBeNull();
    expect(subagents?.getAttribute("aria-label")).toBe("Open Subagents: 2 active, 1 done");
    expect(subagents?.querySelector("[data-activity-section-status]")).toBeNull();
    expect(backgroundTasks?.getAttribute("aria-label")).toBe(
      "Open Background tasks: 1 active, 1 done. Status: error",
    );
    expect(backgroundTasks?.querySelector('[data-activity-section-status="error"]')).not.toBeNull();
    expect(liveRegion?.textContent).toContain("Background tasks error");
    expect(liveRegion?.textContent).not.toContain("Subagents stale");
    expect(liveRegion?.textContent).not.toContain("Subagents error");
  });

  it("marks only a stale subagent section when the global observation is live", async () => {
    const container = await mount(
      <ActivityDock
        {...props({
          expanded: true,
          snapshot: snapshot({
            observationState: "live",
            sections: {
              subagents: {
                state: "stale",
                message: "Actor history is reconnecting.",
                retryable: true,
              },
              backgroundTasks: { state: "live", message: null, retryable: false },
            },
          }),
        })}
      />,
    );
    const subagents = container.querySelector<HTMLButtonElement>(
      'button[data-activity-section="subagents"]',
    );
    const backgroundTasks = container.querySelector<HTMLButtonElement>(
      'button[data-activity-section="backgroundTasks"]',
    );
    const liveRegion = container.querySelector<HTMLElement>('[aria-live="polite"]');

    expect(container.querySelector('[aria-label="Activity data stale"]')).toBeNull();
    expect(subagents?.getAttribute("aria-label")).toBe(
      "Open Subagents: 2 active, 1 done. Status: stale",
    );
    expect(subagents?.querySelector('[data-activity-section-status="stale"]')).not.toBeNull();
    expect(backgroundTasks?.getAttribute("aria-label")).toBe(
      "Open Background tasks: 1 active, 1 done",
    );
    expect(backgroundTasks?.querySelector("[data-activity-section-status]")).toBeNull();
    expect(liveRegion?.textContent).toContain("Subagents stale");
    expect(liveRegion?.textContent).not.toContain("Background tasks stale");
    expect(liveRegion?.textContent).not.toContain("Background tasks error");
  });

  it("gives the native toggle count-aware accessible names and aria-expanded state", async () => {
    const onExpandedChange = vi.fn();
    const container = await mount(
      <ActivityDock {...props({ expanded: false, onExpandedChange })} />,
    );
    const toggle = container.querySelector<HTMLButtonElement>("button");

    expect(toggle?.getAttribute("aria-expanded")).toBe("false");
    expect(toggle?.getAttribute("aria-label")).toBe(
      "Expand activity summary: 2 active subagents, 1 done subagent, 1 active background task, 1 done background task",
    );
    expect(toggle?.type).toBe("button");

    await act(async () => toggle?.click());
    expect(onExpandedChange).toHaveBeenCalledWith(true);
  });

  it("uses Escape to collapse only the expanded dock without reaching its surrounding panel", async () => {
    const onExpandedChange = vi.fn();
    const onSurroundingKeyDown = vi.fn();
    const container = await mount(
      <div onKeyDown={onSurroundingKeyDown}>
        <ActivityDock {...props({ expanded: true, onExpandedChange })} />
      </div>,
    );
    const sectionButton = container.querySelector<HTMLButtonElement>(
      'button[data-activity-section="subagents"]',
    );
    const escape = new KeyboardEvent("keydown", {
      key: "Escape",
      bubbles: true,
      cancelable: true,
    });

    await act(async () => sectionButton?.dispatchEvent(escape));

    expect(escape.defaultPrevented).toBe(true);
    expect(onExpandedChange).toHaveBeenCalledWith(false);
    expect(onSurroundingKeyDown).not.toHaveBeenCalled();
  });

  it("uses one native, tabbable button per control with single Enter/Space activation", async () => {
    const onExpandedChange = vi.fn();
    const onOpenSection = vi.fn();
    const container = await mount(
      <ActivityDock {...props({ expanded: true, onExpandedChange, onOpenSection })} />,
    );
    const buttons = Array.from(container.querySelectorAll<HTMLButtonElement>("button"));
    const [toggle, subagents, backgroundTasks] = buttons;

    expect(buttons).toHaveLength(3);
    expect(buttons.every((button) => button.type === "button" && button.tabIndex === 0)).toBe(true);
    expect(subagents?.getAttribute("aria-label")).toBe("Open Subagents: 2 active, 1 done");
    expect(backgroundTasks?.getAttribute("aria-label")).toBe(
      "Open Background tasks: 1 active, 1 done",
    );

    toggle?.focus();
    const tab = await pressTab(toggle!, subagents!);
    expect(tab.defaultPrevented).toBe(false);
    expect(document.activeElement).toBe(subagents);

    const enter = await activateNativeButton(subagents!, "Enter");
    expect(enter.defaultPrevented).toBe(false);
    expect(onExpandedChange).toHaveBeenCalledTimes(1);
    expect(onOpenSection).toHaveBeenCalledTimes(1);
    expect(onOpenSection).toHaveBeenLastCalledWith("subagents");

    onExpandedChange.mockClear();
    onOpenSection.mockClear();
    const space = await activateNativeButton(backgroundTasks!, " ");
    expect(space.defaultPrevented).toBe(false);
    expect(onExpandedChange).toHaveBeenCalledTimes(1);
    expect(onOpenSection).toHaveBeenCalledTimes(1);
    expect(onOpenSection).toHaveBeenLastCalledWith("backgroundTasks");
  });

  it("keeps one polite live region stable across elapsed ticks and updates it for count changes", async () => {
    vi.useFakeTimers();
    const initialSnapshot = snapshot({ actors: [actor("actor-a", "Alpha")] });
    const initialProps = props({ expanded: true, snapshot: initialSnapshot });
    const container = await mount(<ActivityDock {...initialProps} />);
    const liveRegion = container.querySelector<HTMLElement>('[aria-live="polite"]');

    expect(container.querySelectorAll('[aria-live="polite"]')).toHaveLength(1);
    expect(liveRegion?.getAttribute("role")).toBe("status");
    expect(liveRegion?.textContent).toBe(
      "Activity update: 2 active subagents, 1 done subagent, 1 active background task, 1 done background task",
    );
    const initialAnnouncement = liveRegion?.textContent;
    expect(
      container.querySelector('[data-activity-section-metadata="subagents"]')?.textContent,
    ).toBe("10m");

    await rerender(container, <ActivityDock {...initialProps} now="2026-07-22T20:11:00.000Z" />);
    const afterTick = container.querySelector<HTMLElement>('[aria-live="polite"]');
    expect(afterTick).toBe(liveRegion);
    expect(afterTick?.textContent).toBe(initialAnnouncement);
    expect(
      container.querySelector('[data-activity-section-metadata="subagents"]')?.textContent,
    ).toBe("11m");

    const changedSnapshot = snapshot({
      ...initialSnapshot,
      revision: 2,
      counts: {
        subagents: { active: 3, done: 1 },
        backgroundTasks: { active: 1, done: 1 },
      },
    });
    await rerender(container, <ActivityDock {...initialProps} snapshot={changedSnapshot} />);
    expect(container.querySelector('[aria-live="polite"]')).toBe(liveRegion);
    await act(async () => vi.advanceTimersByTimeAsync(500));
    expect(liveRegion?.textContent).toContain("3 active subagents");
  });

  it("coalesces rapid live announcements to the latest activity counts once per 500ms window", async () => {
    vi.useFakeTimers();
    const initialProps = props({ expanded: true });
    const container = await mount(<ActivityDock {...initialProps} />);
    const liveRegion = container.querySelector<HTMLElement>('[aria-live="polite"]')!;
    const initialAnnouncement = liveRegion.textContent;
    const mutations: MutationRecord[] = [];
    const observer = new MutationObserver((records) => mutations.push(...records));
    observer.observe(liveRegion, { childList: true, characterData: true, subtree: true });

    for (const active of [3, 4, 5]) {
      await rerender(
        container,
        <ActivityDock
          {...initialProps}
          snapshot={snapshot({
            revision: active,
            counts: {
              subagents: { active, done: 1 },
              backgroundTasks: { active: 1, done: 1 },
            },
          })}
        />,
      );
    }

    expect(liveRegion.textContent).toBe(initialAnnouncement);
    expect(mutations).toHaveLength(0);
    await act(async () => vi.advanceTimersByTimeAsync(500));
    expect(liveRegion.textContent).toContain("5 active subagents");
    expect(mutations.length).toBeLessThanOrEqual(1);
    observer.disconnect();
  });

  it("keeps expanded labels and elapsed metadata readable when the collapsed dock is compact", async () => {
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 700 });
    const addEventListener = vi.spyOn(window, "addEventListener");
    const compactProps = props({
      compact: true,
      expanded: true,
      snapshot: snapshot({
        actors: [actor("actor-1")],
        workItems: [workItem("work-1")],
      }),
    });
    const container = await mount(<ActivityDock {...compactProps} />);
    const sectionButtons = Array.from(
      container.querySelectorAll<HTMLButtonElement>("button[data-activity-section]"),
    );

    expect(sectionButtons).toHaveLength(2);
    expect(sectionButtons.every((button) => button.className.includes("min-h-9"))).toBe(true);
    expect(sectionButtons.every((button) => button.className.includes("sm:min-h-8"))).toBe(true);
    expect(sectionButtons.every((button) => !button.className.includes("sm:h-7"))).toBe(true);
    expect(
      container.querySelector('button[aria-label^="Collapse activity summary"]')?.textContent,
    ).toContain("Activity");
    expect(sectionButtons[0]?.textContent).toContain("Subagents");
    expect(sectionButtons[0]?.textContent).toContain("Active 2");
    expect(sectionButtons[0]?.textContent).toContain("Done 1");
    expect(sectionButtons[0]?.textContent).toContain("10m");
    expect(sectionButtons[1]?.textContent).toContain("Background tasks");
    expect(sectionButtons[1]?.textContent).toContain("Active 1");
    expect(sectionButtons[1]?.textContent).toContain("Done 1");
    expect(sectionButtons[1]?.textContent).toContain("5m");
    expect(container.querySelectorAll("[data-activity-section-metadata]")).toHaveLength(2);
    expect(addEventListener.mock.calls.some(([type]) => type === "resize")).toBe(false);

    const collapsed = await mount(<ActivityDock {...props({ compact: true, expanded: false })} />);
    const toggle = collapsed.querySelector<HTMLButtonElement>(
      'button[aria-label^="Expand activity summary"]',
    );
    expect(toggle?.querySelectorAll("[data-activity-provider-glyph]")).toHaveLength(1);
    expect(toggle?.textContent).toContain("Active 3");
    expect(toggle?.textContent).toContain("·");
    expect(toggle?.textContent).toContain("Done 2");
    const separator = Array.from(toggle?.querySelectorAll<HTMLSpanElement>("span") ?? []).find(
      (span) => span.textContent === "·",
    );
    expect(separator?.textContent).toBe("·");
    expect(separator?.classList.contains("text-base")).toBe(true);
    expect(separator?.classList.contains("font-bold")).toBe(true);
    expect(separator?.classList.contains("leading-none")).toBe(true);
    expect(separator?.classList.contains("text-foreground")).toBe(true);
    expect(separator?.classList.contains("text-foreground/70")).toBe(false);
    expect(separator?.classList.contains("font-medium")).toBe(false);
    expect(separator?.classList.contains("text-muted-foreground")).toBe(false);
    expect(separator?.getAttribute("data-activity-count-separator")).toBe("true");
    expect(separator?.getAttribute("aria-hidden")).toBe("true");
    expect(toggle?.querySelector(".lucide-loader-circle")).toBeNull();
    expect(toggle?.querySelector(".lucide-circle-check-big")).toBeNull();
  });

  it("saturates unsafe finite counts and never renders overflow, Infinity, or NaN", async () => {
    const unsafeSnapshot = snapshot({
      counts: {
        subagents: {
          active: Number.MAX_SAFE_INTEGER + 100,
          done: Number.NaN,
        },
        backgroundTasks: {
          active: 1,
          done: Number.POSITIVE_INFINITY,
        },
      },
    } as Partial<ActivitySnapshot>);
    const container = await mount(<ActivityDock {...props({ snapshot: unsafeSnapshot })} />);
    const toggle = container.querySelector<HTMLButtonElement>("button");

    expect(toggle?.textContent).toContain(`Active ${Number.MAX_SAFE_INTEGER}`);
    expect(toggle?.textContent).toContain("Done 0");
    expect(toggle?.textContent).not.toContain("Infinity");
    expect(toggle?.textContent).not.toContain("NaN");
    expect(toggle?.getAttribute("aria-label")).toContain(
      `${Number.MAX_SAFE_INTEGER} active subagents`,
    );
  });

  it("never exposes unsupported section rows or actor glyphs while another section remains visible", async () => {
    const backgroundOnly = snapshot({
      capabilities: {
        actors: false,
        attributedActivity: false,
        backgroundWork: true,
        historyRecovery: "bounded",
        terminalObservation: false,
        targetedActorCancellation: false,
      },
      sections: {
        subagents: { state: "unsupported", message: null, retryable: false },
        backgroundTasks: { state: "live", message: null, retryable: false },
      },
      counts: {
        subagents: { active: 1, done: 1 },
        backgroundTasks: { active: 2, done: 3 },
      },
      actors: [actor("unsupported-actor")],
      workItems: [workItem("work-a")],
    });
    const backgroundOnlyProps = props({ expanded: false, snapshot: backgroundOnly });
    const container = await mount(<ActivityDock {...backgroundOnlyProps} />);

    expect(container.querySelector("[data-activity-glyph]")).toBeNull();
    await rerender(container, <ActivityDock {...backgroundOnlyProps} expanded />);
    expect(container.querySelector('[data-activity-section="subagents"]')).toBeNull();
    expect(container.querySelector('[data-activity-section="backgroundTasks"]')).not.toBeNull();
    expect(container.textContent).not.toContain("unsupported-actor");
  });

  it("shows the mapped provider glyph when exact counts exceed the loaded actor summaries", async () => {
    const container = await mount(<ActivityDock {...props()} />);
    const glyph = container.querySelector<HTMLElement>('[data-activity-provider-glyph="codex"]');
    const icon = glyph?.querySelector("svg");

    expect(glyph).not.toBeNull();
    expect(icon?.getAttribute("viewBox")).toBe("0 0 256 260");
    expect(icon?.classList.contains("lucide-bot")).toBe(false);
    expect(container.querySelector("[data-activity-glyph]")).toBeNull();
  });

  it.each(["constructor", "toString", "ordinary-unknown-provider"])(
    "uses the exact Bot fallback for the unknown provider slug %s",
    async (provider) => {
      const unknownProviderSnapshot = snapshot({
        provider: provider as ActivitySnapshot["provider"],
      });

      const container = await mount(
        <ActivityDock {...props({ snapshot: unknownProviderSnapshot })} />,
      );
      const glyph = container.querySelector<HTMLElement>(
        `[data-activity-provider-glyph="${provider}"]`,
      );
      const icon = glyph?.querySelector("svg");

      expect(glyph).not.toBeNull();
      expect(icon?.classList.contains("lucide-bot")).toBe(true);
      expect(icon?.getAttribute("viewBox")).toBe("0 0 24 24");
    },
  );

  it("keeps exact counts when actor summaries are truncated without a glyph overflow badge", async () => {
    const truncatedSnapshot = snapshot({
      counts: {
        subagents: { active: 8, done: 3 },
        backgroundTasks: { active: 0, done: 0 },
      },
      actors: [
        actor("actor-f"),
        actor("actor-b"),
        actor("actor-d"),
        actor("actor-a"),
        actor("actor-e"),
        actor("actor-c"),
      ],
      actorsHasMore: true,
    });
    const dockProps = props({ snapshot: truncatedSnapshot });
    const container = await mount(<ActivityDock {...dockProps} />);

    expect(container.querySelectorAll("[data-activity-provider-glyph]")).toHaveLength(1);
    expect(container.querySelectorAll("[data-activity-glyph]")).toHaveLength(0);
    expect(container.textContent).not.toContain("+7");
    expect(container.textContent).toContain("Active 8");
    expect(container.textContent).toContain("Done 3");

    await rerender(
      container,
      <ActivityDock {...dockProps} snapshot={snapshot({ ...truncatedSnapshot, actors: [] })} />,
    );
    expect(container.querySelectorAll("[data-activity-provider-glyph]")).toHaveLength(1);
    expect(container.querySelectorAll("[data-activity-glyph]")).toHaveLength(0);
    expect(container.textContent).not.toContain("+11");
  });

  it("uses bounded tokenized placement, focus, truncation, and reduced-motion classes", async () => {
    installReducedMotionPreference(false);
    const container = await mount(<ActivityDock {...props({ expanded: true })} />);
    const placement = container.querySelector<HTMLElement>('[data-testid="activity-dock"]');
    const card = placement?.firstElementChild as HTMLElement | null;
    const buttons = Array.from(container.querySelectorAll<HTMLButtonElement>("button"));

    expect(placement?.className).toContain("pointer-events-none");
    expect(placement?.className).toContain("absolute");
    expect(placement?.className).toContain("top-3");
    expect(placement?.className).toContain("right-3");
    expect(placement?.className).toContain("z-20");
    expect(card?.getAttribute("role")).toBe("group");
    expect(card?.className).toContain("pointer-events-auto");
    expect(card?.className.split(/\s+/u)).toContain("w-72");
    expect(card?.className).toContain("max-w-72");
    expect(card?.className).toContain("border-border");
    expect(card?.className).toContain("bg-popover");
    expect(card?.className).toContain("text-popover-foreground");
    expect(card?.className).not.toContain("bg-white");
    expect(card?.className).toContain("transition-[width,opacity]");
    expect(buttons.every((button) => button.className.includes("min-h-9"))).toBe(true);
    expect(buttons.every((button) => button.className.includes("focus-visible:ring-2"))).toBe(true);
    expect(
      container.querySelector('button[data-activity-section="subagents"] .truncate'),
    ).not.toBeNull();
    expect(
      Array.from(container.querySelectorAll<HTMLElement>(".tabular-nums")).every(
        (count) => !count.className.includes("transition"),
      ),
    ).toBe(true);
  });

  it("uses the shared responsive sheet width as an inset when the sheet is open", async () => {
    installReducedMotionPreference(false);
    const container = await mount(
      <ActivityDock {...props({ avoidRightPanelSheet: true, compact: true })} />,
    );
    const placement = container.querySelector<HTMLElement>('[data-testid="activity-dock"]');

    expect(placement?.className).toContain("right-[calc(min(42vw,28rem)+0.75rem)]");
    expect(placement?.className).toContain("max-[760px]:right-[calc(min(88vw,24rem)+0.75rem)]");
    expect(placement?.className.split(/\s+/u)).not.toContain("right-3");
  });

  it.each([
    ["normal", false, true],
    ["reduced", true, false],
  ] as const)(
    "uses the live %s motion preference to control activity width/count transitions",
    async (motion, reduced, transitions) => {
      installReducedMotionPreference(reduced);

      const container = await mount(<ActivityDock {...props({ expanded: true })} />);
      const card = container.querySelector<HTMLElement>(
        '[data-testid="activity-dock"] > [role="group"]',
      );
      const counts = Array.from(container.querySelectorAll<HTMLElement>(".tabular-nums"));

      expect(card?.dataset.activityMotion).toBe(motion);
      expect(card?.className.includes("transition-[width,opacity]")).toBe(transitions);
      expect(counts.every((count) => !count.className.includes("transition"))).toBe(true);
    },
  );

  it("reacts to live reduced-motion changes and unsubscribes on unmount", async () => {
    const preference = installLiveReducedMotionPreference(false);
    const container = await mount(<ActivityDock {...props({ expanded: true })} />);
    const card = container.querySelector<HTMLElement>(
      '[data-testid="activity-dock"] > [role="group"]',
    )!;

    expect(card.dataset.activityMotion).toBe("normal");
    expect(card.className).toContain("transition-[width,opacity]");
    expect(preference.listenerCount()).toBe(1);

    await act(async () => preference.set(true));
    expect(card.dataset.activityMotion).toBe("reduced");
    expect(card.className).not.toContain("transition-[width,opacity]");

    await act(async () => preference.set(false));
    expect(card.dataset.activityMotion).toBe("normal");
    expect(card.className).toContain("transition-[width,opacity]");

    const index = mounted.findIndex((entry) => entry.container === container);
    const entry = mounted[index]!;
    mounted.splice(index, 1);
    await act(async () => entry.root.unmount());
    entry.container.remove();
    expect(preference.listenerCount()).toBe(0);
  });
});
