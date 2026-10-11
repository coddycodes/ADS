import {
  EnvironmentId,
  OrchestrationProposedPlanId,
  ProviderDriverKind,
  ProviderInstanceId,
  type ServerProvider,
} from "@bibcode/contracts";
import { useState } from "react";
import ChatMarkdown from "../../src/components/ChatMarkdown";
import PlanSidebar from "../../src/components/PlanSidebar";
import { ProviderModelPicker } from "../../src/components/chat/ProviderModelPicker";
import type { ModelEsque } from "../../src/components/chat/providerIconUtils";
import { deriveProviderInstanceEntries } from "../../src/providerInstances";
import type { ActivePlanState, LatestProposedPlanState } from "../../src/session-logic";
import { formatTimestamp } from "../../src/timestampFormat";
import { activityContextFixtures } from "./activity-context";
import type { Fixture } from "./types";

const createdAt = "2026-10-10T12:00:00.000Z";
const environmentId = EnvironmentId.make("typography-chat-owned");
const cwd = "/owned-fixture/workspace";
const markdown = [
  "File links use real MarkdownFileLink chips: [first](src/left/typography-link.ts:12:2) and [second](src/right/typography-link.ts:28).",
  "",
  "The footnote reference[^typography] and its back reference retain readable labels.",
  "",
  "```text title=typography-example.txt",
  "A plain text code block retains its real filename title.",
  "```",
  "",
  "[^typography]: Typography footnote detail.",
].join("\n");
const activePlan: ActivePlanState = {
  createdAt,
  turnId: null,
  explanation: "Review all plan text at the production font size.",
  steps: [
    { step: "Completed typography step", status: "completed" },
    { step: "Current typography step", status: "inProgress" },
    { step: "Pending typography step", status: "pending" },
  ],
};
const proposedPlan: LatestProposedPlanState = {
  id: OrchestrationProposedPlanId.make("typography-proposed-plan"),
  createdAt,
  updatedAt: createdAt,
  turnId: null,
  planMarkdown:
    "# Typography implementation plan\n\nThe expanded plan uses the real ChatMarkdown child.",
  implementedAt: null,
  implementationThreadId: null,
};
function PlanFixture({
  empty = false,
  untitled = false,
}: {
  readonly empty?: boolean;
  readonly untitled?: boolean;
}) {
  return (
    <div className="h-[520px]">
      <PlanSidebar
        activePlan={empty || untitled ? null : activePlan}
        activeProposedPlan={
          empty
            ? null
            : untitled
              ? {
                  ...proposedPlan,
                  planMarkdown: "The untitled plan uses the real ChatMarkdown child.",
                }
              : proposedPlan
        }
        environmentId={environmentId}
        markdownCwd={cwd}
        workspaceRoot={undefined}
        timestampFormat="24-hour"
      />
    </div>
  );
}
const instanceId = ProviderInstanceId.make("codex_typography_chat");
const modelOptions: readonly ModelEsque[] = [
  { slug: "typography-sol", name: "Typography Sol model", shortName: "Typography Sol" },
  { slug: "typography-reviewed", name: "Typography Reviewed model", shortName: "Reviewed" },
];
const provider: ServerProvider = {
  instanceId,
  driver: ProviderDriverKind.make("codex"),
  displayName: "Typography Codex",
  accentColor: "#7c3aed",
  enabled: true,
  installed: true,
  version: "1.2.3",
  status: "ready",
  auth: { status: "authenticated" },
  checkedAt: createdAt,
  models: modelOptions.map((model) => ({
    slug: model.slug,
    name: model.name,
    isCustom: false,
    capabilities: null,
  })),
  slashCommands: [],
  skills: [],
  agents: [],
};
const instanceEntries = deriveProviderInstanceEntries([provider]);
const modelOptionsByInstance = new Map([[instanceId, modelOptions]]);
function PickerFixture({ initiallyOpen = false }: { readonly initiallyOpen?: boolean }) {
  const [open, setOpen] = useState(initiallyOpen);
  const [selection, setSelection] = useState({ instanceId, model: "typography-sol" });
  return (
    <ProviderModelPicker
      activeInstanceId={selection.instanceId}
      model={selection.model}
      lockedProvider={null}
      instanceEntries={instanceEntries}
      modelOptionsByInstance={modelOptionsByInstance}
      open={open}
      onOpenChange={setOpen}
      onInstanceModelChange={(nextInstanceId, model) =>
        setSelection({ instanceId: nextInstanceId, model })
      }
    />
  );
}
export const chatFixtures: readonly Fixture[] = [
  {
    id: "chat-markdown",
    title: "Markdown file chips, footnotes and code title",
    modules: ["components/ChatMarkdown.tsx", "index.css"],
    owners: ["ChatMarkdown", "MarkdownFileLink", "MarkdownCodeBlockTitleContent"],
    anchors: [
      "typography-link.ts",
      "src/left",
      "src/right",
      "L12:C2",
      "L28",
      "Typography footnote detail.",
      "typography-example.txt",
      "1",
      "↩",
    ],
    render: () => <ChatMarkdown text={markdown} cwd={cwd} />,
  },
  {
    id: "chat-markdown-file-tooltip",
    title: "Markdown file path tooltip",
    modules: ["components/ChatMarkdown.tsx"],
    owners: ["MarkdownFileLink"],
    anchors: ["src/left/typography-link.ts"],
    render: () => <ChatMarkdown text={markdown} cwd={cwd} />,
  },
  {
    id: "plan-sidebar",
    title: "Active plan explanation, timestamp, steps and title",
    modules: ["components/PlanSidebar.tsx"],
    owners: ["PlanSidebar"],
    anchors: [
      formatTimestamp(createdAt, "24-hour"),
      "Review all plan text at the production font size.",
      "Steps",
      "Completed typography step",
      "Current typography step",
      "Pending typography step",
      "Typography implementation plan",
    ],
    render: () => <PlanFixture />,
  },
  {
    id: "plan-sidebar-empty",
    title: "Plan empty state",
    modules: ["components/PlanSidebar.tsx"],
    owners: ["PlanSidebar"],
    anchors: ["No active plan yet.", "Plans will appear here when generated."],
    render: () => <PlanFixture empty />,
  },
  {
    id: "plan-sidebar-untitled",
    title: "Proposed plan default title",
    modules: ["components/PlanSidebar.tsx"],
    owners: ["PlanSidebar"],
    anchors: ["Full Plan"],
    render: () => <PlanFixture untitled />,
  },
  ...activityContextFixtures,
  {
    id: "chat-provider-picker",
    title: "Provider picker trigger",
    modules: ["components/chat/ProviderModelPicker.tsx"],
    owners: ["ProviderModelPicker"],
    anchors: ["Typography Sol", "TC"],
    render: () => <PickerFixture />,
  },
  {
    id: "chat-provider-picker-open",
    title: "Provider picker with actual popup and model rows",
    modules: ["components/chat/ProviderModelPicker.tsx"],
    owners: ["ProviderModelPicker"],
    anchors: ["Typography Sol", "TC"],
    render: () => <PickerFixture initiallyOpen />,
  },
];
