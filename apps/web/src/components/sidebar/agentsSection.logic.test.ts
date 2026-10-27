import type { EnvironmentAvailabilityStatus } from "@coddy/client-runtime/state/shell";
import type {
  EnvironmentProject,
  EnvironmentThreadShell,
} from "@coddy/client-runtime/state/models";
import { EnvironmentId, ProjectId, ProviderInstanceId, ThreadId } from "@coddy/contracts";
import { createModelSelection } from "@coddy/shared/model";
import { describe, expect, it } from "vite-plus/test";

import type { ThreadStatusPill } from "../Sidebar.logic";
import {
  buildAgentRows,
  buildAgentViewGroups,
  countUnreadAgentRows,
  resolveAgentCheckouts,
  resolveAgentGroup,
  resolveAgentPreviewLine,
  resolveAgentProvider,
  resolveConversationPreviewLine,
} from "./agentsSection.logic";

const ENVIRONMENT_A = EnvironmentId.make("environment-a");
const ENVIRONMENT_B = EnvironmentId.make("environment-b");
const PROJECT_A = ProjectId.make("project-a");
const UPDATED_AT = "2026-08-31T12:00:00.000Z";

type ThreadSession = NonNullable<EnvironmentThreadShell["session"]>;

function makeSession(overrides: Partial<ThreadSession> = {}): ThreadSession {
  return {
    threadId: ThreadId.make("thread-default"),
    status: "ready",
    providerName: "Codex",
    runtimeMode: "full-access",
    activeTurnId: null,
    lastError: null,
    updatedAt: UPDATED_AT,
    ...overrides,
  } as ThreadSession;
}

function makeShell(overrides: Partial<EnvironmentThreadShell> = {}): EnvironmentThreadShell {
  const id = overrides.id ?? ThreadId.make("thread-default");
  return {
    id,
    projectId: PROJECT_A,
    title: "Default thread",
    modelSelection: createModelSelection(ProviderInstanceId.make("codex"), "gpt-5-codex"),
    runtimeMode: "full-access",
    interactionMode: "default",
    branch: null,
    worktreePath: null,
    latestTurn: null,
    createdAt: UPDATED_AT,
    updatedAt: UPDATED_AT,
    archivedAt: null,
    session: makeSession({ threadId: id }),
    latestUserMessageAt: UPDATED_AT,
    hasPendingApprovals: false,
    hasPendingUserInput: false,
    hasActionableProposedPlan: false,
    conversationPreview: null,
    environmentId: ENVIRONMENT_A,
    ...overrides,
  } as EnvironmentThreadShell;
}

function buildRows(
  shells: ReadonlyArray<EnvironmentThreadShell>,
  overrides: {
    readonly projectTitleById?: ReadonlyMap<string, string>;
    readonly environmentLabelById?: ReadonlyMap<string, string>;
    readonly availabilityByEnvironmentId?: ReadonlyMap<string, EnvironmentAvailabilityStatus>;
    readonly liveBranchByThreadKey?: ReadonlyMap<string, string>;
  } = {},
) {
  return buildAgentRows({
    shells,
    projectTitleById:
      overrides.projectTitleById ?? new Map<string, string>([[PROJECT_A, "Project Alpha"]]),
    environmentLabelById:
      overrides.environmentLabelById ??
      new Map<string, string>([
        [ENVIRONMENT_A, "Local"],
        [ENVIRONMENT_B, "Build farm"],
      ]),
    availabilityByEnvironmentId:
      overrides.availabilityByEnvironmentId ??
      new Map<string, EnvironmentAvailabilityStatus>([
        [ENVIRONMENT_A, "live"],
        [ENVIRONMENT_B, "live"],
      ]),
    ...(overrides.liveBranchByThreadKey === undefined
      ? {}
      : { liveBranchByThreadKey: overrides.liveBranchByThreadKey }),
  });
}

const pillRest = {
  colorClass: "text-test",
  dotClass: "bg-test",
  pulse: false,
} as const;
const workingPill: ThreadStatusPill = { label: "Working", ...pillRest };
const completedPill: ThreadStatusPill = { label: "Completed", ...pillRest };

describe("resolveAgentGroup", () => {
  it("maps pill labels to groups per spec §3.3", () => {
    expect(resolveAgentGroup({ label: "Working", ...pillRest })).toBe("working");
    expect(resolveAgentGroup({ label: "Connecting", ...pillRest })).toBe("working");
    expect(resolveAgentGroup({ label: "Pending Approval", ...pillRest })).toBe("blocked");
    expect(resolveAgentGroup({ label: "Awaiting Input", ...pillRest })).toBe("waiting");
    expect(resolveAgentGroup({ label: "Plan Ready", ...pillRest })).toBe("waiting");
    expect(resolveAgentGroup({ label: "Completed", ...pillRest })).toBe("done");
    expect(resolveAgentGroup(null)).toBe("done");
  });
});

describe("resolveAgentPreviewLine", () => {
  it("shows the tool line only while working, else assistant, else prompt", () => {
    const preview = { prompt: "p", tool: "Bash: ls", assistantMessage: "a" };
    expect(resolveAgentPreviewLine(workingPill, preview)).toBe("Bash: ls");
    expect(resolveAgentPreviewLine({ label: "Connecting", ...pillRest }, preview)).toBe("Bash: ls");
    expect(resolveAgentPreviewLine(workingPill, { ...preview, tool: null })).toBe("a");
    expect(resolveAgentPreviewLine(completedPill, preview)).toBe("a");
    expect(resolveAgentPreviewLine(completedPill, { ...preview, assistantMessage: null })).toBe(
      "p",
    );
    expect(
      resolveAgentPreviewLine(completedPill, {
        prompt: null,
        tool: null,
        assistantMessage: null,
      }),
    ).toBeNull();
    expect(resolveAgentPreviewLine(completedPill, null)).toBeNull();
    expect(resolveAgentPreviewLine(completedPill, undefined)).toBeNull();
  });
});

describe("resolveConversationPreviewLine", () => {
  const preview = { prompt: "p", tool: "Bash: ls", assistantMessage: "a" };

  it("shows the tool only while working, then the reply, then the prompt", () => {
    expect(resolveConversationPreviewLine(true, preview)).toBe("Bash: ls");
    expect(resolveConversationPreviewLine(false, preview)).toBe("a");
    expect(resolveConversationPreviewLine(true, { ...preview, tool: null })).toBe("a");
    expect(resolveConversationPreviewLine(false, { ...preview, assistantMessage: null })).toBe("p");
    expect(resolveConversationPreviewLine(false, null)).toBeNull();
  });
});

describe("resolveAgentProvider", () => {
  it("maps a session provider name to a driver kind and display label", () => {
    expect(resolveAgentProvider("claudeAgent")).toEqual({
      driverKind: "claudeAgent",
      label: "Claude",
    });
    expect(resolveAgentProvider("codex")).toEqual({ driverKind: "codex", label: "Codex" });
    expect(resolveAgentProvider("unknownProvider").label).toBe("Unknown Provider");
    expect(resolveAgentProvider("  ")).toEqual({ driverKind: null, label: null });
    expect(resolveAgentProvider(null)).toEqual({ driverKind: null, label: null });
  });
});

describe("buildAgentRows", () => {
  it("carries the session provider onto the row", () => {
    const base = makeShell({ id: ThreadId.make("thread-provider") });
    const rows = buildRows([
      { ...base, session: { ...base.session!, providerName: "claudeAgent" } },
      {
        ...base,
        id: ThreadId.make("thread-unknown"),
        session: { ...base.session!, providerName: null },
      },
    ]);

    expect(rows.map((row) => [row.providerDriverKind, row.providerLabel])).toEqual([
      ["claudeAgent", "Claude"],
      [null, null],
    ]);
  });

  it("includes only non-archived shells with a session", () => {
    const included = makeShell({ id: ThreadId.make("thread-included") });
    const archived = makeShell({
      id: ThreadId.make("thread-archived"),
      archivedAt: "2026-08-30T12:00:00.000Z",
    });
    const sessionless = makeShell({
      id: ThreadId.make("thread-sessionless"),
      session: null,
    });

    const rows = buildRows([archived, included, sessionless]);

    expect(rows.map((row) => row.shell.id)).toEqual([ThreadId.make("thread-included")]);
    expect(rows[0]?.key).toBe("environment-a:thread-included");
    expect(rows[0]?.ref).toEqual({
      environmentId: ENVIRONMENT_A,
      threadId: ThreadId.make("thread-included"),
    });
  });

  it("marks rows stale when availability is not 'live'", () => {
    const stale = makeShell({ id: ThreadId.make("thread-stale") });
    const unknown = makeShell({
      id: ThreadId.make("thread-unknown"),
      environmentId: ENVIRONMENT_B,
    });

    const rows = buildRows([stale, unknown], {
      availabilityByEnvironmentId: new Map<string, EnvironmentAvailabilityStatus>([
        [ENVIRONMENT_A, "degraded"],
      ]),
    });

    expect(
      rows.map((row) => ({
        id: row.shell.id,
        environmentLive: row.environmentLive,
        environmentStatus: row.environmentStatus,
      })),
    ).toEqual([
      {
        id: ThreadId.make("thread-stale"),
        environmentLive: false,
        environmentStatus: "degraded",
      },
      {
        id: ThreadId.make("thread-unknown"),
        environmentLive: false,
        environmentStatus: null,
      },
    ]);
  });

  it("builds a lowercase haystack containing title, project, branch, env label, provider, pill label, previews", () => {
    const shell = makeShell({
      title: "Fix Auth Flow",
      branch: "Feature/Login",
      session: makeSession({ status: "running", providerName: "Claude Code" }),
      conversationPreview: {
        prompt: "Prompt Text",
        tool: "Bash: pnpm TEST",
        assistantMessage: "Assistant Reply",
      },
    });

    const rows = buildRows([shell], {
      projectTitleById: new Map<string, string>([[PROJECT_A, "Orca UI"]]),
      environmentLabelById: new Map<string, string>([[ENVIRONMENT_A, "Build Farm"]]),
    });
    const searchText = rows[0]?.searchText;

    expect(searchText).toBe(searchText?.toLowerCase());
    for (const part of [
      "fix auth flow",
      "orca ui",
      "feature/login",
      "build farm",
      "claude code",
      "working",
      "prompt text",
      "bash: pnpm test",
      "assistant reply",
    ]) {
      expect(searchText).toContain(part);
    }
  });
});

describe("agent row branch", () => {
  it("shows and searches the live branch over the recorded one", () => {
    const moved = makeShell({ id: ThreadId.make("thread-moved"), branch: "main" });
    const unread = makeShell({ id: ThreadId.make("thread-unread"), branch: "main" });
    const rows = buildRows([moved, unread], {
      liveBranchByThreadKey: new Map([["environment-a:thread-moved", "feature/live"]]),
    });
    expect(rows.map((row) => row.branch)).toEqual(["feature/live", "main"]);
    expect(rows[0]?.searchText).toContain("feature/live");
    expect(rows[0]?.searchText).not.toContain("main");
  });

  it("reads each live thread's worktree, else its project's root, once per checkout", () => {
    const project = (id: ProjectId, environmentId: EnvironmentId, workspaceRoot: string) =>
      ({ id, environmentId, workspaceRoot }) as EnvironmentProject;
    const PROJECT_B = ProjectId.make("project-b");
    const { checkoutKeyByThreadKey, checkouts } = resolveAgentCheckouts({
      shells: [
        makeShell({ id: ThreadId.make("thread-root-1") }),
        makeShell({ id: ThreadId.make("thread-root-2") }),
        makeShell({ id: ThreadId.make("thread-worktree"), worktreePath: "/wt/feature" }),
        // Same project id in another environment: its own root, not project-a's.
        makeShell({ id: ThreadId.make("thread-remote"), environmentId: ENVIRONMENT_B }),
        makeShell({ id: ThreadId.make("thread-unknown-project"), projectId: PROJECT_B }),
        makeShell({ id: ThreadId.make("thread-archived"), archivedAt: UPDATED_AT }),
      ],
      projects: [
        project(PROJECT_A, ENVIRONMENT_A, "/repo/a"),
        project(PROJECT_A, ENVIRONMENT_B, "/remote/a"),
      ],
      availabilityByEnvironmentId: new Map<string, EnvironmentAvailabilityStatus>([
        [ENVIRONMENT_A, "live"],
        [ENVIRONMENT_B, "live"],
      ]),
    });
    const keyA = JSON.stringify([ENVIRONMENT_A, "/repo/a"]);
    expect([...checkoutKeyByThreadKey]).toEqual([
      ["environment-a:thread-root-1", keyA],
      ["environment-a:thread-root-2", keyA],
      ["environment-a:thread-worktree", JSON.stringify([ENVIRONMENT_A, "/wt/feature"])],
      ["environment-b:thread-remote", JSON.stringify([ENVIRONMENT_B, "/remote/a"])],
    ]);
    expect(checkouts.map((checkout) => checkout.cwd)).toEqual([
      "/repo/a",
      "/wt/feature",
      "/remote/a",
    ]);
  });

  it("reads no checkout in an environment that is not live", () => {
    const { checkouts } = resolveAgentCheckouts({
      shells: [makeShell({ worktreePath: "/wt/feature" })],
      projects: [],
      availabilityByEnvironmentId: new Map<string, EnvironmentAvailabilityStatus>([
        [ENVIRONMENT_A, "synchronizing"],
      ]),
    });
    expect(checkouts).toEqual([]);
  });
});

describe("buildAgentViewGroups", () => {
  it("builds status groups in fixed order with prefixed ids and elides empty groups", () => {
    const rows = buildRows([
      makeShell({
        id: ThreadId.make("thread-done"),
        updatedAt: "2026-08-31T11:00:00.000Z",
      }),
      makeShell({
        id: ThreadId.make("thread-blocked"),
        updatedAt: "2026-08-31T12:00:00.000Z",
        hasPendingApprovals: true,
      }),
      makeShell({
        id: ThreadId.make("thread-working-old"),
        updatedAt: "2026-08-31T13:00:00.000Z",
        session: makeSession({ status: "running" }),
      }),
      makeShell({
        id: ThreadId.make("thread-working-new"),
        updatedAt: "2026-08-31T14:00:00.000Z",
        session: makeSession({ status: "running" }),
      }),
    ]);

    const groups = buildAgentViewGroups(rows, {
      query: "",
      groupBy: "status",
      unreadOnly: false,
      unreadThreadKeys: [],
      selectedKey: null,
    });

    expect(groups.map(({ id, label }) => ({ id, label }))).toEqual([
      { id: "status:working", label: "Working" },
      { id: "status:blocked", label: "Pending Approval" },
      { id: "status:done", label: "Done" },
    ]);
    expect(groups[0]?.rows.map((row) => row.shell.id)).toEqual([
      ThreadId.make("thread-working-new"),
      ThreadId.make("thread-working-old"),
    ]);
  });

  it("uses normalized query filtering and fails closed past the byte cap", () => {
    const rows = buildRows([
      makeShell({
        id: ThreadId.make("thread-match"),
        conversationPreview: {
          prompt: "Initial prompt",
          tool: null,
          assistantMessage: "Assistant Reply",
        },
      }),
      makeShell({ id: ThreadId.make("thread-other"), title: "Unrelated work" }),
    ]);
    const options = {
      groupBy: "status",
      unreadOnly: false,
      unreadThreadKeys: [],
      selectedKey: null,
    } as const;

    expect(
      buildAgentViewGroups(rows, { ...options, query: "  ASSISTANT   REPLY " }).flatMap((group) =>
        group.rows.map((row) => row.shell.id),
      ),
    ).toEqual([ThreadId.make("thread-match")]);
    expect(buildAgentViewGroups(rows, { ...options, query: "x".repeat(2049) })).toEqual([]);
  });

  it("groups by project with Unknown fallback and orders groups by their newest row", () => {
    const projectB = ProjectId.make("project-b");
    const unknownProject = ProjectId.make("project-unknown");
    const rows = buildRows(
      [
        makeShell({
          id: ThreadId.make("thread-alpha-old"),
          updatedAt: "2026-08-31T11:00:00.000Z",
        }),
        makeShell({
          id: ThreadId.make("thread-beta"),
          projectId: projectB,
          updatedAt: "2026-08-31T14:00:00.000Z",
        }),
        makeShell({
          id: ThreadId.make("thread-unknown"),
          projectId: unknownProject,
          updatedAt: "2026-08-31T12:00:00.000Z",
        }),
        makeShell({
          id: ThreadId.make("thread-alpha-new"),
          updatedAt: "2026-08-31T15:00:00.000Z",
        }),
      ],
      {
        projectTitleById: new Map<string, string>([
          [PROJECT_A, "Project Alpha"],
          [projectB, "Project Beta"],
        ]),
      },
    );

    const groups = buildAgentViewGroups(rows, {
      query: "",
      groupBy: "project",
      unreadOnly: false,
      unreadThreadKeys: [],
      selectedKey: null,
    });

    expect(groups.map(({ id, label }) => ({ id, label }))).toEqual([
      { id: "project:Project Alpha", label: "Project Alpha" },
      { id: "project:Project Beta", label: "Project Beta" },
      { id: "project:Unknown", label: "Unknown" },
    ]);
    expect(groups[0]?.rows.map((row) => row.shell.id)).toEqual([
      ThreadId.make("thread-alpha-new"),
      ThreadId.make("thread-alpha-old"),
    ]);
  });

  it("groups by environment with Unknown fallback and recency ordering", () => {
    const unknownEnvironment = EnvironmentId.make("environment-unknown");
    const rows = buildRows([
      makeShell({
        id: ThreadId.make("thread-local-new"),
        updatedAt: "2026-08-31T13:00:00.000Z",
      }),
      makeShell({
        id: ThreadId.make("thread-build-farm"),
        environmentId: ENVIRONMENT_B,
        updatedAt: "2026-08-31T14:00:00.000Z",
      }),
      makeShell({
        id: ThreadId.make("thread-unknown-environment"),
        environmentId: unknownEnvironment,
        updatedAt: "2026-08-31T12:00:00.000Z",
      }),
      makeShell({
        id: ThreadId.make("thread-local-old"),
        updatedAt: "2026-08-31T11:00:00.000Z",
      }),
    ]);

    const groups = buildAgentViewGroups(rows, {
      query: "",
      groupBy: "environment",
      unreadOnly: false,
      unreadThreadKeys: [],
      selectedKey: null,
    });

    expect(groups.map(({ id, label }) => ({ id, label }))).toEqual([
      { id: "environment:Build farm", label: "Build farm" },
      { id: "environment:Local", label: "Local" },
      { id: "environment:Unknown", label: "Unknown" },
    ]);
    expect(groups[1]?.rows.map((row) => row.shell.id)).toEqual([
      ThreadId.make("thread-local-new"),
      ThreadId.make("thread-local-old"),
    ]);
  });

  it("keeps only unread rows plus the selected row when unread-only is enabled", () => {
    const rows = buildRows([
      makeShell({
        id: ThreadId.make("thread-unread"),
        updatedAt: "2026-08-31T14:00:00.000Z",
      }),
      makeShell({
        id: ThreadId.make("thread-selected-read"),
        updatedAt: "2026-08-31T13:00:00.000Z",
      }),
      makeShell({
        id: ThreadId.make("thread-hidden-read"),
        updatedAt: "2026-08-31T15:00:00.000Z",
      }),
    ]);

    const groups = buildAgentViewGroups(rows, {
      query: "",
      groupBy: "status",
      unreadOnly: true,
      unreadThreadKeys: [rows[0]!.key],
      selectedKey: rows[1]!.key,
    });

    expect(groups.flatMap((group) => group.rows.map((row) => row.shell.id))).toEqual([
      ThreadId.make("thread-unread"),
      ThreadId.make("thread-selected-read"),
    ]);
  });
});

describe("countUnreadAgentRows", () => {
  it("counts rows whose keys are unread", () => {
    const rows = buildRows([
      makeShell({ id: ThreadId.make("thread-unread-a") }),
      makeShell({ id: ThreadId.make("thread-read") }),
      makeShell({ id: ThreadId.make("thread-unread-b") }),
    ]);

    expect(countUnreadAgentRows(rows, [rows[0]!.key, "environment-a:missing", rows[2]!.key])).toBe(
      2,
    );
  });
});
