// @vitest-environment happy-dom

import { scopeThreadRef } from "@coddy/client-runtime/environment";
import type { EnvironmentActivityState } from "@coddy/client-runtime/state/activity";
import {
  type ActivityActorSummary,
  type ActivitySnapshot,
  EnvironmentId,
  ProjectId,
  ProviderDriverKind,
  ProviderInstanceId,
  ThreadId,
  type ProviderTerminalActivityLaunch,
} from "@coddy/contracts";
import * as Option from "effect/Option";
import { act, type ReactElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const state = vi.hoisted(() => ({
  activityStateTargets: [] as unknown[],
  activityState: null as unknown,
}));

vi.mock("@effect/atom-react", () => ({
  useAtomValue: () => state.activityState,
}));

vi.mock("~/state/activity", () => ({
  environmentActivity: {
    stateValueAtom: (target: unknown) => {
      state.activityStateTargets.push(target);
      return { target };
    },
  },
}));

import { scopedProjectKey, scopeProjectRef } from "@coddy/client-runtime/environment";
import { useActivityDockStore } from "~/activityDockStore";
import { selectThreadRightPanelState, useRightPanelStore } from "~/rightPanelStore";
import { ProviderTerminalActivityDock } from "./ProviderTerminalActivityDock";

const environmentId = EnvironmentId.make("provider-terminal-dock");
const threadId = ThreadId.make("thread-provider-terminal-dock");
const threadRef = scopeThreadRef(environmentId, threadId);
const projectId = ProjectId.make("project-provider-terminal-dock");
const activity: ProviderTerminalActivityLaunch = {
  driverKind: ProviderDriverKind.make("codex"),
  providerInstanceId: ProviderInstanceId.make("codex-default"),
};
const mounted: Array<{ readonly container: HTMLDivElement; readonly root: Root }> = [];

function actor(id: string): ActivityActorSummary {
  return {
    _tag: "actor",
    id,
    name: `Agent ${id}`,
    status: "running",
    summary: null,
    startedAt: "2026-07-22T20:00:00.000Z",
    updatedAt: "2026-07-22T20:01:00.000Z",
    terminalAt: null,
    parentActorId: null,
    role: null,
    providerType: "codex",
  } as unknown as ActivityActorSummary;
}

function snapshot(terminalId: string, overrides: Partial<ActivitySnapshot> = {}): ActivitySnapshot {
  const currentActor = actor(`actor-${terminalId}`);
  return {
    protocolVersion: 2,
    scopeId: `scope-${terminalId}`,
    scope: { _tag: "terminal", threadId, terminalId },
    revision: 1,
    provider: "codex",
    providerInstanceId: activity.providerInstanceId,
    capabilities: {
      actors: true,
      attributedActivity: true,
      backgroundWork: false,
      historyRecovery: "full",
      terminalObservation: true,
      targetedActorCancellation: false,
    },
    observationState: "live",
    sections: {
      subagents: { state: "live", message: null, retryable: false },
      backgroundTasks: { state: "unsupported", message: null, retryable: false },
    },
    counts: {
      subagents: { active: 1, done: 0 },
      backgroundTasks: { active: 0, done: 0 },
    },
    actors: [currentActor],
    workItems: [],
    actorsHasMore: false,
    workItemsHasMore: false,
    control: {
      scopeId: `scope-${terminalId}`,
      revision: 0,
      actors: [],
      operations: [],
    },
    updatedAt: "2026-07-22T20:01:00.000Z",
    ...overrides,
  } as ActivitySnapshot;
}

function activityState(
  value: ActivitySnapshot | null,
  status: EnvironmentActivityState["status"] = "live",
  error: string | null = null,
): EnvironmentActivityState {
  return {
    snapshot: value === null ? Option.none() : Option.some(value),
    status,
    error: error === null ? Option.none() : Option.some(error),
    recentEntries: new Map(),
  };
}

function dock(overrides: Partial<Parameters<typeof ProviderTerminalActivityDock>[0]> = {}) {
  return (
    <ProviderTerminalActivityDock
      threadRef={threadRef}
      projectId={projectId}
      terminalId="terminal-codex"
      activity={activity}
      visible
      compact={false}
      {...overrides}
    />
  );
}

async function mount(element: ReactElement): Promise<{
  readonly container: HTMLDivElement;
  readonly root: Root;
}> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const mountedTree = { container, root };
  mounted.push(mountedTree);
  await act(async () => root.render(element));
  return mountedTree;
}

describe("ProviderTerminalActivityDock", () => {
  beforeEach(() => {
    (
      globalThis as {
        IS_REACT_ACT_ENVIRONMENT?: boolean;
      }
    ).IS_REACT_ACT_ENVIRONMENT = true;
    state.activityStateTargets = [];
    state.activityState = activityState(null, "empty");
    useActivityDockStore.setState({ expandedByProject: {} });
    useRightPanelStore.setState({ byThreadKey: {} });
  });

  afterEach(async () => {
    for (const tree of mounted.splice(0)) {
      await act(async () => tree.root.unmount());
      tree.container.remove();
    }
    delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT;
  });

  it("does not subscribe or render when the terminal has no activity hint", () => {
    const markup = renderToStaticMarkup(
      dock({ terminalId: "terminal-shell", activity: undefined }),
    );

    expect(state.activityStateTargets).toEqual([]);
    expect(markup).not.toContain('data-testid="activity-dock"');
  });

  it("subscribes a supported terminal to its full terminal activity scope", () => {
    renderToStaticMarkup(dock());

    expect(state.activityStateTargets).toEqual([
      {
        environmentId,
        input: {
          _tag: "terminal",
          threadId,
          terminalId: "terminal-codex",
        },
      },
    ]);
  });

  it.each([
    ["starting", activityState(null, "synchronizing")],
    ["reconnecting", activityState(null, "stale")],
    ["error", activityState(null, "synchronizing", "observer unavailable")],
  ])("keeps the dock hidden while %s has no successful handshake", (_label, nextState) => {
    state.activityState = nextState;

    const markup = renderToStaticMarkup(dock());

    expect(markup).not.toContain('data-testid="activity-dock"');
  });

  it("keeps a pre-handshake snapshot hidden until terminal observation is proven", () => {
    state.activityState = activityState(
      snapshot("terminal-codex", {
        capabilities: {
          ...snapshot("terminal-codex").capabilities,
          terminalObservation: false,
        },
      }),
    );

    const markup = renderToStaticMarkup(dock());

    expect(markup).not.toContain('data-testid="activity-dock"');
  });

  it("renders the shared dock only after a terminal handshake publishes visible records", () => {
    state.activityState = activityState(snapshot("terminal-codex"));

    const markup = renderToStaticMarkup(dock());

    expect(markup).toContain('data-testid="activity-dock"');
    expect(markup).toContain("Active 1");
  });

  it("keeps the correlated terminal journal visible but marked stale after reconnect", () => {
    state.activityState = activityState(snapshot("terminal-codex"), "stale");

    const markup = renderToStaticMarkup(dock());

    expect(markup).toContain('data-testid="activity-dock"');
    expect(markup).toContain('aria-label="Activity data stale"');
  });

  it("does not mount an activity state reader, dock, or announcement for a hidden terminal pane", () => {
    state.activityState = activityState(snapshot("terminal-codex"));

    const markup = renderToStaticMarkup(dock({ visible: false }));

    expect(state.activityStateTargets).toEqual([]);
    expect(markup).not.toContain('data-testid="activity-dock"');
    expect(markup).not.toContain('aria-live="polite"');
  });

  it("immediately removes the dock and live announcement when a visible pane becomes hidden", async () => {
    state.activityState = activityState(snapshot("terminal-codex"));
    const tree = await mount(dock());
    expect(tree.container.querySelector('[data-testid="activity-dock"]')).not.toBeNull();
    expect(tree.container.querySelector('[aria-live="polite"]')).not.toBeNull();

    await act(async () => tree.root.render(dock({ visible: false })));

    expect(tree.container.querySelector('[data-testid="activity-dock"]')).toBeNull();
    expect(tree.container.querySelector('[aria-live="polite"]')).toBeNull();
    expect(state.activityStateTargets).toHaveLength(1);
  });

  it("opens the exact persisted terminal scope when a section is clicked", async () => {
    state.activityState = activityState(snapshot("terminal-codex"));
    const projectKey = scopedProjectKey(scopeProjectRef(environmentId, projectId));
    useActivityDockStore.getState().setExpanded(projectKey, true);
    const tree = await mount(dock());
    const section = tree.container.querySelector<HTMLButtonElement>(
      '[data-activity-section="subagents"]',
    );
    expect(section).not.toBeNull();

    await act(async () => section!.click());

    const panel = selectThreadRightPanelState(useRightPanelStore.getState().byThreadKey, threadRef);
    expect(panel.isOpen).toBe(true);
    expect(panel.surfaces).toContainEqual({
      id: "activity",
      kind: "activity",
      scope: { _tag: "terminal", terminalId: "terminal-codex" },
      section: "subagents",
      selectedRecordKind: null,
      selectedRecordId: null,
    });
  });
});
