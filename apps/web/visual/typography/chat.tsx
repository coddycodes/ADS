import {
  ActivityEntryId,
  ActivityRecordId,
  ProviderDriverKind,
  ThreadId,
  TurnId,
  type ActivityEntry,
  type PreviewAnnotationPayload,
} from "@bibcode/contracts";
import { ActivityEntryRow } from "../../src/components/activity/ActivityEntryRow";
import { ChangedFilesCard } from "../../src/components/chat/ChangedFilesTree";
import { ComposerPendingElementContextChip } from "../../src/components/chat/ComposerPendingElementContexts";
import { ComposerPreviewAnnotationCards } from "../../src/components/chat/ComposerPreviewAnnotationCards";
import { ModelListRow } from "../../src/components/chat/ModelListRow";
import { ProviderInstanceIcon } from "../../src/components/chat/ProviderInstanceIcon";
import {
  Combobox,
  ComboboxInput,
  ComboboxList,
  ComboboxPopup,
} from "../../src/components/ui/combobox";
import { useTheme } from "../../src/hooks/useTheme";
import type { ElementContextDraft } from "../../src/lib/elementContext";
import { fixtureInstanceId } from "./settings-data";
import type { Fixture } from "./types";

const noChange = () => undefined;
const createdAt = "2026-10-10T12:00:00.000Z";
const codex = ProviderDriverKind.make("codex");
const claude = ProviderDriverKind.make("claudeCode");
const entries: readonly Pick<ActivityEntry, "kind" | "title" | "detail" | "tone">[] = [
  {
    kind: "command",
    title: "Verify typography",
    detail: "Inspect real component text in both themes.",
    tone: "tool",
  },
  {
    kind: "result",
    title: "All fixture requirements observed",
    detail: "The result detail uses the text floor.",
    tone: "success",
  },
  {
    kind: "error",
    title: "Sample actionable error",
    detail: "Retry after selecting an available model.",
    tone: "error",
  },
];
const context: ElementContextDraft = {
  id: "typography-element",
  threadId: ThreadId.make("typography-thread"),
  pickedAt: createdAt,
  pageUrl: "http://localhost:3000/fixture",
  pageTitle: "Typography fixture",
  tagName: "button",
  selector: "button.save",
  htmlPreview: "<button>Save</button>",
  componentName: "SaveButton",
  source: {
    functionName: "SaveButton",
    fileName: "/workspace/src/SaveButton.tsx",
    lineNumber: 42,
    columnNumber: 2,
  },
  styles: "",
};
const annotation: PreviewAnnotationPayload = {
  id: "typography-annotation",
  pageUrl: context.pageUrl,
  pageTitle: context.pageTitle,
  comment: "Keep annotation counters readable.",
  elements: [
    {
      id: "element-1",
      rect: { x: 1, y: 2, width: 30, height: 20 },
      element: { ...context, stack: [] },
    },
  ],
  regions: [{ id: "region-1", rect: { x: 1, y: 2, width: 30, height: 20 } }],
  strokes: [
    {
      id: "stroke-1",
      color: "#000000",
      width: 2,
      points: [{ x: 1, y: 1 }],
      bounds: { x: 1, y: 1, width: 1, height: 1 },
    },
  ],
  styleChanges: [
    {
      targetId: "element-1",
      selector: "button.save",
      property: "font-size",
      previousValue: "11px",
      value: "12px",
    },
  ],
  screenshot: null,
  createdAt,
};

function ChangedFilesFixture() {
  const { resolvedTheme } = useTheme();
  return (
    <ChangedFilesCard
      turnId={TurnId.make("typography-turn")}
      files={[
        { path: "src/components/SaveButton.tsx", kind: "modified", additions: 12, deletions: 4 },
        { path: "src/styles.css", kind: "added", additions: 8, deletions: 0 },
      ]}
      allDirectoriesExpanded
      resolvedTheme={resolvedTheme}
      onToggleAllDirectories={noChange}
      onOpenTurnDiff={noChange}
    />
  );
}

function ModelRowsFixture() {
  const options = ["gpt-6.1-sol", "claude-sonnet"];
  return (
    <Combobox items={options} open value="gpt-6.1-sol">
      <ComboboxInput aria-label="Fixture model search" />
      <ComboboxPopup>
        <ComboboxList>
          <ModelListRow
            index={0}
            model={{ slug: options[0]!, name: "Sol fixture model", subProvider: "OpenAI" }}
            instanceId={fixtureInstanceId}
            driverKind={codex}
            providerDisplayName="Codex Personal"
            isFavorite
            isSelected
            showProvider
            showNewBadge
            jumpLabel="⌘1"
            onToggleFavorite={noChange}
          />
          <ModelListRow
            index={1}
            model={{ slug: options[1]!, name: "Sonnet fixture model" }}
            instanceId={fixtureInstanceId}
            driverKind={claude}
            providerDisplayName="Claude Work"
            isFavorite={false}
            isSelected={false}
            showProvider
            disabledReason="This model cannot continue the selected conversation."
            onToggleFavorite={noChange}
          />
        </ComboboxList>
      </ComboboxPopup>
    </Combobox>
  );
}

export const chatFixtures: readonly Fixture[] = [
  {
    id: "chat-changed-files",
    title: "Changed file names and counters",
    modules: ["components/chat/ChangedFilesTree.tsx"],
    owners: ["ChangedFilesCard", "ChangedFilesTree"],
    anchors: ["2 changed files", "SaveButton.tsx", "styles.css", "Collapse all"],
    render: () => <ChangedFilesFixture />,
  },
  {
    id: "chat-element-context",
    title: "Picked element source label",
    modules: ["components/chat/ComposerPendingElementContexts.tsx"],
    owners: ["ComposerPendingElementContextChip"],
    anchors: ["<SaveButton>", "SaveButton.tsx:42"],
    render: () => <ComposerPendingElementContextChip context={context} onRemove={noChange} />,
  },
  {
    id: "chat-preview-annotation",
    title: "Preview annotation targets",
    modules: ["components/chat/ComposerPreviewAnnotationCards.tsx"],
    owners: ["TargetStat", "ComposerPreviewAnnotationCards"],
    anchors: ["Keep annotation counters readable.", "<SaveButton>"],
    render: () => (
      <ComposerPreviewAnnotationCards
        annotations={[annotation]}
        images={[]}
        onRemove={noChange}
        onExpandImage={noChange}
      />
    ),
  },
  {
    id: "chat-model-rows",
    title: "Model names, provider labels and keyboard hints",
    modules: ["components/chat/ModelListRow.tsx"],
    owners: ["ModelListRow"],
    anchors: ["Sol fixture model", "Codex Personal", "New", "Sonnet fixture model", "Claude Work"],
    render: () => <ModelRowsFixture />,
  },
  {
    id: "chat-instance-icons",
    title: "Provider initials",
    modules: ["components/chat/ProviderInstanceIcon.tsx"],
    owners: ["ProviderInstanceIcon"],
    anchors: ["CP", "CW"],
    render: () => (
      <div className="flex items-center gap-8">
        <ProviderInstanceIcon driverKind={codex} displayName="Codex Personal" showBadge />
        <ProviderInstanceIcon
          driverKind={claude}
          displayName="Claude Work"
          accentColor="#6366f1"
          showBadge
        />
      </div>
    ),
  },
  {
    id: "activity-entry",
    title: "Activity kinds, detail and timestamps",
    modules: ["components/activity/ActivityEntryRow.tsx"],
    owners: ["ActivityEntryRow"],
    anchors: [
      "Command",
      "Verify typography",
      "Result",
      "All fixture requirements observed",
      "Error",
      "Sample actionable error",
    ],
    render: () => (
      <div className="space-y-3">
        {entries.map((entry, index) => (
          <ActivityEntryRow
            key={entry.kind}
            entry={{
              ...entry,
              id: ActivityEntryId.make(`typography-entry-${index}`),
              ownerKind: "actor",
              ownerId: ActivityRecordId.make("typography-actor"),
              createdAt,
            }}
            timestampFormat="24-hour"
          />
        ))}
      </div>
    ),
  },
];
