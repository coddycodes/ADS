// @vitest-environment happy-dom

import {
  ProviderDriverKind,
  type ActivityActorSummary,
  type ActivityEntry,
  type ActivitySnapshot,
  type ActivityWorkItemSummary,
} from "@coddy/contracts";
import { act, type ReactElement, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterAll, afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import {
  ActivityPanel,
  type ActivityDetailPageData,
  type ActivityPanelProps,
  type ActivityQueryResult,
  type ActivityRosterPageData,
} from "./ActivityPanel";
import { formatChatTimestampTooltip } from "~/timestampFormat";

const FIXED_NOW = "2026-07-22T20:20:00.000Z";
const mounted: Array<{ readonly root: Root; readonly container: HTMLDivElement }> = [];
const suiteGetAnimationsDescriptor = Object.getOwnPropertyDescriptor(
  Element.prototype,
  "getAnimations",
);
let originalGetAnimationsDescriptor: PropertyDescriptor | undefined;

function actor(
  id: string,
  overrides: Partial<Omit<ActivityActorSummary, "id" | "parentActorId">> & {
    readonly parentActorId?: string | null;
  } = {},
): ActivityActorSummary {
  return {
    _tag: "actor",
    id,
    name: id,
    status: "running",
    summary: `Summary for ${id}`,
    startedAt: "2026-07-22T20:00:00.000Z",
    updatedAt: "2026-07-22T20:10:00.000Z",
    terminalAt: null,
    parentActorId: null,
    role: "reviewer",
    providerType: "codex",
    ...overrides,
  } as unknown as ActivityActorSummary;
}

function workItem(
  id: string,
  overrides: Partial<ActivityWorkItemSummary> = {},
): ActivityWorkItemSummary {
  return {
    _tag: "workItem",
    id,
    name: id,
    status: "running",
    summary: `Summary for ${id}`,
    startedAt: "2026-07-22T20:00:00.000Z",
    updatedAt: "2026-07-22T20:10:00.000Z",
    terminalAt: null,
    ownerActorId: null,
    workKind: "process",
    command: "private command that must not appear in the roster",
    cwd: "/private/workspace",
    ...overrides,
  } as unknown as ActivityWorkItemSummary;
}

function entry(
  id: string,
  kind: ActivityEntry["kind"],
  createdAt: string,
  detail: string | null = `Detail for ${id}`,
  overrides: Partial<ActivityEntry> = {},
): ActivityEntry {
  return {
    id,
    ownerKind: "actor",
    ownerId: "child",
    kind,
    title: `${kind} ${id}`,
    detail,
    tone: kind === "error" ? "error" : "info",
    createdAt,
    ...overrides,
  } as unknown as ActivityEntry;
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
      subagents: { active: 2, done: 2 },
      backgroundTasks: { active: 1, done: 1 },
    },
    actors: [],
    workItems: [],
    actorsHasMore: false,
    workItemsHasMore: false,
    control: { scopeId: "scope-1", revision: 0, actors: [], operations: [] },
    updatedAt: "2026-07-22T20:10:00.000Z",
    ...overrides,
  } as unknown as ActivitySnapshot;
}

function query<Page>(
  pages: ReadonlyArray<Page> = [],
  overrides: Partial<ActivityQueryResult<Page>> = {},
): ActivityQueryResult<Page> {
  return {
    pages,
    loading: false,
    error: null,
    ...overrides,
  };
}

function rosterPage(
  records: ActivityRosterPageData["records"],
  nextCursor: string | null = null,
): ActivityRosterPageData {
  return { records, actorControls: [], nextCursor } as ActivityRosterPageData;
}

function detailPage(
  record: ActivityDetailPageData["record"],
  entries: ActivityDetailPageData["entries"],
  nextCursor: string | null = null,
): ActivityDetailPageData {
  return { record, actorControl: null, entries, nextCursor } as ActivityDetailPageData;
}

function detailQuery(
  recordKind: "actor" | "workItem",
  recordId: string,
  pages: ReadonlyArray<ActivityDetailPageData> = [],
  overrides: Partial<NonNullable<ActivityPanelProps["detail"]>> = {},
): NonNullable<ActivityPanelProps["detail"]> {
  return {
    recordKind,
    recordId,
    pages,
    loading: false,
    error: null,
    ...overrides,
  } as unknown as NonNullable<ActivityPanelProps["detail"]>;
}

function props(overrides: Partial<ActivityPanelProps> = {}): ActivityPanelProps {
  return {
    route: {
      section: "subagents",
      selectedRecordKind: null,
      selectedRecordId: null,
    },
    snapshot: snapshot(),
    roster: {
      active: query([rosterPage([])]),
      done: query([rosterPage([])]),
    },
    detail: null,
    onNavigate: vi.fn(),
    onLoadMoreRoster: vi.fn(),
    onLoadMoreDetail: vi.fn(),
    onRefreshSnapshot: vi.fn(),
    now: FIXED_NOW,
    timestampFormat: "24-hour",
    ...overrides,
  };
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
  const mountedTree = mounted.find((candidate) => candidate.container === container);
  if (mountedTree === undefined) {
    throw new Error("Cannot rerender an unmounted ActivityPanel test tree.");
  }
  await act(async () => mountedTree.root.render(element));
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  originalGetAnimationsDescriptor = Object.getOwnPropertyDescriptor(
    Element.prototype,
    "getAnimations",
  );
  Object.defineProperty(Element.prototype, "getAnimations", {
    configurable: true,
    value: () => [],
  });
});

afterEach(async () => {
  for (const mountedTree of mounted.splice(0)) {
    await act(async () => mountedTree.root.unmount());
    mountedTree.container.remove();
  }
  if (originalGetAnimationsDescriptor === undefined) {
    Reflect.deleteProperty(Element.prototype, "getAnimations");
  } else {
    Object.defineProperty(Element.prototype, "getAnimations", originalGetAnimationsDescriptor);
  }
  originalGetAnimationsDescriptor = undefined;
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
  vi.restoreAllMocks();
});

afterAll(() => {
  expect(Object.getOwnPropertyDescriptor(Element.prototype, "getAnimations")).toEqual(
    suiteGetAnimationsDescriptor,
  );
});

describe("ActivityPanel roster", () => {
  it("renders only server-authoritative partial retries and forwards the fenced summary", async () => {
    const onRetryCancellation = vi.fn();
    const partialSnapshot = snapshot({
      capabilities: {
        actors: true,
        attributedActivity: true,
        backgroundWork: true,
        historyRecovery: "full",
        terminalObservation: false,
        targetedActorCancellation: true,
      },
      control: {
        scopeId: "scope-1" as ActivitySnapshot["scopeId"],
        revision: 12,
        actors: [],
        operations: [
          {
            rootActorId: "root-actor" as ActivityActorSummary["id"],
            state: "partial",
            residualCount: 2,
            message: "Some agents are still running.",
            operationRevision: 6,
          },
        ],
      },
    });
    const container = await mount(
      <ActivityPanel {...props({ snapshot: partialSnapshot, onRetryCancellation })} />,
    );

    expect(container.textContent).toContain("Some agents are still running. 2 remaining.");
    const retry = [...container.querySelectorAll<HTMLButtonElement>("button")].find(
      (button) => button.textContent === "Retry remaining",
    );
    expect(retry).toBeDefined();
    await act(async () => retry?.click());
    expect(onRetryCancellation).toHaveBeenCalledWith("root-actor", 6);
  });

  it("suppresses command failures while a server operation is requested", async () => {
    const requestedSnapshot = snapshot({
      capabilities: {
        actors: true,
        attributedActivity: true,
        backgroundWork: true,
        historyRecovery: "full",
        terminalObservation: false,
        targetedActorCancellation: true,
      },
      control: {
        scopeId: "scope-1" as ActivitySnapshot["scopeId"],
        revision: 13,
        actors: [],
        operations: [
          {
            rootActorId: "root-actor" as ActivityActorSummary["id"],
            state: "requested",
            residualCount: 0,
            message: null,
            operationRevision: 7,
          },
        ],
      },
    });
    const container = await mount(
      <ActivityPanel
        {...props({
          snapshot: requestedSnapshot,
          cancellationError: "Unable to stop agents. Try again.",
          onRetryCancellation: vi.fn(),
        })}
      />,
    );

    expect(container.textContent).not.toContain("Unable to stop agents");
    expect(container.textContent).not.toContain("Retry remaining");
  });

  it("keeps the last provider lifecycle visible beside a bounded command failure", async () => {
    const running = actor("still-running", { name: "Still running" });
    const container = await mount(
      <ActivityPanel
        {...props({
          cancellationError: "Stopping agents timed out. Some agents may still be running.",
          roster: {
            active: query([rosterPage([running])]),
            done: query([rosterPage([])]),
          },
        })}
      />,
    );

    expect(container.textContent).toContain(
      "Stopping agents timed out. Some agents may still be running.",
    );
    expect(
      container.querySelector('button[data-activity-row="still-running"]')?.textContent,
    ).toContain("Running");
  });
  it("shows only actors in active-oldest and done-newest order with safe row fields", async () => {
    const onNavigate = vi.fn();
    const active = [
      actor("newer", {
        name: "Newer reviewer",
        startedAt: "2026-07-22T20:10:00.000Z",
        providerType: "codex",
      }),
      workItem("wrong-kind"),
      actor("older", {
        name: "Older reviewer",
        summary: "<img src=x onerror=alert(1)> safe summary",
        startedAt: "2026-07-22T20:00:00.000Z",
        providerType: "claude",
      }),
    ];
    const done = [
      actor("done-old", {
        status: "completed",
        terminalAt: "2026-07-22T20:12:00.000Z",
      }),
      actor("done-new", {
        status: "failed",
        terminalAt: "2026-07-22T20:18:00.000Z",
      }),
    ];
    const container = await mount(
      <ActivityPanel
        {...props({
          onNavigate,
          roster: {
            active: query([rosterPage(active)]),
            done: query([rosterPage(done)]),
          },
        })}
      />,
    );

    expect(container.textContent).toContain("Active");
    expect(container.textContent).toContain("Done · 2");
    const rows = Array.from(container.querySelectorAll<HTMLButtonElement>("[data-activity-row]"));
    expect(rows.map((row) => row.dataset.activityRow)).toEqual([
      "older",
      "newer",
      "done-new",
      "done-old",
    ]);
    expect(container.textContent).not.toContain("wrong-kind");
    const older = rows[0]!;
    expect(older.textContent).toContain("Older reviewer");
    expect(older.textContent).toContain("<img src=x onerror=alert(1)> safe summary");
    expect(older.querySelector("img")).toBeNull();
    expect(older.textContent).toContain("Running");
    expect(older.textContent).toContain("Elapsed 20m");
    expect(older.className).toContain("min-h-9");
    expect(older.className).toContain("sm:min-h-8");
    expect(older.className).not.toContain("sm:h-8");
    expect(older.querySelectorAll('[data-activity-provider-glyph="claude"]')).toHaveLength(1);
    expect(older.querySelector('[data-activity-record-glyph="actor"]')).toBeNull();
    expect(rows[2]?.textContent).toContain("Completed in 18m");

    await act(async () => older.click());
    expect(onNavigate).toHaveBeenCalledWith({
      section: "subagents",
      selectedRecordKind: "actor",
      selectedRecordId: "older",
    });
  });

  it("orders active and done records by RFC3339 instant across timezone offsets", async () => {
    const container = await mount(
      <ActivityPanel
        {...props({
          roster: {
            active: query([
              rosterPage([
                actor("active-offset-later", {
                  startedAt: "2026-07-22T09:00:00Z",
                }),
                actor("active-offset-earlier", {
                  startedAt: "2026-07-22T10:30:00+02:00",
                }),
              ]),
            ]),
            done: query([
              rosterPage([
                actor("done-offset-earlier", {
                  status: "completed",
                  terminalAt: "2026-07-22T10:30:00+02:00",
                }),
                actor("done-offset-later", {
                  status: "completed",
                  terminalAt: "2026-07-22T09:00:00Z",
                }),
              ]),
            ]),
          },
        })}
      />,
    );

    expect(
      Array.from(container.querySelectorAll<HTMLElement>("[data-activity-row]")).map(
        (row) => row.dataset.activityRow,
      ),
    ).toEqual([
      "active-offset-earlier",
      "active-offset-later",
      "done-offset-later",
      "done-offset-earlier",
    ]);
  });

  it("reconciles transition races across buckets using the newest update instant", async () => {
    const container = await mount(
      <ActivityPanel
        {...props({
          roster: {
            active: query([
              rosterPage([
                actor("offset-race", {
                  name: "Stale active offset",
                  status: "running",
                  updatedAt: "2026-07-22T10:30:00+02:00",
                }),
                actor("fraction-race", {
                  name: "Stale active fraction",
                  status: "running",
                  updatedAt: "2026-07-22T08:00:00.1234Z",
                }),
                actor("revived-race", {
                  name: "Current active",
                  status: "running",
                  updatedAt: "2026-07-22T10:00:00Z",
                }),
              ]),
            ]),
            done: query([
              rosterPage([
                actor("offset-race", {
                  name: "Current done offset",
                  status: "completed",
                  updatedAt: "2026-07-22T09:00:00Z",
                  terminalAt: "2026-07-22T09:00:00Z",
                }),
                actor("fraction-race", {
                  name: "Current done fraction",
                  status: "completed",
                  updatedAt: "2026-07-22T08:00:00.1235Z",
                  terminalAt: "2026-07-22T08:00:00.1235Z",
                }),
                actor("revived-race", {
                  name: "Stale done",
                  status: "completed",
                  updatedAt: "2026-07-22T09:00:00Z",
                  terminalAt: "2026-07-22T09:00:00Z",
                }),
              ]),
            ]),
          },
        })}
      />,
    );

    const rows = Array.from(container.querySelectorAll<HTMLElement>("[data-activity-row]"));
    expect(rows.map((row) => row.dataset.activityRow)).toEqual([
      "revived-race",
      "offset-race",
      "fraction-race",
    ]);
    expect(container.textContent).toContain("Current active");
    expect(container.textContent).toContain("Current done offset");
    expect(container.textContent).toContain("Current done fraction");
    expect(container.textContent).not.toContain("Stale active");
    expect(container.textContent).not.toContain("Stale done");
  });

  it("prefers a terminal record when cross-bucket updates have the same timestamp", async () => {
    const updatedAt = "2026-07-22T09:00:00.123456Z";
    const container = await mount(
      <ActivityPanel
        {...props({
          roster: {
            active: query([
              rosterPage([
                actor("equal-time-race", {
                  name: "Equal-time active",
                  status: "running",
                  updatedAt,
                }),
              ]),
            ]),
            done: query([
              rosterPage([
                actor("equal-time-race", {
                  name: "Equal-time terminal",
                  status: "completed",
                  updatedAt,
                  terminalAt: updatedAt,
                }),
              ]),
            ]),
          },
        })}
      />,
    );

    expect(container.querySelectorAll('[data-activity-row="equal-time-race"]')).toHaveLength(1);
    expect(container.textContent).toContain("Equal-time terminal");
    expect(container.textContent).toContain("Completed");
    expect(container.textContent).not.toContain("Equal-time active");
  });

  it("shows only work items in Background Tasks and appends de-duplicated pages", async () => {
    const onLoadMoreRoster = vi.fn();
    const firstProps = props({
      route: {
        section: "backgroundTasks",
        selectedRecordKind: null,
        selectedRecordId: null,
      },
      roster: {
        active: query([rosterPage([workItem("work-a"), actor("wrong-kind")], "next-active")]),
        done: query([rosterPage([])]),
      },
      onLoadMoreRoster,
    });
    const container = await mount(<ActivityPanel {...firstProps} />);

    expect(container.textContent).toContain("Background Tasks");
    expect(container.textContent).toContain("work-a");
    expect(container.textContent).not.toContain("wrong-kind");
    const loadMore = container.querySelector<HTMLButtonElement>(
      'button[aria-label="Load more background tasks"]',
    );
    expect(loadMore).not.toBeNull();
    expect(container.querySelectorAll('button[aria-label^="Load more"]')).toHaveLength(1);

    await act(async () => loadMore?.click());
    expect(onLoadMoreRoster).toHaveBeenCalledWith("active");

    await rerender(
      container,
      <ActivityPanel
        {...firstProps}
        roster={{
          active: query([
            rosterPage([workItem("work-a")], "next-active"),
            rosterPage([workItem("work-a"), workItem("work-b")]),
          ]),
          done: query([rosterPage([])]),
        }}
      />,
    );
    expect(
      Array.from(container.querySelectorAll("[data-activity-row]")).map(
        (row) => (row as HTMLElement).dataset.activityRow,
      ),
    ).toEqual(["work-a", "work-b"]);
    expect(container.querySelector('button[aria-label^="Load more"]')).toBeNull();
  });

  it("bounds a 5,000-row roster to 200 rendered rows in 50-row groups", async () => {
    const records = Array.from({ length: 5_000 }, (_, index) =>
      actor(`actor-${String(index).padStart(3, "0")}`),
    );
    const container = await mount(
      <ActivityPanel
        {...props({
          roster: {
            active: query([rosterPage(records.slice(0, 110)), rosterPage(records.slice(110))]),
            done: query([rosterPage([])]),
          },
        })}
      />,
    );

    expect(container.querySelectorAll("[data-activity-row]")).toHaveLength(200);
    expect(container.querySelectorAll("[data-activity-window-group]")).toHaveLength(4);
    expect(container.querySelector('[data-activity-row="actor-4999"]')).toBeNull();
  });

  it("keeps stale and failed last-known pages inspectable with scoped retry banners", async () => {
    const onRefreshSnapshot = vi.fn();
    const failedSnapshot = snapshot({
      observationState: "error",
      sections: {
        subagents: {
          state: "error",
          message: "Actor stream failed.",
          retryable: true,
        },
        backgroundTasks: {
          state: "stale",
          message: "Background history is stale.",
          retryable: true,
        },
      },
    });
    const container = await mount(
      <ActivityPanel
        {...props({
          snapshot: failedSnapshot,
          onRefreshSnapshot,
          roster: {
            active: query([rosterPage([actor("retained")])]),
            done: query([rosterPage([])]),
          },
        })}
      />,
    );

    expect(container.textContent).toContain("retained");
    expect(container.textContent).toContain("Showing the last known activity");
    expect(container.textContent).toContain("Actor stream failed.");
    expect(container.textContent).not.toContain("Background history is stale.");
    const retry = Array.from(container.querySelectorAll<HTMLButtonElement>("button")).find(
      (button) => button.textContent === "Retry",
    );
    expect(retry).toBeDefined();
    await act(async () => retry?.click());
    expect(onRefreshSnapshot).toHaveBeenCalledTimes(1);

    await rerender(
      container,
      <ActivityPanel
        {...props({
          snapshot: snapshot({ observationState: "stale" }),
          roster: {
            active: query([rosterPage([actor("still-visible")])]),
            done: query([rosterPage([])]),
          },
        })}
      />,
    );
    expect(container.textContent).toContain("Activity data is stale");
    expect(container.textContent).toContain("still-visible");
  });

  it("retries an initial active-roster failure without claiming empty or retained data", async () => {
    const onLoadMoreRoster = vi.fn();
    const onRefreshSnapshot = vi.fn();
    const container = await mount(
      <ActivityPanel
        {...props({
          roster: {
            active: query([], { error: "Active roster failed." }),
            done: query([rosterPage([])]),
          },
          onLoadMoreRoster,
          onRefreshSnapshot,
        })}
      />,
    );

    expect(container.textContent).toContain("Active roster failed.");
    expect(container.textContent).not.toContain("No subagents observed");
    expect(container.textContent).not.toContain("last loaded page");
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>('button[aria-label="Retry active activity"]')
        ?.click(),
    );
    expect(onLoadMoreRoster).toHaveBeenCalledWith("active");
    expect(onRefreshSnapshot).not.toHaveBeenCalled();
  });

  it("retries an initial done-roster failure through the done query", async () => {
    const onLoadMoreRoster = vi.fn();
    const container = await mount(
      <ActivityPanel
        {...props({
          roster: {
            active: query([rosterPage([])]),
            done: query([], { error: "Done roster failed." }),
          },
          onLoadMoreRoster,
        })}
      />,
    );

    expect(container.textContent).toContain("Done roster failed.");
    expect(container.textContent).not.toContain("No subagents observed");
    expect(container.textContent).not.toContain("last loaded page");
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>('button[aria-label="Retry done activity"]')
        ?.click(),
    );
    expect(onLoadMoreRoster).toHaveBeenCalledWith("done");
  });

  it("retains roster pages on failure and retries only the affected bucket", async () => {
    const onLoadMoreRoster = vi.fn();
    const onRefreshSnapshot = vi.fn();
    const container = await mount(
      <ActivityPanel
        {...props({
          roster: {
            active: query([rosterPage([actor("retained-active")])], {
              error: "Active pagination failed.",
            }),
            done: query([rosterPage([])]),
          },
          onLoadMoreRoster,
          onRefreshSnapshot,
        })}
      />,
    );

    expect(container.textContent).toContain("retained-active");
    expect(container.textContent).toContain("The last loaded page remains available.");
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>('button[aria-label="Retry active activity"]')
        ?.click(),
    );
    expect(onLoadMoreRoster).toHaveBeenCalledWith("active");
    expect(onRefreshSnapshot).not.toHaveBeenCalled();
  });

  it("distinguishes unsupported sections from observed empty sections", async () => {
    const unsupported = await mount(
      <ActivityPanel
        {...props({
          snapshot: snapshot({
            capabilities: {
              actors: false,
              attributedActivity: false,
              backgroundWork: true,
              historyRecovery: "none",
              terminalObservation: false,
              targetedActorCancellation: false,
            },
            sections: {
              subagents: { state: "unsupported", message: null, retryable: false },
              backgroundTasks: { state: "live", message: null, retryable: false },
            },
          }),
        })}
      />,
    );
    expect(unsupported.textContent).toContain("Subagents are not supported by this provider");

    const observedEmpty = await mount(<ActivityPanel {...props()} />);
    expect(observedEmpty.textContent).toContain("No subagents observed");
  });

  it("shows initial loading and a disabled done-page continuation without claiming emptiness", async () => {
    const loading = await mount(
      <ActivityPanel
        {...props({
          roster: {
            active: query([], { loading: true }),
            done: query([]),
          },
        })}
      />,
    );
    expect(loading.textContent).toContain("Loading activity");
    expect(loading.textContent).not.toContain("No subagents observed");

    const paginating = await mount(
      <ActivityPanel
        {...props({
          route: {
            section: "backgroundTasks",
            selectedRecordKind: null,
            selectedRecordId: null,
          },
          roster: {
            active: query([rosterPage([])]),
            done: query(
              [
                rosterPage(
                  [
                    workItem("completed-work", {
                      status: "completed",
                      terminalAt: "2026-07-22T20:10:00.000Z",
                    }),
                  ],
                  "next-done",
                ),
              ],
              { loading: true, error: "Done pagination is retryable." },
            ),
          },
        })}
      />,
    );
    const loadMore = paginating.querySelector<HTMLButtonElement>(
      'button[aria-label="Load more background tasks"]',
    );
    expect(loadMore?.disabled).toBe(true);
    expect(loadMore?.textContent).toContain("Loading more");
  });

  it("renders fallback provider, actor type, summary, and terminal timing branches predictably", async () => {
    const timestamp = "2026-07-22T20:00:00.000Z";
    const container = await mount(
      <ActivityPanel
        {...props({
          snapshot: snapshot({ provider: ProviderDriverKind.make("future-provider") }),
          roster: {
            active: query([
              rosterPage([
                actor("a-active"),
                actor("z-active", {
                  providerType: null,
                  role: null,
                  summary: null,
                  startedAt: timestamp,
                }),
                actor("m-active", { startedAt: timestamp }),
              ]),
            ]),
            done: query([
              rosterPage([
                actor("z-done", {
                  status: "completed",
                  terminalAt: null,
                  updatedAt: timestamp,
                }),
                actor("a-done", {
                  status: "completed",
                  terminalAt: null,
                  updatedAt: timestamp,
                }),
              ]),
            ]),
          },
        })}
      />,
    );

    expect(
      Array.from(container.querySelectorAll<HTMLElement>("[data-activity-row]")).map(
        (row) => row.dataset.activityRow,
      ),
    ).toEqual(["a-active", "m-active", "z-active", "a-done", "z-done"]);
    const fallbackRow = container.querySelector<HTMLElement>('[data-activity-row="z-active"]');
    expect(
      fallbackRow?.querySelector('[data-activity-provider-glyph="future-provider"]'),
    ).not.toBeNull();
    expect(fallbackRow?.textContent).toContain("Actor");
    expect(fallbackRow?.textContent).not.toContain("Summary for z-active");
    expect(container.querySelector('[data-activity-row="a-done"]')?.textContent).toContain(
      "Elapsed",
    );
  });
});

describe("ActivityPanel detail", () => {
  it("does not render a stale detail page for a different routed record", async () => {
    const container = await mount(
      <ActivityPanel
        {...props({
          route: {
            section: "subagents",
            selectedRecordKind: "actor",
            selectedRecordId: "child",
          },
          detail: detailQuery("actor", "previous", [
            detailPage(actor("previous", { name: "Previous selection" }), []),
          ]),
        })}
      />,
    );

    expect(container.textContent).toContain("Loading record");
    expect(container.textContent).not.toContain("Previous selection");
  });

  it("rejects a keyed detail result when any later page belongs to another record", async () => {
    const child = actor("child", { name: "Child reviewer" });
    const container = await mount(
      <ActivityPanel
        {...props({
          route: {
            section: "subagents",
            selectedRecordKind: "actor",
            selectedRecordId: "child",
          },
          detail: detailQuery("actor", "child", [
            detailPage(child, [entry("child-entry", "state", "2026-07-22T20:02:00Z")], "next"),
            detailPage(actor("previous", { name: "Previous reviewer" }), [
              entry("previous-entry", "state", "2026-07-22T20:01:00Z"),
            ]),
          ]),
        })}
      />,
    );

    expect(container.textContent).toContain("Loading record");
    expect(container.textContent).not.toContain("Child reviewer");
    expect(container.textContent).not.toContain("Previous reviewer");
    expect(container.textContent).not.toContain("child-entry");
    expect(container.textContent).not.toContain("previous-entry");
  });

  it("does not apply a removed result keyed to a previous selection", async () => {
    const onNavigate = vi.fn();
    const container = await mount(
      <ActivityPanel
        {...props({
          route: {
            section: "subagents",
            selectedRecordKind: "actor",
            selectedRecordId: "child",
          },
          detail: detailQuery("actor", "previous", [], { removed: true }),
          onNavigate,
        })}
      />,
    );

    expect(container.textContent).toContain("Loading record");
    expect(container.textContent).not.toContain("no longer available");
    expect(onNavigate).not.toHaveBeenCalled();
  });

  it("makes an initial detail load failure retryable", async () => {
    const onLoadMoreDetail = vi.fn();
    const container = await mount(
      <ActivityPanel
        {...props({
          route: {
            section: "subagents",
            selectedRecordKind: "actor",
            selectedRecordId: "child",
          },
          detail: detailQuery("actor", "child", [], {
            error: "Could not load this activity record.",
          }),
          onLoadMoreDetail,
        })}
      />,
    );

    expect(container.textContent).toContain("Could not load this activity record.");
    const retry = Array.from(container.querySelectorAll<HTMLButtonElement>("button")).find(
      (button) => button.textContent === "Retry",
    );
    expect(retry).toBeDefined();
    await act(async () => retry?.click());
    expect(onLoadMoreDetail).toHaveBeenCalledTimes(1);
  });

  it("renders record metadata and safe, distinct chronological de-duplicated entries", async () => {
    const command = "<script>globalThis.pwned = true</script>";
    const child = actor("child", {
      name: "Child reviewer",
      status: "completed",
      startedAt: "2026-07-22T20:00:00.000Z",
      terminalAt: "2026-07-22T20:15:00.000Z",
      parentActorId: "parent",
      providerType: "codex",
    });
    const parent = actor("parent", { name: "Parent reviewer" });
    const details = detailQuery("actor", "child", [
      detailPage(
        child,
        [
          entry("entry-5", "error", "2026-07-22T20:05:00.000Z"),
          entry("entry-4", "tool", "2026-07-22T20:04:00.000Z"),
          entry("entry-3", "state", "2026-07-22T20:03:00.000Z"),
        ],
        "next",
      ),
      detailPage(child, [
        entry("entry-3", "state", "2026-07-22T20:03:00.000Z", null, {
          title: "stale duplicate title",
        }),
        entry("entry-2", "command", "2026-07-22T20:02:00.000Z", command),
        entry("entry-1", "commentary", "2026-07-22T20:01:00.000Z"),
      ]),
    ]);
    const container = await mount(
      <ActivityPanel
        {...props({
          route: {
            section: "subagents",
            selectedRecordKind: "actor",
            selectedRecordId: "child",
          },
          snapshot: snapshot({ actors: [parent, child] }),
          roster: {
            active: query([rosterPage([parent])]),
            done: query([rosterPage([child])]),
          },
          detail: details,
        })}
      />,
    );

    const heading = container.querySelector<HTMLHeadingElement>("[data-activity-detail-heading]");
    expect(heading?.textContent).toBe("Child reviewer");
    expect(container.textContent).toContain("Actor");
    expect(container.textContent).toContain("Completed");
    expect(container.textContent).toContain("codex");
    const started = container.querySelector<HTMLTimeElement>(
      'time[datetime="2026-07-22T20:00:00.000Z"]',
    );
    const ended = container.querySelector<HTMLTimeElement>(
      'time[datetime="2026-07-22T20:15:00.000Z"]',
    );
    expect(started?.textContent).toBe(
      formatChatTimestampTooltip("2026-07-22T20:00:00.000Z", "24-hour"),
    );
    expect(ended?.textContent).toBe(
      formatChatTimestampTooltip("2026-07-22T20:15:00.000Z", "24-hour"),
    );
    expect(started?.title).toBe("2026-07-22T20:00:00.000Z");
    expect(ended?.title).toBe("2026-07-22T20:15:00.000Z");
    expect(started?.textContent).not.toBe(started?.dateTime);
    const parentButton = container.querySelector<HTMLButtonElement>(
      'button[data-activity-relation="parent"]',
    );
    expect(parentButton?.textContent).toContain("Parent reviewer");

    const entries = Array.from(container.querySelectorAll<HTMLElement>("[data-activity-entry-id]"));
    expect(entries.map((row) => row.dataset.activityEntryId)).toEqual([
      "entry-1",
      "entry-2",
      "entry-3",
      "entry-4",
      "entry-5",
    ]);
    expect(
      entries.map((row) => row.querySelector("[data-activity-entry-label]")?.textContent),
    ).toEqual(["Commentary", "Command", "State", "Tool", "Error"]);
    const firstEntryTime = container.querySelector<HTMLTimeElement>(
      '[data-activity-entry-id="entry-1"] time[datetime="2026-07-22T20:01:00.000Z"]',
    );
    expect(firstEntryTime?.textContent).toBe(
      formatChatTimestampTooltip("2026-07-22T20:01:00.000Z", "24-hour"),
    );
    expect(firstEntryTime?.dateTime).toBe("2026-07-22T20:01:00.000Z");
    expect(firstEntryTime?.title).toBe("2026-07-22T20:01:00.000Z");
    expect(firstEntryTime?.textContent).not.toBe(firstEntryTime?.dateTime);
    expect(new Set(entries.map((row) => row.dataset.activityEntryKind)).size).toBe(5);
    expect(container.textContent).toContain(command);
    expect(container.textContent).not.toContain("stale duplicate title");
    expect(container.querySelector("script")).toBeNull();
    expect(Reflect.get(globalThis, "pwned")).toBeUndefined();
  });

  it("preserves malformed activity timestamps in visible and semantic metadata", async () => {
    const child = actor("child", { startedAt: "not-a-date" });
    const container = await mount(
      <ActivityPanel
        {...props({
          route: {
            section: "subagents",
            selectedRecordKind: "actor",
            selectedRecordId: "child",
          },
          detail: detailQuery("actor", "child", [
            detailPage(child, [entry("bad-entry", "command", "not-a-date")]),
          ]),
        })}
      />,
    );

    const started = container.querySelector<HTMLTimeElement>('time[datetime="not-a-date"]');
    expect(started?.textContent).toBe("not-a-date");
    expect(started?.dateTime).toBe("not-a-date");
    expect(started?.title).toBe("not-a-date");
    const entryTimestamp = container.querySelector<HTMLTimeElement>(
      '[data-activity-entry-id="bad-entry"] time[datetime="not-a-date"]',
    );
    expect(entryTimestamp?.textContent).toBe("not-a-date");
    expect(entryTimestamp?.dateTime).toBe("not-a-date");
    expect(entryTimestamp?.title).toBe("not-a-date");
  });

  it("formats record and event timestamps with the selected 12-hour preference", async () => {
    const startedAt = "2026-07-22T20:00:00.000Z";
    const createdAt = "2026-07-22T20:01:00.000Z";
    const child = actor("child", { startedAt });
    const container = await mount(
      <ActivityPanel
        {...props({
          timestampFormat: "12-hour",
          route: {
            section: "subagents",
            selectedRecordKind: "actor",
            selectedRecordId: "child",
          },
          detail: detailQuery("actor", "child", [
            detailPage(child, [entry("twelve-hour-entry", "command", createdAt)]),
          ]),
        })}
      />,
    );

    expect(
      container.querySelector<HTMLTimeElement>(`time[datetime="${startedAt}"]`)?.textContent,
    ).toBe(formatChatTimestampTooltip(startedAt, "12-hour"));
    expect(
      container.querySelector<HTMLTimeElement>(
        `[data-activity-entry-id="twelve-hour-entry"] time[datetime="${createdAt}"]`,
      )?.textContent,
    ).toBe(formatChatTimestampTooltip(createdAt, "12-hour"));
  });

  it("formats event timestamps in a narrow detail row while retaining canonical metadata", async () => {
    const createdAt = "2026-07-22T20:01:02.123456789Z";
    const child = actor("child");
    const container = await mount(
      <div style={{ width: "12rem" }}>
        <ActivityPanel
          {...props({
            route: {
              section: "subagents",
              selectedRecordKind: "actor",
              selectedRecordId: "child",
            },
            detail: detailQuery("actor", "child", [
              detailPage(child, [
                entry("narrow-command", "command", createdAt, null, {
                  title: "A command title that uses the remaining narrow row space",
                }),
              ]),
            ]),
          })}
        />
      </div>,
    );

    const timestamp = container.querySelector<HTMLTimeElement>(
      `[data-activity-entry-id="narrow-command"] time[datetime="${createdAt}"]`,
    );
    const row = timestamp?.parentElement;
    const title = timestamp?.previousElementSibling;
    expect(timestamp?.textContent).toBe(formatChatTimestampTooltip(createdAt, "24-hour"));
    expect(timestamp?.dateTime).toBe(createdAt);
    expect(timestamp?.title).toBe(createdAt);
    expect(timestamp?.textContent).not.toBe(timestamp?.dateTime);
    expect(row?.className).toContain("flex-wrap");
    expect(title?.className).toContain("min-w-16");
    expect(timestamp?.className).toContain("ml-auto");
  });

  it("filters entries whose owner does not match the keyed record", async () => {
    const child = actor("child");
    const container = await mount(
      <ActivityPanel
        {...props({
          route: {
            section: "subagents",
            selectedRecordKind: "actor",
            selectedRecordId: "child",
          },
          detail: detailQuery("actor", "child", [
            detailPage(child, [
              entry("foreign-entry", "error", "2026-07-22T20:02:00Z", "must not leak", {
                ownerId: "previous" as ActivityEntry["ownerId"],
              }),
              entry("child-entry", "state", "2026-07-22T20:01:00Z"),
            ]),
          ]),
        })}
      />,
    );

    expect(container.textContent).toContain("child-entry");
    expect(container.textContent).not.toContain("foreign-entry");
    expect(container.textContent).not.toContain("must not leak");
  });

  it("orders detail entries chronologically by RFC3339 instant across offsets", async () => {
    const child = actor("child");
    const container = await mount(
      <ActivityPanel
        {...props({
          route: {
            section: "subagents",
            selectedRecordKind: "actor",
            selectedRecordId: "child",
          },
          detail: detailQuery("actor", "child", [
            detailPage(child, [
              entry("offset-later", "state", "2026-07-22T09:00:00Z"),
              entry("offset-earlier", "state", "2026-07-22T10:30:00+02:00"),
            ]),
          ]),
        })}
      />,
    );

    expect(
      Array.from(container.querySelectorAll<HTMLElement>("[data-activity-entry-id]")).map(
        (row) => row.dataset.activityEntryId,
      ),
    ).toEqual(["offset-earlier", "offset-later"]);
  });

  it("only links an in-scope parent and keeps 16KiB detail collapsed by default", async () => {
    const hugeDetail = "x".repeat(16 * 1_024);
    const child = actor("child", { parentActorId: "missing-parent" });
    const container = await mount(
      <ActivityPanel
        {...props({
          route: {
            section: "subagents",
            selectedRecordKind: "actor",
            selectedRecordId: "child",
          },
          detail: detailQuery("actor", "child", [
            detailPage(child, [
              entry("large-entry", "tool", "2026-07-22T20:01:00.000Z", hugeDetail),
            ]),
          ]),
        })}
      />,
    );

    expect(container.querySelector('[data-activity-relation="parent"]')).toBeNull();
    expect(container.textContent).toContain("missing-parent");
    const disclosure = container.querySelector<HTMLDetailsElement>(
      'details[data-activity-entry-detail="large-entry"]',
    );
    expect(disclosure).not.toBeNull();
    expect(disclosure?.open).toBe(false);
    expect(disclosure?.querySelector("summary")?.textContent).toContain("Show details");
  });

  it("retains the newest 200 entries when older server pages exceed the cap", async () => {
    const child = actor("child");
    const newestFirstEntries = Array.from({ length: 205 }, (_, index) =>
      entry(
        `entry-${String(index).padStart(3, "0")}`,
        "state",
        new Date(Date.UTC(2026, 6, 22, 20, 10, 0) - index * 1_000).toISOString(),
      ),
    );
    const container = await mount(
      <ActivityPanel
        {...props({
          route: {
            section: "subagents",
            selectedRecordKind: "actor",
            selectedRecordId: "child",
          },
          detail: detailQuery("actor", "child", [
            detailPage(child, newestFirstEntries.slice(0, 110), "next"),
            detailPage(child, newestFirstEntries.slice(110)),
          ]),
        })}
      />,
    );

    expect(container.querySelectorAll("[data-activity-entry-id]")).toHaveLength(200);
    expect(container.querySelectorAll("[data-activity-entry-window-group]")).toHaveLength(4);
    expect(container.querySelector('[data-activity-entry-id="entry-000"]')).not.toBeNull();
    expect(container.querySelector('[data-activity-entry-id="entry-199"]')).not.toBeNull();
    expect(container.querySelector('[data-activity-entry-id="entry-200"]')).toBeNull();
    expect(container.querySelector('[data-activity-entry-id="entry-204"]')).toBeNull();
  });

  it("returns to the same roster, restores focus, and focuses the heading after selection", async () => {
    const routes: ActivityPanelProps["route"][] = [];
    const onNavigate = vi.fn((route: ActivityPanelProps["route"]) => routes.push(route));
    const rosterProps = props({
      onNavigate,
      roster: {
        active: query([rosterPage([actor("child", { name: "Child reviewer" })])]),
        done: query([rosterPage([])]),
      },
    });
    const container = await mount(<ActivityPanel {...rosterProps} />);
    const panel = container.querySelector<HTMLElement>("[data-activity-panel]");
    expect(panel?.className).toContain("bg-background");
    expect(panel?.className).not.toContain("bg-white");
    const row = container.querySelector<HTMLButtonElement>('[data-activity-row="child"]')!;
    row.focus();
    await act(async () => row.click());
    expect(routes.at(-1)).toEqual({
      section: "subagents",
      selectedRecordKind: "actor",
      selectedRecordId: "child",
    });

    const selectedProps: ActivityPanelProps = {
      ...rosterProps,
      route: routes.at(-1)!,
      detail: detailQuery("actor", "child", [
        detailPage(actor("child", { name: "Child reviewer" }), []),
      ]),
    };
    await rerender(container, <ActivityPanel {...selectedProps} />);
    const heading = container.querySelector<HTMLHeadingElement>("[data-activity-detail-heading]");
    expect(document.activeElement).toBe(heading);

    const back = container.querySelector<HTMLButtonElement>(
      'button[aria-label="Back to Subagents"]',
    );
    expect(back?.className).toContain("focus-visible:ring-2");
    expect(back?.className).toContain("focus-visible:ring-ring");
    await act(async () => back?.click());
    expect(routes.at(-1)).toEqual({
      section: "subagents",
      selectedRecordKind: null,
      selectedRecordId: null,
    });
    await rerender(
      container,
      <ActivityPanel {...selectedProps} route={routes.at(-1)!} detail={null} />,
    );
    expect(document.activeElement).toBe(
      container.querySelector<HTMLButtonElement>('[data-activity-row="child"]'),
    );
  });

  it("persists the polite removal notice after the parent normalizes the route", async () => {
    const navigation = vi.fn();
    function StatefulRemovalParent() {
      const [route, setRoute] = useState<ActivityPanelProps["route"]>({
        section: "subagents",
        selectedRecordKind: "actor",
        selectedRecordId: "removed-child",
      });
      return (
        <ActivityPanel
          {...props({
            route,
            roster: {
              active: query([rosterPage([actor("next-child", { name: "Next reviewer" })])]),
              done: query([rosterPage([])]),
            },
            detail: detailQuery("actor", "removed-child", [], { removed: true }),
            onNavigate: (nextRoute) => {
              navigation(nextRoute);
              setRoute(nextRoute);
            },
          })}
        />
      );
    }

    const container = await mount(<StatefulRemovalParent />);

    expect(navigation).toHaveBeenCalledWith({
      section: "subagents",
      selectedRecordKind: null,
      selectedRecordId: null,
    });
    expect(container.textContent).toContain("This activity record is no longer available");
    expect(container.querySelector('[role="status"]')?.getAttribute("aria-live")).toBe("polite");
    expect(container.textContent).toContain("Next reviewer");

    await act(async () =>
      container.querySelector<HTMLButtonElement>('[data-activity-row="next-child"]')?.click(),
    );
    expect(container.textContent).not.toContain("This activity record is no longer available");
  });

  it("renders background-work ownership, retained errors, tied entries, and loading continuation", async () => {
    const onNavigate = vi.fn();
    const owner = actor("owner", { name: "Owning actor" });
    const task = workItem("work", {
      name: "Background build",
      ownerActorId: owner.id,
      status: "running",
      terminalAt: null,
    });
    const tiedAt = "2026-07-22T20:01:00.000Z";
    const container = await mount(
      <ActivityPanel
        {...props({
          route: {
            section: "backgroundTasks",
            selectedRecordKind: "workItem",
            selectedRecordId: task.id,
          },
          snapshot: snapshot({ actors: [owner] }),
          roster: {
            active: query([rosterPage([task])]),
            done: query([rosterPage([])]),
          },
          detail: detailQuery(
            "workItem",
            task.id,
            [
              detailPage(
                task,
                [
                  entry("a-entry", "state", tiedAt, null, {
                    ownerKind: "workItem",
                    ownerId: task.id,
                  }),
                  entry("z-entry", "state", tiedAt, null, {
                    ownerKind: "workItem",
                    ownerId: task.id,
                  }),
                  entry("m-entry", "state", tiedAt, null, {
                    ownerKind: "workItem",
                    ownerId: task.id,
                  }),
                ],
                "next-detail",
              ),
            ],
            {
              loading: true,
              error: "Later entries could not be loaded.",
            },
          ),
          onNavigate,
        })}
      />,
    );

    expect(container.textContent).toContain("Background task · process");
    expect(container.textContent).toContain("Later entries could not be loaded.");
    expect(container.textContent).toContain("The last loaded entries remain available.");
    expect(container.textContent).toContain("—");
    expect(
      Array.from(container.querySelectorAll<HTMLElement>("[data-activity-entry-id]")).map(
        (row) => row.dataset.activityEntryId,
      ),
    ).toEqual(["a-entry", "m-entry", "z-entry"]);
    const ownerButton = container.querySelector<HTMLButtonElement>(
      'button[data-activity-relation="owner"]',
    );
    expect(ownerButton?.textContent).toContain("Owning actor");
    await act(async () => ownerButton?.click());
    expect(onNavigate).toHaveBeenCalledWith({
      section: "subagents",
      selectedRecordKind: "actor",
      selectedRecordId: owner.id,
    });
    const loadMore = Array.from(container.querySelectorAll<HTMLButtonElement>("button")).find(
      (button) => button.textContent?.includes("Loading more"),
    );
    expect(loadMore?.disabled).toBe(true);
  });

  it("uses the snapshot provider and loading-entry copy for an actor without provider metadata", async () => {
    const onNavigate = vi.fn();
    const child = actor("child-without-provider", {
      providerType: null,
      terminalAt: null,
    });
    const container = await mount(
      <ActivityPanel
        {...props({
          route: {
            section: "subagents",
            selectedRecordKind: "actor",
            selectedRecordId: child.id,
          },
          snapshot: snapshot({ provider: ProviderDriverKind.make("claude") }),
          detail: detailQuery("actor", child.id, [detailPage(child, [])], {
            loading: true,
          }),
          onNavigate,
        })}
      />,
    );

    expect(container.textContent).toContain("claude");
    expect(container.textContent).toContain("Loading record entries");
    await act(async () =>
      container.querySelector<HTMLButtonElement>('button[aria-label="Back to Subagents"]')?.click(),
    );
    expect(onNavigate).toHaveBeenCalledWith({
      section: "subagents",
      selectedRecordKind: null,
      selectedRecordId: null,
    });
    const { now: _now, ...propsWithoutNow } = props({
      route: {
        section: "subagents",
        selectedRecordKind: null,
        selectedRecordId: null,
      },
      onNavigate,
    });
    await rerender(container, <ActivityPanel {...propsWithoutNow} />);
    expect(container.textContent).toContain("No subagents observed");
  });

  it("reconciles open roster and detail views after authoritative query refreshes", async () => {
    const stale = actor("live-child", {
      status: "running",
      updatedAt: "2026-07-22T20:10:00.000Z",
    });
    const live = actor("live-child", {
      status: "completed",
      terminalAt: "2026-07-22T20:12:00.000Z",
      updatedAt: "2026-07-22T20:12:00.000Z",
    });
    const recent = entry(
      "live-entry",
      "commentary",
      "2026-07-22T20:11:00.000Z",
      "Arrived while the inspector stayed open.",
      { ownerId: live.id },
    );
    const container = await mount(
      <ActivityPanel
        {...props({
          route: {
            section: "subagents",
            selectedRecordKind: "actor",
            selectedRecordId: live.id,
          },
          snapshot: snapshot({
            actors: [live],
            counts: {
              subagents: { active: 0, done: 1 },
              backgroundTasks: { active: 0, done: 0 },
            },
          }),
          roster: {
            active: query([rosterPage([stale])]),
            done: query([rosterPage([])]),
          },
          detail: detailQuery("actor", live.id, [detailPage(stale, [])]),
        })}
      />,
    );

    expect(container.textContent).toContain("LifecycleRunning");
    await rerender(
      container,
      <ActivityPanel
        {...props({
          route: {
            section: "subagents",
            selectedRecordKind: "actor",
            selectedRecordId: live.id,
          },
          snapshot: snapshot({
            actors: [live],
            counts: {
              subagents: { active: 0, done: 1 },
              backgroundTasks: { active: 0, done: 0 },
            },
          }),
          roster: {
            active: query([rosterPage([])]),
            done: query([rosterPage([live])]),
          },
          detail: detailQuery("actor", live.id, [detailPage(live, [recent])]),
        })}
      />,
    );
    expect(container.textContent).toContain("LifecycleCompleted");
    expect(container.textContent).toContain("Arrived while the inspector stayed open.");
    await rerender(
      container,
      <ActivityPanel
        {...props({
          snapshot: snapshot({
            actors: [live],
            counts: {
              subagents: { active: 0, done: 1 },
              backgroundTasks: { active: 0, done: 0 },
            },
          }),
          roster: {
            active: query([rosterPage([])]),
            done: query([rosterPage([live])]),
          },
        })}
      />,
    );
    expect(container.querySelector(`[data-activity-row="${live.id}"]`)?.textContent).toContain(
      "Completed",
    );
  });

  it("uses safe section-health fallback copy without exposing a retry for non-retryable errors", async () => {
    const container = await mount(
      <ActivityPanel
        {...props({
          route: {
            section: "backgroundTasks",
            selectedRecordKind: null,
            selectedRecordId: null,
          },
          snapshot: snapshot({
            observationState: "reconnecting",
            sections: {
              subagents: { state: "live", message: null, retryable: false },
              backgroundTasks: { state: "error", message: null, retryable: false },
            },
          }),
        })}
      />,
    );

    expect(container.textContent).toContain("Activity data is stale");
    expect(container.textContent).toContain("Background tasks data is error.");
    expect(
      Array.from(container.querySelectorAll<HTMLButtonElement>("button")).some(
        (button) => button.textContent === "Retry",
      ),
    ).toBe(false);

    await rerender(
      container,
      <ActivityPanel
        {...props({
          snapshot: snapshot({
            sections: {
              subagents: { state: "stale", message: null, retryable: false },
              backgroundTasks: { state: "live", message: null, retryable: false },
            },
          }),
        })}
      />,
    );
    expect(container.textContent).toContain("Subagents data is stale.");
  });

  it("handles a repeated removed-detail effect only once for the same route", async () => {
    const firstNavigate = vi.fn();
    const route: ActivityPanelProps["route"] = {
      section: "subagents",
      selectedRecordKind: "actor",
      selectedRecordId: "removed-child",
    };
    const detail = detailQuery("actor", "removed-child", [], { removed: true });
    const container = await mount(
      <ActivityPanel {...props({ route, detail, onNavigate: firstNavigate })} />,
    );
    expect(firstNavigate).toHaveBeenCalledTimes(1);

    const secondNavigate = vi.fn();
    await rerender(
      container,
      <ActivityPanel {...props({ route, detail, onNavigate: secondNavigate })} />,
    );
    expect(secondNavigate).not.toHaveBeenCalled();
    expect(container.textContent).toContain("This activity record is no longer available");
  });
});
