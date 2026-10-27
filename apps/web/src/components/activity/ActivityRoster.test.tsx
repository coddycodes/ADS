// @vitest-environment happy-dom

import type {
  ActivityActorControl,
  ActivityActorSummary,
  ActivityRecordSummary,
  ActivitySnapshot,
  ActivityWorkItemSummary,
} from "@coddy/contracts";
import { act, type ReactElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { TooltipProvider } from "~/components/ui/tooltip";
import {
  ActivityRoster,
  type ActivityRosterProps,
  projectActivityRosterHierarchy,
} from "./ActivityRoster";
import type { ActivityRosterPageData } from "./ActivityPanel";

const mounted: Array<{ readonly root: Root; readonly container: HTMLDivElement }> = [];

function actor(
  id: string,
  name: string,
  overrides: Partial<Omit<ActivityActorSummary, "parentActorId">> & {
    readonly parentActorId?: string | null;
  } = {},
): ActivityActorSummary {
  return {
    _tag: "actor",
    id,
    name,
    status: "running",
    summary: null,
    startedAt: "2026-08-11T20:00:00.000Z",
    updatedAt: "2026-08-11T20:01:00.000Z",
    terminalAt: null,
    parentActorId: null,
    role: "reviewer",
    providerType: "codex",
    ...overrides,
  } as ActivityActorSummary;
}

function control(
  actorId: string,
  state: ActivityActorControl["state"],
  activeDescendantCount = 0,
  controlRevision = 3,
): ActivityActorControl {
  return { actorId, state, activeDescendantCount, controlRevision } as ActivityActorControl;
}

function workItem(
  id: string,
  overrides: Partial<Omit<ActivityWorkItemSummary, "ownerActorId">> & {
    readonly ownerActorId?: string | null;
  } = {},
): ActivityWorkItemSummary {
  return {
    _tag: "workItem",
    id,
    name: id,
    status: "running",
    summary: null,
    startedAt: "2026-08-11T20:00:00.000Z",
    updatedAt: "2026-08-11T20:01:00.000Z",
    terminalAt: null,
    ownerActorId: null,
    workKind: "process",
    command: null,
    cwd: null,
    ...overrides,
  } as ActivityWorkItemSummary;
}

function projectHierarchy(
  records: ReadonlyArray<ActivityRecordSummary>,
  section: "subagents" | "backgroundTasks",
): ReturnType<typeof projectActivityRosterHierarchy> {
  return projectActivityRosterHierarchy(records, section);
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
      backgroundWork: false,
      historyRecovery: "full",
      terminalObservation: false,
      targetedActorCancellation: true,
    },
    observationState: "live",
    sections: {
      subagents: { state: "live", message: null, retryable: false },
      backgroundTasks: { state: "unsupported", message: null, retryable: false },
    },
    counts: {
      subagents: { active: 3, done: 1 },
      backgroundTasks: { active: 0, done: 0 },
    },
    actors: [],
    workItems: [],
    actorsHasMore: false,
    workItemsHasMore: false,
    control: { scopeId: "scope-1", revision: 4, actors: [], operations: [] },
    updatedAt: "2026-08-11T20:02:00.000Z",
    ...overrides,
  } as ActivitySnapshot;
}

function page(
  records: ActivityRosterPageData["records"],
  actorControls: readonly ActivityActorControl[] = [],
): ActivityRosterPageData {
  return { records, actorControls, nextCursor: null } as ActivityRosterPageData;
}

function props(overrides: Partial<ActivityRosterProps> = {}): ActivityRosterProps {
  const available = actor("available", "Lovelace");
  const requested = actor("requested", "Turing");
  const unsupported = actor("unsupported", "Hopper");
  const done = actor("done", "Hamilton", {
    status: "completed",
    terminalAt: "2026-08-11T20:02:00.000Z",
  });
  const activePage = page(
    [available, requested, unsupported],
    [
      control(available.id, "available", 2),
      control(requested.id, "requested", 1),
      control(unsupported.id, "unsupported"),
    ],
  );
  const donePage = page([done], [control(done.id, "available")]);
  return {
    section: "subagents",
    snapshot: snapshot(),
    active: { pages: [activePage], loading: false, error: null },
    done: { pages: [donePage], loading: false, error: null },
    reconciled: { active: [available, requested, unsupported], done: [done] },
    now: "2026-08-11T20:02:00.000Z",
    notification: null,
    focusRecordId: null,
    onFocusRestored: vi.fn(),
    onSelect: vi.fn(),
    onLoadMore: vi.fn(),
    onCancelActor: vi.fn(),
    ...overrides,
  };
}

async function mount(element: ReactElement): Promise<HTMLDivElement> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  mounted.push({ root, container });
  await act(async () => {
    root.render(<TooltipProvider delay={0}>{element}</TooltipProvider>);
  });
  return container;
}

async function rerender(container: HTMLDivElement, element: ReactElement): Promise<void> {
  const tree = mounted.find((candidate) => candidate.container === container);
  if (tree === undefined) {
    throw new Error("Cannot rerender an unmounted ActivityRoster test tree.");
  }
  await act(async () => {
    tree.root.render(<TooltipProvider delay={0}>{element}</TooltipProvider>);
  });
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  Object.defineProperty(Element.prototype, "getAnimations", {
    configurable: true,
    value: () => [],
  });
});

afterEach(async () => {
  for (const tree of mounted.splice(0)) {
    await act(async () => tree.root.unmount());
    tree.container.remove();
  }
  document.querySelectorAll('[data-slot="tooltip-positioner"]').forEach((node) => node.remove());
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
  vi.restoreAllMocks();
});

describe("ActivityRoster targeted cancellation controls", () => {
  it("renders sibling Stop controls only for active server-controlled actors", async () => {
    const onCancelActor = vi.fn();
    const onSelect = vi.fn();
    const container = await mount(<ActivityRoster {...props({ onCancelActor, onSelect })} />);

    const availableStop = container.querySelector<HTMLButtonElement>(
      'button[aria-label="Stop Lovelace and 2 child agents"]',
    );
    const requestedStop = container.querySelector<HTMLButtonElement>(
      'button[aria-label="Stop Turing and 1 child agent"]',
    );
    expect(availableStop).not.toBeNull();
    expect(availableStop?.textContent).toBe("Stop subtree");
    expect(requestedStop?.disabled).toBe(true);
    expect(requestedStop?.textContent).toBe("Stopping");
    expect(container.textContent).toContain("Stopping");
    expect(container.querySelector('button[aria-label^="Stop Hopper"]')).toBeNull();
    expect(container.querySelector('button[aria-label^="Stop Hamilton"]')).toBeNull();

    const detailButton = container.querySelector<HTMLButtonElement>(
      'button[data-activity-row="available"]',
    );
    expect(detailButton).not.toBeNull();
    expect(detailButton?.parentElement).toBe(availableStop?.parentElement);
    expect(detailButton?.contains(availableStop)).toBe(false);
    expect(availableStop?.className).toContain("focus-visible:ring-2");
    expect(availableStop?.className).toContain("shrink-0");
    expect(detailButton?.className).toContain("min-w-0");
    expect(detailButton?.className).toContain("flex-1");
    expect(detailButton?.parentElement?.className).toContain("min-w-0");
    const providerGlyph = detailButton?.querySelector<HTMLElement>(
      "[data-activity-provider-glyph]",
    );
    expect(detailButton?.querySelectorAll("[data-activity-provider-glyph]")).toHaveLength(1);
    expect(providerGlyph?.className).toContain("shrink-0");
    expect(detailButton?.querySelector("[data-activity-record-glyph='actor']")).toBeNull();

    availableStop?.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    await act(async () => availableStop?.click());
    expect(onCancelActor).toHaveBeenCalledWith("available", 3);
    expect(onSelect).not.toHaveBeenCalled();

    await act(async () => detailButton?.click());
    expect(onSelect).toHaveBeenCalledWith(expect.objectContaining({ id: "available" }));
  });

  it("explains an active actor whose exact Stop target is unavailable", async () => {
    const container = await mount(<ActivityRoster {...props()} />);

    const unavailable = container.querySelector(
      '[data-activity-control-unavailable="unsupported"]',
    );
    expect(unavailable?.textContent).toBe("Stop unavailable");
    expect(unavailable?.closest("button")).toBeNull();
    expect(
      container.querySelector('button[data-activity-row="unsupported"]')?.textContent,
    ).toContain("Running");
    expect(container.querySelector('[data-activity-control-unavailable="done"]')).toBeNull();
  });

  it("is keyboard reachable and exposes the same exact subtree impact in its tooltip", async () => {
    const container = await mount(<ActivityRoster {...props()} />);
    const stop = container.querySelector<HTMLButtonElement>(
      'button[aria-label="Stop Lovelace and 2 child agents"]',
    );
    expect(stop?.tabIndex).toBe(0);

    await act(async () => {
      stop?.focus();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(document.activeElement).toBe(stop);
    expect(
      [...document.body.querySelectorAll('[data-slot="tooltip-popup"]')].some(
        (popup) => popup.textContent === "Stop Lovelace and 2 child agents",
      ),
    ).toBe(true);
  });

  it("renders parent, leaf, and requested text actions with exact server-authoritative impact", async () => {
    const parent = actor("parent-action", "Alpha");
    const leaf = actor("leaf-action", "Beta", { parentActorId: parent.id });
    const requested = actor("requested-action", "Gamma");
    const activePage = page(
      [leaf, requested, parent],
      [
        control(parent.id, "available", 1, 4),
        control(leaf.id, "available", 0, 5),
        control(requested.id, "requested", 1, 6),
      ],
    );
    const container = await mount(
      <ActivityRoster
        {...props({
          active: { pages: [activePage], loading: false, error: null },
          done: { pages: [page([])], loading: false, error: null },
          reconciled: { active: [leaf, requested, parent], done: [] },
        })}
      />,
    );

    const parentStop = container.querySelector<HTMLButtonElement>(
      'button[aria-label="Stop Alpha and 1 child agent"]',
    );
    const leafStop = container.querySelector<HTMLButtonElement>('button[aria-label="Stop Beta"]');
    const requestedStop = container.querySelector<HTMLButtonElement>(
      'button[aria-label="Stop Gamma and 1 child agent"]',
    );
    expect(parentStop?.textContent).toBe("Stop subtree");
    expect(leafStop?.textContent).toBe("Stop");
    expect(requestedStop?.textContent).toBe("Stopping");
    expect(requestedStop?.disabled).toBe(true);

    const rows = Array.from(container.querySelectorAll<HTMLElement>("[data-activity-row-layout]"));
    expect(rows.map((row) => row.dataset.activityRowLayout)).toEqual([
      "requested-action",
      "parent-action",
      "leaf-action",
    ]);
    expect(rows.map((row) => row.dataset.activityHierarchyDepth)).toEqual(["0", "0", "1"]);
    expect(
      rows[2]?.querySelector('[data-activity-hierarchy-connector="leaf-action"]'),
    ).not.toBeNull();
    expect(rows[2]?.textContent).toContain("reviewer");
    expect(rows[2]?.textContent).toContain("Child agent");
  });

  it("uses roster-page controls only as fallback and gives streamed snapshot controls precedence", async () => {
    const fallbackActor = actor("fallback", "Ada");
    const overriddenActor = actor("overridden", "Grace");
    const activePage = page(
      [fallbackActor, overriddenActor],
      [control(overriddenActor.id, "requested", 0, 8)],
    );
    const customSnapshot = snapshot({
      counts: {
        subagents: { active: 2, done: 0 },
        backgroundTasks: { active: 0, done: 0 },
      },
      control: {
        scopeId: "scope-1" as ActivitySnapshot["scopeId"],
        revision: 9,
        actors: [
          control(fallbackActor.id, "available", 0, 7),
          control(overriddenActor.id, "available", 0, 7),
        ],
        operations: [],
      },
    });
    const container = await mount(
      <ActivityRoster
        {...props({
          snapshot: customSnapshot,
          active: { pages: [activePage], loading: false, error: null },
          done: { pages: [page([])], loading: false, error: null },
          reconciled: { active: [fallbackActor, overriddenActor], done: [] },
        })}
      />,
    );

    expect(container.querySelector('button[aria-label="Stop Ada"]')).not.toBeNull();
    expect(
      container.querySelector<HTMLButtonElement>('button[aria-label="Stop Grace"]')?.disabled,
    ).toBe(false);
    expect(
      container.querySelector('button[data-activity-row="overridden"]')?.textContent,
    ).toContain("Running");
  });

  it("applies a control-only streamed update over a stale available roster-page fallback", async () => {
    const currentActor = actor("control-only", "Curie");
    const staleRosterPage = page([currentActor], [control(currentActor.id, "available", 1, 4)]);
    const baseProps = props({
      snapshot: snapshot({
        revision: 12,
        control: {
          scopeId: "scope-1" as ActivitySnapshot["scopeId"],
          revision: 4,
          actors: [],
          operations: [],
        },
      }),
      active: { pages: [staleRosterPage], loading: false, error: null },
      done: { pages: [page([])], loading: false, error: null },
      reconciled: { active: [currentActor], done: [] },
    });
    const container = await mount(<ActivityRoster {...baseProps} />);
    expect(
      container.querySelector<HTMLButtonElement>(
        'button[aria-label="Stop Curie and 1 child agent"]',
      )?.disabled,
    ).toBe(false);

    await rerender(
      container,
      <ActivityRoster
        {...baseProps}
        snapshot={snapshot({
          revision: 12,
          control: {
            scopeId: "scope-1" as ActivitySnapshot["scopeId"],
            revision: 5,
            actors: [control(currentActor.id, "requested", 1, 5)],
            operations: [],
          },
        })}
      />,
    );

    expect(
      container.querySelector<HTMLButtonElement>(
        'button[aria-label="Stop Curie and 1 child agent"]',
      )?.disabled,
    ).toBe(true);
    expect(
      container.querySelector('button[data-activity-row="control-only"]')?.textContent,
    ).toContain("Stopping");
  });

  it("restores focus to the main detail action instead of the trailing Stop control", async () => {
    const onFocusRestored = vi.fn();
    const container = await mount(
      <ActivityRoster {...props({ focusRecordId: "available", onFocusRestored })} />,
    );

    expect(document.activeElement).toBe(
      container.querySelector('button[data-activity-row="available"]'),
    );
    expect(document.activeElement).not.toBe(
      container.querySelector('button[aria-label^="Stop Lovelace"]'),
    );
    expect(onFocusRestored).toHaveBeenCalledTimes(1);
  });

  it("omits mutation controls for terminal and background-task surfaces", async () => {
    const terminal = await mount(
      <ActivityRoster
        {...props({
          snapshot: snapshot({
            scope: {
              _tag: "terminal",
              threadId: "thread-1",
              terminalId: "terminal-1",
            } as ActivitySnapshot["scope"],
          }),
        })}
      />,
    );
    expect(terminal.querySelector('button[aria-label^="Stop "]')).toBeNull();
    expect(terminal.querySelector("[data-activity-control-unavailable]")).toBeNull();

    const work = workItem("work-1");
    const background = await mount(
      <ActivityRoster
        {...props({
          section: "backgroundTasks",
          active: { pages: [page([work])], loading: false, error: null },
          done: { pages: [page([])], loading: false, error: null },
          reconciled: { active: [work], done: [] },
        })}
      />,
    );
    expect(background.querySelector('button[aria-label^="Stop "]')).toBeNull();
    expect(background.querySelector("[data-activity-control-unavailable]")).toBeNull();
    const row = background.querySelector('button[data-activity-row="work-1"]');
    expect(row?.querySelectorAll('[data-activity-record-glyph="workItem"]')).toHaveLength(1);
    expect(
      row?.querySelector<HTMLElement>('[data-activity-record-glyph="workItem"]')?.className,
    ).toContain("shrink-0");
    expect(row?.querySelector("[data-activity-provider-glyph]")).toBeNull();
  });
});

describe("ActivityRoster hierarchy projection", () => {
  it("renders each child subtree before the next stable sibling", () => {
    const grandchild = actor("grandchild", "Grandchild", { parentActorId: "child" });
    const parent = actor("parent", "Parent");
    const child = actor("child", "Child", { parentActorId: "parent" });
    const sibling = actor("sibling", "Sibling", { parentActorId: "parent" });

    const projected = projectHierarchy([grandchild, parent, child, sibling], "subagents");

    expect(projected.map(({ record }) => record.id)).toEqual([
      "parent",
      "child",
      "grandchild",
      "sibling",
    ]);
  });

  it("uses stable canonical sibling order for a parent-first preorder", () => {
    const sibling = actor("sibling", "Sibling", { parentActorId: "parent" });
    const grandchild = actor("grandchild", "Grandchild", { parentActorId: "child" });
    const parent = actor("parent", "Parent");
    const child = actor("child", "Child", { parentActorId: "parent" });

    const projected = projectHierarchy([sibling, grandchild, parent, child], "subagents");

    expect(projected.map(({ record }) => record.id)).toEqual([
      "parent",
      "sibling",
      "child",
      "grandchild",
    ]);
    expect(projected.map(({ depth }) => depth)).toEqual([0, 1, 1, 2]);
    expect(projected.map(({ connectedToVisibleParent }) => connectedToVisibleParent)).toEqual([
      false,
      true,
      true,
      true,
    ]);
  });

  it("keeps missing and cross-kind parents at the stable root fallback", () => {
    const missing = actor("missing", "Missing", { parentActorId: "not-loaded" });
    const crossKind = actor("cross-kind", "Cross kind", { parentActorId: "work-parent" });
    const workParent = workItem("work-parent");

    const projected = projectHierarchy([missing, crossKind, workParent], "subagents");

    expect(projected.map(({ record }) => record.id)).toEqual([
      "missing",
      "cross-kind",
      "work-parent",
    ]);
    expect(projected.map(({ depth }) => depth)).toEqual([0, 0, 0]);
    expect(projected.every(({ connectedToVisibleParent }) => !connectedToVisibleParent)).toBe(true);
  });

  it("keeps self-parent and every cycle member as stable roots", () => {
    const cycleB = actor("cycle-b", "Cycle B", { parentActorId: "cycle-a" });
    const self = actor("self", "Self", { parentActorId: "self" });
    const cycleA = actor("cycle-a", "Cycle A", { parentActorId: "cycle-b" });

    const projected = projectHierarchy([cycleB, self, cycleA], "subagents");

    expect(projected.map(({ record }) => record.id)).toEqual(["cycle-b", "self", "cycle-a"]);
    expect(projected.map(({ depth }) => depth)).toEqual([0, 0, 0]);
    expect(projected.every(({ connectedToVisibleParent }) => !connectedToVisibleParent)).toBe(true);
  });

  it("caps visual depth without changing eight-level preorder", () => {
    const chain = Array.from({ length: 8 }, (_, index) =>
      actor(`level-${index}`, `Level ${index}`, {
        parentActorId: index === 0 ? null : `level-${index - 1}`,
      }),
    ).toReversed();

    const projected = projectHierarchy(chain, "subagents");

    expect(projected.map(({ record }) => record.id)).toEqual([
      "level-0",
      "level-1",
      "level-2",
      "level-3",
      "level-4",
      "level-5",
      "level-6",
      "level-7",
    ]);
    expect(projected.map(({ depth }) => depth)).toEqual([0, 1, 2, 3, 4, 4, 4, 4]);
  });

  it("leaves background work flat and stable", () => {
    const second = workItem("second", { ownerActorId: "first" });
    const first = workItem("first");

    const projected = projectHierarchy([second, first], "backgroundTasks");

    expect(projected.map(({ record }) => record.id)).toEqual(["second", "first"]);
    expect(projected.map(({ depth }) => depth)).toEqual([0, 0]);
    expect(projected.every(({ connectedToVisibleParent }) => !connectedToVisibleParent)).toBe(true);
  });

  it("projects active and done buckets independently", () => {
    const activeChild = actor("active-child", "Active child", { parentActorId: "done-parent" });
    const doneParent = actor("done-parent", "Done parent", {
      status: "completed",
      terminalAt: "2026-08-11T20:02:00.000Z",
    });

    expect(projectHierarchy([activeChild], "subagents")).toEqual([
      { record: activeChild, depth: 0, connectedToVisibleParent: false },
    ]);
    expect(projectHierarchy([doneParent], "subagents")).toEqual([
      { record: doneParent, depth: 0, connectedToVisibleParent: false },
    ]);
  });
});
