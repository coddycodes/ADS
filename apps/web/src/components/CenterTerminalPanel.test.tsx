import { EnvironmentId, ProjectId, ThreadId } from "@coddy/contracts";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  panelProps: null as Record<string, unknown> | null,
}));

vi.mock("~/state/entities", () => ({
  useThread: () => ({
    environmentId: EnvironmentId.make("environment-1"),
    projectId: ProjectId.make("project-1"),
    worktreePath: "/repo/.coddy/studio/worktrees/feature",
  }),
  useProject: () => ({ workspaceRoot: "/repo" }),
}));
vi.mock("~/composerDraftStore", () => ({
  useComposerDraftStore: (selector: (state: { getDraftThreadByRef: () => null }) => unknown) =>
    selector({ getDraftThreadByRef: () => null }),
}));
vi.mock("~/state/terminalSessions", () => ({
  useKnownTerminalSessions: () => [
    {
      target: { terminalId: "term-1" },
      state: {
        summary: {
          cwd: "/repo/.coddy/studio/worktrees/stale",
          worktreePath: "/repo/.coddy/studio/worktrees/stale",
          label: "stale process",
        },
      },
    },
  ],
}));
vi.mock("@coddy/shared/projectScripts", () => ({
  projectScriptCwd: ({ worktreePath }: { worktreePath: string | null }) => worktreePath ?? "/repo",
  projectScriptRuntimeEnv: () => ({ CODDY_STUDIO_PROJECT_ROOT: "/repo" }),
}));
vi.mock("./ThreadTerminalPanel", () => ({
  default: (props: Record<string, unknown>) => {
    h.panelProps = props;
    return <div data-terminal-panel />;
  },
}));

import { CenterTerminalPanel } from "./CenterTerminalPanel";

beforeEach(() => {
  h.panelProps = null;
});

describe("CenterTerminalPanel", () => {
  it("uses the host worktree and forwards the provider command", () => {
    const onClose = vi.fn();
    const surface = {
      id: "terminal:term-1",
      kind: "terminal",
      terminalId: "term-1",
      label: "Codex Terminal",
      command: {
        executable: "/opt/codex",
        args: ["--dangerously-bypass-approvals-and-sandbox"],
        label: "Codex Terminal",
      },
    } as const;
    renderToStaticMarkup(
      <CenterTerminalPanel
        threadRef={{
          environmentId: EnvironmentId.make("environment-1"),
          threadId: ThreadId.make("thread-1"),
        }}
        projectId={ProjectId.make("project-1")}
        surface={surface}
        launchContext={{
          cwd: "/repo/.coddy/studio/worktrees/feature",
          worktreePath: "/repo/.coddy/studio/worktrees/feature",
          runtimeEnv: { CODDY_STUDIO_PROJECT_ROOT: "/repo" },
        }}
        keybindings={{} as never}
        focusRequestId={7}
        focusEligible={false}
        onAddTerminalContext={vi.fn()}
        onClose={onClose}
      />,
    );

    expect(h.panelProps).toMatchObject({
      owner: "center-panel",
      projectId: ProjectId.make("project-1"),
      cwd: "/repo/.coddy/studio/worktrees/feature",
      worktreePath: "/repo/.coddy/studio/worktrees/feature",
      terminalIds: ["term-1"],
      activeTerminalId: "term-1",
      terminalGroups: [{ id: "terminal:term-1", terminalIds: ["term-1"] }],
      focusRequestId: 7,
      focusEligible: false,
    });
    expect(h.panelProps?.["onSplitTerminal"]).toBeUndefined();
    expect(h.panelProps?.["onSplitTerminalVertical"]).toBeUndefined();
    expect(h.panelProps?.["onCloseTerminal"]).toBe(onClose);
    expect(
      (h.panelProps!["terminalCommandsById"] as ReadonlyMap<string, unknown>).get("term-1"),
    ).toEqual(surface.command);
    expect((h.panelProps!["terminalLabelsById"] as ReadonlyMap<string, string>).get("term-1")).toBe(
      "Codex Terminal",
    );
  });

  it("does not mount an attach layer without a resolved live-thread launch context", () => {
    renderToStaticMarkup(
      <CenterTerminalPanel
        threadRef={{
          environmentId: EnvironmentId.make("environment-1"),
          threadId: ThreadId.make("thread-1"),
        }}
        projectId={ProjectId.make("project-1")}
        surface={{
          id: "terminal:term-1",
          kind: "terminal",
          terminalId: "term-1",
        }}
        launchContext={null}
        keybindings={{} as never}
        focusRequestId={1}
        focusEligible={true}
        onAddTerminalContext={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    expect(h.panelProps).toBeNull();
  });
});
