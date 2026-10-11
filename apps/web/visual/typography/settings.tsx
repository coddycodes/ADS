import { useState } from "react";
import type { PreviewViewportSetting } from "@bibcode/contracts";

import type { Fixture } from "./types";
import { BrowserDeviceToolbar } from "../../src/browser/BrowserDeviceToolbar";
import { ProviderInstanceCard } from "../../src/components/settings/ProviderInstanceCard";
import { ProviderModelsSection } from "../../src/components/settings/ProviderModelsSection";
import { ProviderSettingsForm } from "../../src/components/settings/ProviderSettingsForm";
import {
  PROVIDER_CLIENT_DEFINITIONS,
  getDriverOption,
} from "../../src/components/settings/providerDriverMeta";
import { RedactedSensitiveText } from "../../src/components/settings/RedactedSensitiveText";
import { ResourceDiagnosticsSections } from "../../src/components/settings/ResourceDiagnosticsSections";
import { SettingsSection, SettingsRow } from "../../src/components/settings/settingsLayout";
import { ProviderUsageControl } from "../../src/components/status-bar/ProviderUsageControl";
import { ProviderUsageDetail } from "../../src/components/status-bar/ProviderUsageDetail";
import { ProviderUsageWindowMeter } from "../../src/components/status-bar/ProviderUsageWindowMeter";
import { ResourceUsageSegment } from "../../src/components/status-bar/ResourceUsageSegment";
import { AddProviderInstanceDialog } from "../../src/components/settings/AddProviderInstanceDialog";
import { DiagnosticsSettingsPanel } from "../../src/components/settings/DiagnosticsSettings";
import { KeybindingsSettingsPanel } from "../../src/components/settings/KeybindingsSettings";
import {
  AboutSettingsPanel,
  ProviderSettingsPanel,
} from "../../src/components/settings/SettingsPanels";
import { ShareTab } from "../../src/components/settings/remote-servers/ShareTab";
import { PreviewView } from "../../src/components/preview/PreviewView";
import { SettingsFixtureFrame } from "./settings-ports";
import {
  fixtureDriver,
  fixtureInstanceId,
  fixtureThreadRef,
  providerInstance,
  providerModels,
  liveProvider,
  resourceProcesses,
  resourceHistory,
  usageViewModel,
  usageWindow,
} from "./settings-data";

const noChange = () => undefined;

function DeviceToolbarFixture() {
  const [setting, setSetting] = useState<
    Exclude<PreviewViewportSetting, { readonly _tag: "fill" }>
  >({ _tag: "freeform", width: 1280, height: 720 });
  const [aspectRatio, setAspectRatio] = useState<number | null>(null);
  return (
    <BrowserDeviceToolbar
      setting={setting}
      width={760}
      aspectRatio={aspectRatio}
      onAspectRatioChange={setAspectRatio}
      onChange={async (next) => {
        if (next._tag !== "fill") setSetting(next);
      }}
    />
  );
}

function ProviderCardFixture({ authenticated = true }: { readonly authenticated?: boolean }) {
  const [instance, setInstance] = useState(
    authenticated ? providerInstance : { ...providerInstance, environment: [] },
  );
  const [expanded, setExpanded] = useState(true);
  const [hidden, setHidden] = useState<ReadonlyArray<string>>(["hidden-fixture-model"]);
  const [favorites, setFavorites] = useState<ReadonlyArray<string>>(["gpt-6.1-sol"]);
  const [order, setOrder] = useState<ReadonlyArray<string>>([]);
  return (
    <ProviderInstanceCard
      instanceId={fixtureInstanceId}
      instance={instance}
      driverOption={getDriverOption(fixtureDriver)}
      liveProvider={
        authenticated
          ? liveProvider
          : {
              ...liveProvider,
              auth: { status: "unauthenticated", email: "typography@example.invalid" },
            }
      }
      isExpanded={expanded}
      onExpandedChange={setExpanded}
      onUpdate={setInstance}
      hiddenModels={hidden}
      favoriteModels={favorites}
      modelOrder={order}
      onHiddenModelsChange={setHidden}
      onFavoriteModelsChange={setFavorites}
      onModelOrderChange={setOrder}
    />
  );
}

function ModelsFixture() {
  const [customModels, setCustomModels] = useState<ReadonlyArray<string>>(["owned-custom-model"]);
  const [hidden, setHidden] = useState<ReadonlyArray<string>>(["hidden-fixture-model"]);
  const [favorites, setFavorites] = useState<ReadonlyArray<string>>(["gpt-6.1-sol"]);
  const [order, setOrder] = useState<ReadonlyArray<string>>([]);
  return (
    <ProviderModelsSection
      instanceId={fixtureInstanceId}
      driverKind={fixtureDriver}
      models={providerModels}
      customModels={customModels}
      hiddenModels={hidden}
      favoriteModels={favorites}
      modelOrder={order}
      onChange={setCustomModels}
      onHiddenModelsChange={setHidden}
      onFavoriteModelsChange={setFavorites}
      onModelOrderChange={setOrder}
    />
  );
}

function ResourceFixture() {
  const [windowMs, setWindowMs] = useState(15 * 60_000);
  return (
    <ResourceDiagnosticsSections
      processData={resourceProcesses}
      processError={null}
      isProcessPending={false}
      signalingPid={null}
      supportsInterrupt={true}
      onSignal={noChange}
      liveHeaderAction={null}
      resourceData={resourceHistory}
      resourceError={null}
      isResourcePending={false}
      resourceWindowMs={windowMs}
      onSelectResourceWindow={setWindowMs}
      historyHeaderAction={null}
    />
  );
}

export const settingsFixtures: readonly Fixture[] = [
  {
    id: "settings-add-provider",
    title: "Add provider dialog",
    modules: ["components/settings/AddProviderInstanceDialog.tsx"],
    owners: ["AddProviderInstanceDialog"],
    anchors: ["Add provider instance", "Codex", "Coming Soon"],
    render: () => (
      <SettingsFixtureFrame>
        <AddProviderInstanceDialog open onOpenChange={noChange} />
      </SettingsFixtureFrame>
    ),
  },
  {
    id: "settings-diagnostics-panel",
    title: "Diagnostics tables and checked timestamps",
    modules: ["components/settings/DiagnosticsSettings.tsx"],
    owners: [
      "StatBlock",
      "ExpandableText",
      "DiagnosticsTable",
      "TraceIdCell",
      "DiagnosticsLastChecked",
      "DiagnosticsSettingsPanel",
    ],
    anchors: [
      "Spans",
      "Show full error",
      "fixture.trace.slow",
      "trace-0123456789ab...vwxyz-9876",
      "Checked",
    ],
    render: () => (
      <SettingsFixtureFrame>
        <DiagnosticsSettingsPanel />
      </SettingsFixtureFrame>
    ),
  },
  {
    id: "settings-keybindings",
    title: "Keybinding row and condition editor",
    modules: ["components/settings/KeybindingsSettings.tsx"],
    owners: [
      "ExpandableHeaderSearch",
      "WhenExpressionNodeEditor",
      "WhenExpressionBuilder",
      "KeybindingTableRow",
      "KeybindingsSettingsPanel",
    ],
    anchors: ["Terminal: Split", "When", "Condition", "Group", "Not"],
    render: () => (
      <SettingsFixtureFrame>
        <KeybindingsSettingsPanel />
      </SettingsFixtureFrame>
    ),
  },
  {
    id: "settings-provider-last-checked",
    title: "Provider checked timestamp",
    modules: ["components/settings/SettingsPanels.tsx"],
    owners: ["ProviderLastChecked"],
    anchors: ["Checked"],
    render: () => (
      <SettingsFixtureFrame>
        <ProviderSettingsPanel />
      </SettingsFixtureFrame>
    ),
  },
  {
    id: "settings-about-version",
    title: "About version title",
    modules: ["components/settings/SettingsPanels.tsx"],
    owners: ["AboutVersionTitle"],
    anchors: ["Version", "Current version of the application."],
    render: () => (
      <SettingsFixtureFrame>
        <AboutSettingsPanel />
      </SettingsFixtureFrame>
    ),
  },
  {
    id: "settings-share-clients",
    title: "Pairing links, clients and advertised endpoints",
    modules: ["components/settings/remote-servers/ShareTab.tsx"],
    owners: [
      "PairingLinkListRow",
      "ConnectedClientListRow",
      "AdvertisedEndpointListRow",
      "PairingClientsList",
    ],
    anchors: [
      "Fixture pairing link",
      "Fixture desktop client",
      "Fixture network endpoint",
      "Paired clients",
    ],
    render: () => (
      <SettingsFixtureFrame>
        <ShareTab />
      </SettingsFixtureFrame>
    ),
  },
  {
    id: "settings-preview-agent-controller",
    title: "Preview agent controller label",
    modules: ["components/preview/PreviewView.tsx"],
    owners: ["PreviewView"],
    anchors: ["Agent controlling browser"],
    render: () => (
      <SettingsFixtureFrame previewController="agent">
        <div className="flex h-80 flex-col">
          <PreviewView threadRef={fixtureThreadRef} visible />
        </div>
      </SettingsFixtureFrame>
    ),
  },
  {
    id: "settings-preview-human-controller",
    title: "Preview human controller label",
    modules: ["components/preview/PreviewView.tsx"],
    owners: ["PreviewView"],
    anchors: ["Human control"],
    render: () => (
      <SettingsFixtureFrame previewController="human">
        <div className="flex h-80 flex-col">
          <PreviewView threadRef={fixtureThreadRef} visible />
        </div>
      </SettingsFixtureFrame>
    ),
  },
  {
    id: "settings-device-toolbar",
    title: "Browser device toolbar",
    modules: ["browser/BrowserDeviceToolbar.tsx"],
    owners: ["BrowserDeviceToolbar"],
    anchors: ["Dimensions", "Responsive", "×"],
    render: () => <DeviceToolbarFixture />,
  },
  {
    id: "settings-provider-card",
    title: "Provider instance with email and environment",
    modules: [
      "components/settings/ProviderInstanceCard.tsx",
      "components/settings/RedactedSensitiveText.tsx",
      "components/settings/ProviderModelsSection.tsx",
      "components/settings/ProviderSettingsForm.tsx",
    ],
    owners: [
      "ProviderInstanceCard",
      "ProviderEnvironmentSection",
      "ProviderAuthEmail",
      "RedactedSensitiveText",
      "ProviderModelsSection",
      "ProviderSettingsFieldRow",
    ],
    anchors: [
      "Typography provider instance",
      "Authenticated as",
      "Environment variables",
      "Sensitive",
      "Sol fixture model",
    ],
    render: () => <ProviderCardFixture />,
  },
  {
    id: "settings-provider-card-unauthenticated",
    title: "Provider email prefix and empty environment",
    modules: ["components/settings/ProviderInstanceCard.tsx"],
    owners: ["ProviderInstanceCard", "ProviderEnvironmentSection", "ProviderAuthEmail"],
    anchors: [
      "Email",
      "Add variables to pass API keys, base URLs, or other per-instance CLI settings.",
    ],
    render: () => <ProviderCardFixture authenticated={false} />,
  },
  {
    id: "settings-provider-models",
    title: "Provider model controls",
    modules: ["components/settings/ProviderModelsSection.tsx"],
    owners: ["ProviderModelsSection"],
    anchors: ["Sol fixture model", "Hidden fixture model", "Custom fixture model", "Add"],
    render: () => <ModelsFixture />,
  },
  {
    id: "settings-provider-fields",
    title: "Real provider schema fields",
    modules: ["components/settings/ProviderSettingsForm.tsx"],
    owners: ["ProviderSettingsFieldRow"],
    anchors: ["Binary path", "Path to the Codex binary used by this instance."],
    render: () => (
      <>
        {PROVIDER_CLIENT_DEFINITIONS.map((definition) => (
          <ProviderSettingsForm
            key={definition.value}
            definition={definition}
            value={{}}
            idPrefix={`fixture-field-${definition.value}`}
            variant="card"
            onChange={noChange}
          />
        ))}
      </>
    ),
  },
  {
    id: "settings-redacted-email",
    title: "Account email visibility",
    modules: ["components/settings/RedactedSensitiveText.tsx"],
    owners: ["RedactedSensitiveText"],
    anchors: ["typography@example.invalid"],
    render: () => (
      <RedactedSensitiveText
        value="typography@example.invalid"
        ariaLabel="Toggle fixture email visibility"
        revealTooltip="Click to reveal email"
        hideTooltip="Click to hide email"
      />
    ),
  },
  {
    id: "settings-resource-diagnostics",
    title: "Live process and resource history data",
    modules: ["components/settings/ResourceDiagnosticsSections.tsx"],
    owners: [
      "SummaryMetric",
      "SortHeader",
      "HistorySortHeader",
      "LiveProcessTable",
      "ResourceHistoryWindowSelector",
      "HistoryMetricToggle",
      "ResourceHistoryChart",
      "HistoryProcessTable",
      "ResourceDiagnosticsSections",
    ],
    anchors: [
      "Live Processes",
      "Resource History",
      "Codex fixture worker",
      "BiBCode Core average",
      "External Tooling average",
    ],
    render: () => <ResourceFixture />,
  },
  {
    id: "settings-layout",
    title: "Settings section and row",
    modules: ["components/settings/settingsLayout.tsx"],
    owners: ["SettingsSection", "SettingsRow"],
    anchors: [
      "Typography settings section",
      "Typography settings row",
      "Secondary text supplied through the real SettingsRow description prop.",
    ],
    render: () => (
      <SettingsSection title="Typography settings section">
        <SettingsRow
          title="Typography settings row"
          description="Secondary text supplied through the real SettingsRow description prop."
          status="Owned fixture status text"
        />
      </SettingsSection>
    ),
  },
  {
    id: "status-provider-usage",
    title: "Provider usage control",
    modules: ["components/status-bar/ProviderUsageControl.tsx"],
    owners: ["ProviderUsageControl"],
    anchors: ["Session", "28% left"],
    render: () => (
      <ProviderUsageControl
        viewModel={usageViewModel}
        statusBarUsageMode="detailed"
        iconOnly={false}
        onOpenProviderSettings={noChange}
      />
    ),
  },
  {
    id: "status-provider-usage-detail",
    title: "Provider usage detail",
    modules: ["components/status-bar/ProviderUsageDetail.tsx"],
    owners: ["ProviderUsageDetail"],
    anchors: ["Codex usage", "Fixture plan", "Next expiry: in 1d", "Provider settings"],
    render: () => (
      <ProviderUsageDetail viewModel={usageViewModel} onOpenProviderSettings={noChange} />
    ),
  },
  {
    id: "status-provider-usage-unavailable",
    title: "Provider usage state copy",
    modules: ["components/status-bar/ProviderUsageDetail.tsx"],
    owners: ["ProviderUsageDetail"],
    anchors: ["Usage is unavailable for this provider."],
    render: () => (
      <ProviderUsageDetail
        viewModel={{
          ...usageViewModel,
          status: "unavailable",
          windows: [],
          detailedWindows: [],
          compactWindows: [],
          credits: null,
        }}
        onOpenProviderSettings={noChange}
      />
    ),
  },
  {
    id: "status-provider-window-meter",
    title: "Provider usage meters",
    modules: ["components/status-bar/ProviderUsageWindowMeter.tsx"],
    owners: ["ProviderUsageWindowMeter"],
    anchors: ["28% remaining", "Resets in 5m", "Caution"],
    render: () => (
      <>
        <ProviderUsageWindowMeter variant="detail" window={usageWindow} />
        <ProviderUsageWindowMeter variant="footer" window={usageWindow} />
      </>
    ),
  },
  {
    id: "status-resource-usage",
    title: "Resource usage popover",
    modules: ["components/status-bar/ResourceUsageSegment.tsx"],
    owners: ["ResourceUsageSegment", "ResourceTotalsCard"],
    anchors: ["Resource Manager", "BiBCode Core", "External Tooling", "Highest consumers"],
    render: () => (
      <ResourceUsageSegment
        diagnostics={{ diagnostics: resourceProcesses, queryError: null }}
        localDiagnostics={null}
        terminalCount={3}
        iconOnly={false}
      />
    ),
  },
  {
    id: "status-resource-unavailable",
    title: "Unavailable resource totals",
    modules: ["components/status-bar/ResourceUsageSegment.tsx"],
    owners: ["ResourceUsageSegment", "ResourceTotalsCard"],
    anchors: ["Resource Manager", "Unavailable", "BiBCode Core", "External Tooling"],
    render: () => (
      <ResourceUsageSegment
        diagnostics={{ diagnostics: null, queryError: null }}
        localDiagnostics={null}
        terminalCount={0}
        iconOnly={false}
      />
    ),
  },
];
