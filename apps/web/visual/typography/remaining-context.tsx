import DiffPanel from "../../src/components/DiffPanel";
import GitActionsControl from "../../src/components/GitActionsControl";
import { SettingsFixtureFrame } from "./settings-ports";
import type { Fixture } from "./types";
import { remainingThread, remainingThreadRef, publishCwd } from "./remaining-data";
import { ShareTab } from "../../src/components/settings/remote-servers/ShareTab";

function ActiveDiff({ emptyTurns = false }: { readonly emptyTurns?: boolean }) {
  return (
    <SettingsFixtureFrame>
      <div className="h-[560px]">
        <DiffPanel
          mode="embedded"
          composerDraftTarget={remainingThreadRef}
          thread={emptyTurns ? { ...remainingThread, checkpoints: [] } : remainingThread}
        />
      </div>
    </SettingsFixtureFrame>
  );
}

export const remainingContextFixtures: readonly Fixture[] = [
  {
    id: "diff-panel-no-thread",
    title: "Diff thread selection prompt",
    modules: ["components/DiffPanel.tsx"],
    owners: ["DiffPanel"],
    anchors: ["Select a thread to inspect turn diffs."],
    render: () => (
      <SettingsFixtureFrame>
        <div className="h-96">
          <DiffPanel mode="embedded" composerDraftTarget={remainingThreadRef} />
        </div>
      </SettingsFixtureFrame>
    ),
  },
  {
    id: "diff-panel-workspace-unavailable",
    title: "Diff workspace unavailable text",
    modules: ["components/DiffPanel.tsx"],
    owners: ["DiffPanel"],
    anchors: ["Owned fixture workspace is unavailable."],
    render: () => (
      <SettingsFixtureFrame>
        <div className="h-96">
          <DiffPanel
            mode="embedded"
            composerDraftTarget={remainingThreadRef}
            workspaceUnavailable="Owned fixture workspace is unavailable."
          />
        </div>
      </SettingsFixtureFrame>
    ),
  },
  {
    id: "diff-panel-branch",
    title: "Diff scope, comparison target and rendered file",
    modules: ["components/DiffPanel.tsx"],
    owners: ["DiffPanel"],
    anchors: [
      "Branch changes",
      "feature/typography",
      "main",
      "This diff was truncated because it exceeded the preview limit. The changes shown are incomplete.",
    ],
    render: () => <ActiveDiff />,
  },
  {
    id: "diff-panel-raw",
    title: "Raw diff reason and contents",
    modules: ["components/DiffPanel.tsx"],
    owners: ["DiffPanel"],
    anchors: ["Unsupported diff format. Showing raw patch.", "Owned fixture raw patch text"],
    render: () => <ActiveDiff />,
  },
  {
    id: "diff-panel-error",
    title: "Diff error and missing patch",
    modules: ["components/DiffPanel.tsx"],
    owners: ["DiffPanel"],
    anchors: ["Owned fixture diff preview failed.", "No patch available for this selection."],
    render: () => <ActiveDiff />,
  },
  {
    id: "diff-panel-missing",
    title: "Missing diff source",
    modules: ["components/DiffPanel.tsx"],
    owners: ["DiffPanel"],
    anchors: ["No patch available for this selection."],
    render: () => <ActiveDiff />,
  },
  {
    id: "diff-panel-net-zero",
    title: "Resolved empty diff",
    modules: ["components/DiffPanel.tsx"],
    owners: ["DiffPanel"],
    anchors: ["No net changes in this selection."],
    render: () => <ActiveDiff />,
  },
  {
    id: "diff-panel-turn-empty",
    title: "No completed turn diff",
    modules: ["components/DiffPanel.tsx"],
    owners: ["DiffPanel"],
    anchors: ["No completed turns yet."],
    render: () => <ActiveDiff emptyTurns />,
  },
  {
    id: "diff-panel-not-repo",
    title: "Non Git workspace diff",
    modules: ["components/DiffPanel.tsx"],
    owners: ["DiffPanel"],
    anchors: ["Turn diffs are unavailable because this project is not a git repository."],
    render: () => <ActiveDiff />,
  },
  {
    id: "git-actions-publish-dialog",
    title: "Actual repository publish wizard",
    modules: ["components/GitActionsControl.tsx"],
    owners: ["PublishRepositoryDialog"],
    anchors: ["Publish repository", "Step 1", "Step 2", "Step 3", "GitHub"],
    render: () => (
      <SettingsFixtureFrame>
        <div className="@container/header-actions w-full">
          <GitActionsControl gitCwd={publishCwd} activeThreadRef={remainingThreadRef} />
        </div>
      </SettingsFixtureFrame>
    ),
  },
  {
    id: "settings-share-current-unreachable",
    title: "Current client and unreachable pairing help",
    modules: ["components/settings/remote-servers/ShareTab.tsx"],
    owners: ["PairingLinkListRow", "ConnectedClientListRow"],
    anchors: [
      "This device",
      "Copy the token and pair from another client using this backend's reachable host.",
    ],
    render: () => (
      <SettingsFixtureFrame>
        <ShareTab />
      </SettingsFixtureFrame>
    ),
  },
  {
    id: "settings-share-empty-access",
    title: "Empty pairing and client list",
    modules: ["components/settings/remote-servers/ShareTab.tsx"],
    owners: ["PairingClientsList"],
    anchors: ["No pairing links or client sessions."],
    render: () => (
      <SettingsFixtureFrame>
        <ShareTab />
      </SettingsFixtureFrame>
    ),
  },
];
