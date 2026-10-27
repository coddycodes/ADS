// @vitest-environment happy-dom

import {
  ProviderDriverKind,
  ProviderInstanceId,
  type ProviderOptionDescriptor,
  type ProviderOptionSelection,
  type ServerProviderModel,
} from "@coddy/contracts";
import { act, type ReactElement, useEffect, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterAll, afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { TooltipProvider } from "../ui/tooltip";

const testState = vi.hoisted(() => ({
  setProviderModelOptions: vi.fn(),
  addToast: vi.fn(),
}));

vi.mock("../../composerDraftStore", () => ({
  useComposerDraftStore: (selector: (store: unknown) => unknown) =>
    selector({ setProviderModelOptions: testState.setProviderModelOptions }),
}));

vi.mock("../ui/toast", () => ({
  toastManager: { add: testState.addToast },
}));

import {
  ComposerTraitControls,
  type ProviderOptionUpdater,
  shouldRenderTraitsControls,
  TraitsPicker,
  useProviderOptionUpdater,
} from "./TraitsPicker";

const MODEL = "test-model";
const CODEX = ProviderDriverKind.make("codex");
const CLAUDE = ProviderDriverKind.make("claudeAgent");

interface MountedTree {
  readonly container: HTMLDivElement;
  readonly root: Root;
}

const mountedTrees: MountedTree[] = [];
const suiteGetAnimationsDescriptor = Object.getOwnPropertyDescriptor(
  Element.prototype,
  "getAnimations",
);
let originalGetAnimationsDescriptor: PropertyDescriptor | undefined;

function selectDescriptor(
  id: string,
  label: string,
  options: ReadonlyArray<{ id: string; label: string; isDefault?: boolean }>,
  promptInjectedValues?: ReadonlyArray<string>,
): Extract<ProviderOptionDescriptor, { type: "select" }> {
  return {
    id,
    label,
    type: "select",
    options,
    ...(promptInjectedValues ? { promptInjectedValues } : {}),
  };
}

function booleanDescriptor(
  id: string,
  label: string,
): Extract<ProviderOptionDescriptor, { type: "boolean" }> {
  return { id, label, type: "boolean" };
}

function modelsWith(
  descriptors: ReadonlyArray<ProviderOptionDescriptor>,
): ReadonlyArray<ServerProviderModel> {
  return [
    {
      slug: MODEL,
      name: "Test Model",
      isCustom: false,
      capabilities: { optionDescriptors: descriptors },
    },
  ];
}

function selections(...entries: Array<[string, string | boolean]>): ProviderOptionSelection[] {
  return entries.map(([id, value]) => ({ id, value }));
}

function NoPersistenceHarness() {
  const [prompt, setPrompt] = useState("");
  return (
    <>
      <TraitsPicker
        provider={CLAUDE}
        models={modelsWith([effort, booleanDescriptor("thinking", "Thinking")])}
        model={MODEL}
        prompt={prompt}
        onPromptChange={setPrompt}
      />
      <output data-testid="prompt">{prompt}</output>
    </>
  );
}

function FastCommitHarness({
  onCommit,
}: {
  onCommit: (nextOptions: ReadonlyArray<ProviderOptionSelection> | undefined) => Promise<void>;
}) {
  const [modelOptions, setModelOptions] = useState<ReadonlyArray<ProviderOptionSelection>>([
    { id: "fastMode", value: false },
  ]);
  return (
    <ComposerTraitControls
      provider={CLAUDE}
      models={modelsWith([booleanDescriptor("fastMode", "Fast Mode")])}
      model={MODEL}
      prompt=""
      modelOptions={modelOptions}
      onPromptChange={vi.fn()}
      onModelOptionsChange={async (nextOptions) => {
        await onCommit(nextOptions);
        setModelOptions(nextOptions ?? []);
      }}
    />
  );
}

function FastAndEffortCommitHarness({
  onCommit,
}: {
  onCommit: (nextOptions: ReadonlyArray<ProviderOptionSelection> | undefined) => Promise<void>;
}) {
  const [modelOptions, setModelOptions] = useState<ReadonlyArray<ProviderOptionSelection>>([
    { id: "fastMode", value: false },
    { id: "effort", value: "low" },
  ]);
  return (
    <ComposerTraitControls
      provider={CLAUDE}
      models={modelsWith([effort, booleanDescriptor("fastMode", "Fast Mode")])}
      model={MODEL}
      prompt=""
      modelOptions={modelOptions}
      onPromptChange={vi.fn()}
      onModelOptionsChange={async (nextOptions) => {
        await onCommit(nextOptions);
        setModelOptions(nextOptions ?? []);
      }}
    />
  );
}

function DirectUpdaterHarness({
  onCommit,
  onReady,
}: {
  onCommit: (nextOptions: ReadonlyArray<ProviderOptionSelection> | undefined) => Promise<void>;
  onReady: (updater: ProviderOptionUpdater) => void;
}) {
  const updater = useProviderOptionUpdater(CLAUDE, undefined, MODEL, {
    onModelOptionsChange: onCommit,
  });
  useEffect(() => {
    onReady(updater);
  }, [onReady, updater]);
  return null;
}

async function mount(element: ReactElement): Promise<MountedTree> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const mounted = { container, root };
  mountedTrees.push(mounted);
  await act(async () => root.render(element));
  return mounted;
}

async function click(element: HTMLElement): Promise<void> {
  await act(async () => {
    element.click();
    await Promise.resolve();
  });
}

function buttonContaining(text: string): HTMLButtonElement {
  const button = Array.from(document.querySelectorAll<HTMLButtonElement>("button")).find(
    (candidate) => candidate.textContent?.includes(text),
  );
  expect(button).toBeDefined();
  return button!;
}

function radioItem(label: string): HTMLElement {
  const item = Array.from(document.querySelectorAll<HTMLElement>("[role='menuitemradio']")).find(
    (candidate) => candidate.textContent?.trim().startsWith(label),
  );
  expect(item).toBeDefined();
  return item!;
}

const effort = selectDescriptor(
  "effort",
  "Effort",
  [
    { id: "low", label: "Low" },
    { id: "high", label: "High", isDefault: true },
    { id: "ultrathink", label: "Ultrathink" },
  ],
  ["ultrathink"],
);

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
  testState.setProviderModelOptions.mockReset();
  testState.addToast.mockReset();
});

afterEach(async () => {
  for (const mounted of mountedTrees.splice(0)) {
    await act(async () => mounted.root.unmount());
    mounted.container.remove();
  }
  document.body.replaceChildren();
  if (originalGetAnimationsDescriptor) {
    Object.defineProperty(Element.prototype, "getAnimations", originalGetAnimationsDescriptor);
  } else {
    Reflect.deleteProperty(Element.prototype, "getAnimations");
  }
  originalGetAnimationsDescriptor = undefined;
  vi.restoreAllMocks();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

afterAll(() => {
  expect(Object.getOwnPropertyDescriptor(Element.prototype, "getAnimations")).toEqual(
    suiteGetAnimationsDescriptor,
  );
});

describe("TraitsPicker", () => {
  it("keeps Fast unchanged and blocks duplicate commits until the acknowledged update succeeds", async () => {
    let acknowledge!: () => void;
    const acknowledgement = new Promise<void>((resolve) => {
      acknowledge = resolve;
    });
    const onCommit = vi.fn(() => acknowledgement);
    await mount(<FastCommitHarness onCommit={onCommit} />);

    const fastButton = document.querySelector<HTMLButtonElement>(
      'button[aria-label="Enable fast mode"]',
    )!;
    await click(fastButton);

    expect(onCommit).toHaveBeenCalledTimes(1);
    expect(fastButton.disabled).toBe(false);
    expect(fastButton.getAttribute("aria-disabled")).toBe("true");
    expect(document.querySelector('button[aria-label="Applying fast mode"]')).not.toBeNull();
    expect(fastButton.getAttribute("aria-pressed")).toBe("false");

    await click(fastButton);
    expect(onCommit).toHaveBeenCalledTimes(1);

    await act(async () => {
      acknowledge();
      await acknowledgement;
    });

    expect(
      document
        .querySelector<HTMLButtonElement>('button[aria-label="Disable fast mode"]')
        ?.getAttribute("aria-pressed"),
    ).toBe("true");
  });

  it("resets pending controls when the provider model identity changes", async () => {
    let acknowledge!: () => void;
    const acknowledgement = new Promise<void>((resolve) => {
      acknowledge = resolve;
    });
    const oldCommit = vi.fn(() => acknowledgement);
    const newCommit = vi.fn().mockResolvedValue(undefined);
    const mounted = await mount(
      <ComposerTraitControls
        provider={CLAUDE}
        instanceId={ProviderInstanceId.make("claude-work")}
        models={modelsWith([booleanDescriptor("fastMode", "Fast Mode")])}
        model={MODEL}
        prompt=""
        modelOptions={selections(["fastMode", false])}
        onPromptChange={vi.fn()}
        onModelOptionsChange={oldCommit}
      />,
    );

    await click(
      document.querySelector<HTMLButtonElement>('button[aria-label="Enable fast mode"]')!,
    );
    expect(document.querySelector('button[aria-label="Applying fast mode"]')).not.toBeNull();

    await act(async () => {
      mounted.root.render(
        <ComposerTraitControls
          provider={ProviderDriverKind.make("opencode")}
          instanceId={ProviderInstanceId.make("opencode-work")}
          models={[
            {
              slug: "other-model",
              name: "Other Model",
              isCustom: false,
              capabilities: { optionDescriptors: [booleanDescriptor("fastMode", "Fast Mode")] },
            },
          ]}
          model="other-model"
          prompt=""
          modelOptions={selections(["fastMode", false])}
          onPromptChange={vi.fn()}
          onModelOptionsChange={newCommit}
        />,
      );
    });

    const nextFast = document.querySelector<HTMLButtonElement>(
      'button[aria-label="Enable fast mode"]',
    )!;
    expect(nextFast.getAttribute("aria-busy")).toBe("false");
    expect(nextFast.getAttribute("aria-disabled")).toBe("false");
    await click(nextFast);
    expect(newCommit).toHaveBeenCalledOnce();

    await act(async () => {
      acknowledge();
      await acknowledgement;
    });
    expect(newCommit).toHaveBeenCalledOnce();
  });

  it("serializes Fast and effort commits from the latest acknowledged selection", async () => {
    let acknowledgeFast!: () => void;
    const fastAcknowledgement = new Promise<void>((resolve) => {
      acknowledgeFast = resolve;
    });
    const onCommit = vi
      .fn<(nextOptions: ReadonlyArray<ProviderOptionSelection> | undefined) => Promise<void>>()
      .mockImplementationOnce(() => fastAcknowledgement)
      .mockResolvedValue(undefined);
    await mount(<FastAndEffortCommitHarness onCommit={onCommit} />);

    await click(
      document.querySelector<HTMLButtonElement>('button[aria-label="Enable fast mode"]')!,
    );

    const effortButton = document.querySelector<HTMLButtonElement>(
      'button[aria-label="Applying reasoning effort"]',
    )!;
    expect(effortButton.disabled).toBe(false);
    expect(effortButton.getAttribute("aria-disabled")).toBe("true");
    expect(effortButton.getAttribute("aria-busy")).toBe("true");
    await click(effortButton);
    expect(onCommit).toHaveBeenCalledTimes(1);

    await act(async () => {
      acknowledgeFast();
      await fastAcknowledgement;
    });

    await click(
      document.querySelector<HTMLButtonElement>('button[aria-label="Reasoning effort: Low"]')!,
    );
    await click(radioItem("High"));

    expect(onCommit).toHaveBeenCalledTimes(2);
    expect(onCommit).toHaveBeenLastCalledWith(
      expect.arrayContaining([
        { id: "fastMode", value: true },
        { id: "effort", value: "high" },
      ]),
    );
  });

  it("rejects a programmatic alternate descriptor while another option commit is pending", async () => {
    let acknowledgeFast!: () => void;
    const fastAcknowledgement = new Promise<void>((resolve) => {
      acknowledgeFast = resolve;
    });
    const onCommit = vi
      .fn<(nextOptions: ReadonlyArray<ProviderOptionSelection> | undefined) => Promise<void>>()
      .mockImplementationOnce(() => fastAcknowledgement)
      .mockResolvedValue(undefined);
    let updater: ProviderOptionUpdater | null = null;
    const descriptors: ProviderOptionDescriptor[] = [
      { ...booleanDescriptor("fastMode", "Fast Mode"), currentValue: false },
      { ...effort, currentValue: "low" },
    ];
    await mount(<DirectUpdaterHarness onCommit={onCommit} onReady={(next) => (updater = next)} />);

    let firstCommit!: Promise<void>;
    await act(async () => {
      firstCommit = updater!.updateDescriptor(descriptors, "fastMode", true);
      await Promise.resolve();
    });
    await act(async () => {
      await updater!.updateDescriptor(descriptors, "effort", "high");
    });

    expect(onCommit).toHaveBeenCalledTimes(1);

    await act(async () => {
      acknowledgeFast();
      await firstCommit;
    });
    await act(async () => {
      await updater!.updateDescriptor(
        [
          { ...booleanDescriptor("fastMode", "Fast Mode"), currentValue: true },
          { ...effort, currentValue: "low" },
        ],
        "effort",
        "high",
      );
    });

    expect(onCommit).toHaveBeenCalledTimes(2);
    expect(onCommit).toHaveBeenLastCalledWith([
      { id: "fastMode", value: true },
      { id: "effort", value: "high" },
    ]);
  });

  it("keeps Fast unchanged and reports a normalized failure when the acknowledged update rejects", async () => {
    const onModelOptionsChange = vi.fn().mockRejectedValue(new Error("provider rejected Fast"));
    await mount(
      <ComposerTraitControls
        provider={CLAUDE}
        models={modelsWith([booleanDescriptor("fastMode", "Fast Mode")])}
        model={MODEL}
        prompt=""
        modelOptions={selections(["fastMode", false])}
        onPromptChange={vi.fn()}
        onModelOptionsChange={onModelOptionsChange}
      />,
    );

    await click(
      document.querySelector<HTMLButtonElement>('button[aria-label="Enable fast mode"]')!,
    );

    expect(onModelOptionsChange).toHaveBeenCalledTimes(1);
    expect(
      document
        .querySelector<HTMLButtonElement>('button[aria-label="Enable fast mode"]')
        ?.getAttribute("aria-pressed"),
    ).toBe("false");
    expect(testState.addToast).toHaveBeenCalledWith(
      expect.objectContaining({ description: "provider rejected Fast" }),
    );
  });

  it("writes draft option changes locally without waiting for a server commit", async () => {
    await mount(
      <ComposerTraitControls
        provider={CLAUDE}
        models={modelsWith([booleanDescriptor("fastMode", "Fast Mode")])}
        model={MODEL}
        prompt=""
        modelOptions={selections(["fastMode", false])}
        onPromptChange={vi.fn()}
        draftId={"draft-fast" as never}
      />,
    );

    await click(
      document.querySelector<HTMLButtonElement>('button[aria-label="Enable fast mode"]')!,
    );

    expect(testState.setProviderModelOptions).toHaveBeenCalledWith(
      "draft-fast",
      CLAUDE,
      expect.arrayContaining([expect.objectContaining({ id: "fastMode", value: true })]),
      expect.objectContaining({ model: MODEL, persistSticky: true }),
    );
  });

  it("reports and renders controls only for models with option descriptors", async () => {
    expect(
      shouldRenderTraitsControls({
        provider: CODEX,
        models: modelsWith([]),
        model: MODEL,
        prompt: "",
        modelOptions: [],
      }),
    ).toBe(false);
    expect(
      shouldRenderTraitsControls({
        provider: CODEX,
        models: modelsWith([effort]),
        model: MODEL,
        prompt: "",
        modelOptions: [],
      }),
    ).toBe(true);

    await mount(
      <TraitsPicker
        provider={CODEX}
        models={modelsWith([])}
        model={MODEL}
        prompt=""
        onPromptChange={vi.fn()}
        onModelOptionsChange={vi.fn()}
      />,
    );
    expect(document.body.textContent).toBe("");
  });

  it("shows current select and boolean labels and changes a select through the open menu", async () => {
    const onModelOptionsChange = vi.fn();
    const models = modelsWith([
      effort,
      selectDescriptor("contextWindow", "Context Window", [
        { id: "200k", label: "200k", isDefault: true },
        { id: "1m", label: "1M" },
      ]),
      booleanDescriptor("fastMode", "Fast Mode"),
      booleanDescriptor("thinking", "Thinking"),
    ]);
    await mount(
      <TraitsPicker
        provider={CODEX}
        models={models}
        model={MODEL}
        prompt=""
        modelOptions={selections(["effort", "high"], ["fastMode", false], ["thinking", true])}
        onPromptChange={vi.fn()}
        onModelOptionsChange={onModelOptionsChange}
        triggerClassName="test-trigger"
        triggerVariant="outline"
      />,
    );

    const trigger = buttonContaining("High");
    expect(trigger.textContent).toContain("200k");
    expect(trigger.textContent).toContain("Normal");
    expect(trigger.textContent).toContain("Thinking On");
    expect(trigger.className).toContain("test-trigger");

    await click(trigger);
    await click(radioItem("Low"));
    expect(onModelOptionsChange).toHaveBeenCalledWith(
      expect.arrayContaining([expect.objectContaining({ id: "effort", value: "low" })]),
    );
  });

  it("renders dedicated Fast and effort controls without exposing agent selection", async () => {
    const onModelOptionsChange = vi.fn();
    await mount(
      <ComposerTraitControls
        provider={CLAUDE}
        models={modelsWith([
          effort,
          booleanDescriptor("fastMode", "Fast Mode"),
          selectDescriptor("agent", "Agent", [
            { id: "reviewer", label: "reviewer", isDefault: true },
          ]),
        ])}
        model={MODEL}
        prompt=""
        modelOptions={selections(["effort", "high"], ["fastMode", true], ["agent", "reviewer"])}
        onPromptChange={vi.fn()}
        onModelOptionsChange={onModelOptionsChange}
      />,
    );

    const fastButton = document.querySelector<HTMLButtonElement>(
      'button[aria-label="Disable fast mode"]',
    );
    const effortButton = document.querySelector<HTMLButtonElement>(
      'button[aria-label="Reasoning effort: High"]',
    );
    expect(fastButton?.getAttribute("aria-pressed")).toBe("true");
    expect(fastButton?.className).not.toContain("bg-primary");
    expect(fastButton?.className).toContain("bg-foreground/10");
    expect(fastButton?.className).toContain("dark:bg-foreground/14");
    expect(fastButton?.className).toContain("text-foreground");
    expect(effortButton).not.toBeNull();
    expect(effortButton?.getAttribute("aria-pressed")).toBeNull();
    expect(effortButton?.className).not.toContain("bg-primary");
    expect(effortButton?.className).toContain("text-foreground/80");
    expect(document.body.textContent).not.toContain("High");
    expect(document.body.textContent).not.toContain("Agent");
    expect(document.body.textContent).not.toContain("reviewer");

    await click(effortButton!);
    expect(radioItem("Ultrathink")).not.toBeNull();
    await click(radioItem("Low"));
    expect(onModelOptionsChange).toHaveBeenCalledWith(
      expect.arrayContaining([expect.objectContaining({ id: "effort", value: "low" })]),
    );

    await click(fastButton!);
    expect(onModelOptionsChange).toHaveBeenCalledWith(
      expect.arrayContaining([expect.objectContaining({ id: "fastMode", value: false })]),
    );
  });

  it("renders one reasoning bar per provider effort level", async () => {
    const claudeEffort = selectDescriptor("effort", "Effort", [
      { id: "low", label: "Low" },
      { id: "medium", label: "Medium" },
      { id: "high", label: "High" },
      { id: "extra-high", label: "Extra High" },
      { id: "max", label: "Max" },
    ]);
    const codexEffort = selectDescriptor("reasoningEffort", "Reasoning", [
      { id: "low", label: "Low" },
      { id: "medium", label: "Medium" },
      { id: "high", label: "High" },
      { id: "xhigh", label: "Extra High" },
    ]);

    await mount(
      <div>
        <ComposerTraitControls
          provider={CLAUDE}
          models={modelsWith([claudeEffort])}
          model={MODEL}
          prompt=""
          modelOptions={selections(["effort", "max"])}
          onPromptChange={vi.fn()}
          onModelOptionsChange={vi.fn()}
        />
        <ComposerTraitControls
          provider={CODEX}
          models={modelsWith([codexEffort])}
          model={MODEL}
          prompt=""
          modelOptions={selections(["reasoningEffort", "xhigh"])}
          onPromptChange={vi.fn()}
          onModelOptionsChange={vi.fn()}
        />
      </div>,
    );

    const claudeBars = document
      .querySelector<HTMLButtonElement>('button[aria-label="Reasoning effort: Max"]')
      ?.querySelector('[aria-hidden="true"]')?.children;
    const codexBars = document
      .querySelector<HTMLButtonElement>('button[aria-label="Reasoning effort: Extra High"]')
      ?.querySelector('[aria-hidden="true"]')?.children;
    expect(claudeBars).toHaveLength(5);
    expect(codexBars).toHaveLength(4);
  });

  it("emphasizes only each provider's highest reasoning value in the toolbar and menu title", async () => {
    const ultraDescriptor = selectDescriptor("reasoningEffort", "Reasoning", [
      { id: "high", label: "High" },
      { id: "max", label: "Max" },
      { id: "ultra", label: "Ultra" },
    ]);
    const maxDescriptor = selectDescriptor("effort", "Reasoning", [
      { id: "high", label: "High" },
      { id: "max", label: "Max" },
    ]);

    await mount(
      <div>
        <ComposerTraitControls
          provider={CODEX}
          models={modelsWith([ultraDescriptor])}
          model={MODEL}
          prompt=""
          modelOptions={selections(["reasoningEffort", "ultra"])}
          onPromptChange={vi.fn()}
          onModelOptionsChange={vi.fn()}
        />
        <ComposerTraitControls
          provider={CLAUDE}
          models={modelsWith([maxDescriptor])}
          model={MODEL}
          prompt=""
          modelOptions={selections(["effort", "max"])}
          onPromptChange={vi.fn()}
          onModelOptionsChange={vi.fn()}
        />
        <ComposerTraitControls
          provider={CLAUDE}
          models={modelsWith([maxDescriptor])}
          model={MODEL}
          prompt=""
          modelOptions={selections(["effort", "high"])}
          onPromptChange={vi.fn()}
          onModelOptionsChange={vi.fn()}
        />
      </div>,
    );

    const ultraButton = document.querySelector<HTMLButtonElement>(
      'button[aria-label="Reasoning effort: Ultra"]',
    )!;
    const maxButton = document.querySelector<HTMLButtonElement>(
      'button[aria-label="Reasoning effort: Max"]',
    )!;
    const highButton = document.querySelector<HTMLButtonElement>(
      'button[aria-label="Reasoning effort: High"]',
    )!;
    expect(ultraButton.className).toContain("text-destructive");
    expect(maxButton.className).toContain("text-destructive");
    expect(highButton.className).not.toContain("text-destructive");

    await click(ultraButton);
    const ultraItem = radioItem("Ultra");
    const ultraTitle = ultraItem.querySelector<HTMLElement>("[data-effort-title]");
    expect(ultraTitle?.className).toContain("text-destructive");
    expect(ultraItem.className).not.toContain("text-destructive");
  });

  it("keeps unavailable Fast and effort focusable with their reason", async () => {
    const reason = "Fast mode is not supported by Test Model through OpenCode.";
    await mount(
      <ComposerTraitControls
        provider={CLAUDE}
        models={modelsWith([])}
        model={MODEL}
        prompt=""
        onPromptChange={vi.fn()}
        fastAvailability={{ state: "unsupported", reason }}
        effortAvailability={{
          state: "unknown",
          reason: "Reasoning effort availability is still loading.",
        }}
      />,
    );

    const fast = document.querySelector<HTMLButtonElement>(`button[aria-label="${reason}"]`)!;
    const effortControl = document.querySelector<HTMLButtonElement>(
      'button[aria-label="Reasoning effort availability is still loading."]',
    )!;
    expect(fast.disabled).toBe(false);
    expect(fast.getAttribute("aria-disabled")).toBe("true");
    fast.focus();
    expect(document.activeElement).toBe(fast);
    expect(effortControl.getAttribute("aria-disabled")).toBe("true");
  });

  it("uses Codex priority service tier for the dedicated Fast toggle", async () => {
    const onModelOptionsChange = vi.fn();
    const models = modelsWith([
      selectDescriptor("reasoningEffort", "Reasoning", [
        { id: "medium", label: "Medium", isDefault: true },
      ]),
      selectDescriptor("serviceTier", "Service Tier", [
        { id: "default", label: "Standard", isDefault: true },
        { id: "priority", label: "Fast" },
      ]),
    ]);
    const mounted = await mount(
      <ComposerTraitControls
        provider={CODEX}
        models={models}
        model={MODEL}
        prompt=""
        modelOptions={selections(["reasoningEffort", "medium"], ["serviceTier", "default"])}
        onPromptChange={vi.fn()}
        onModelOptionsChange={onModelOptionsChange}
      />,
    );

    const disabledFastButton = document.querySelector<HTMLButtonElement>(
      'button[aria-label="Enable fast mode"]',
    );
    expect(disabledFastButton?.getAttribute("aria-pressed")).toBe("false");
    await click(disabledFastButton!);
    expect(onModelOptionsChange).toHaveBeenCalledWith(
      expect.arrayContaining([expect.objectContaining({ id: "serviceTier", value: "priority" })]),
    );

    await act(async () =>
      mounted.root.render(
        <ComposerTraitControls
          provider={CODEX}
          models={models}
          model={MODEL}
          prompt=""
          modelOptions={selections(["reasoningEffort", "medium"], ["serviceTier", "priority"])}
          onPromptChange={vi.fn()}
          onModelOptionsChange={onModelOptionsChange}
        />,
      ),
    );

    const enabledFastButton = document.querySelector<HTMLButtonElement>(
      'button[aria-label="Disable fast mode"]',
    );
    expect(enabledFastButton?.getAttribute("aria-pressed")).toBe("true");
    await click(enabledFastButton!);
    expect(onModelOptionsChange).toHaveBeenCalledWith(
      expect.arrayContaining([expect.objectContaining({ id: "serviceTier", value: "default" })]),
    );
  });

  it("shows tooltips for supported Fast and reasoning controls", async () => {
    await mount(
      <TooltipProvider delay={0}>
        <ComposerTraitControls
          provider={CLAUDE}
          models={modelsWith([effort, booleanDescriptor("fastMode", "Fast Mode")])}
          model={MODEL}
          prompt=""
          modelOptions={selections(["effort", "high"], ["fastMode", false])}
          onPromptChange={vi.fn()}
          onModelOptionsChange={vi.fn()}
        />
      </TooltipProvider>,
    );

    const fast = document.querySelector<HTMLButtonElement>(
      'button[aria-label="Enable fast mode"]',
    )!;
    await act(async () => {
      fast.focus();
      await Promise.resolve();
    });
    expect(document.body.querySelector('[data-slot="tooltip-popup"]')?.textContent).toContain(
      "Enable fast mode",
    );

    fast.blur();
    const effortControl = document.querySelector<HTMLButtonElement>(
      'button[aria-label="Reasoning effort: High"]',
    )!;
    await act(async () => {
      effortControl.focus();
      await Promise.resolve();
    });
    expect(
      [...document.body.querySelectorAll('[data-slot="tooltip-popup"]')].some((popup) =>
        popup.textContent?.includes("Reasoning effort: High"),
      ),
    ).toBe(true);
  });

  it("turns Codex Fast off with the advertised non-default tier", async () => {
    const onModelOptionsChange = vi.fn();
    await mount(
      <ComposerTraitControls
        provider={CODEX}
        models={modelsWith([
          selectDescriptor("serviceTier", "Service Tier", [
            { id: "fast", label: "Fast" },
            { id: "flex", label: "Flex" },
          ]),
        ])}
        model={MODEL}
        prompt=""
        modelOptions={selections(["serviceTier", "fast"])}
        onPromptChange={vi.fn()}
        onModelOptionsChange={onModelOptionsChange}
      />,
    );

    await click(
      document.querySelector<HTMLButtonElement>('button[aria-label="Disable fast mode"]')!,
    );
    expect(onModelOptionsChange).toHaveBeenCalledWith([{ id: "serviceTier", value: "flex" }]);
  });

  it("keeps one-way Codex Fast visible but disabled", async () => {
    const onModelOptionsChange = vi.fn();
    await mount(
      <ComposerTraitControls
        provider={CODEX}
        models={modelsWith([
          selectDescriptor("serviceTier", "Service Tier", [{ id: "fast", label: "Fast" }]),
        ])}
        model={MODEL}
        prompt=""
        modelOptions={selections(["serviceTier", "fast"])}
        onPromptChange={vi.fn()}
        onModelOptionsChange={onModelOptionsChange}
      />,
    );

    const fast = buttonContaining("Fast");
    expect(fast.getAttribute("aria-disabled")).toBe("true");
    await click(fast);
    expect(onModelOptionsChange).not.toHaveBeenCalled();
  });

  it("does not render composer effort controls for unrelated select descriptors", async () => {
    await mount(
      <ComposerTraitControls
        provider={CLAUDE}
        models={modelsWith([
          selectDescriptor("temperature", "Temperature", [
            { id: "warm", label: "Warm", isDefault: true },
          ]),
        ])}
        model={MODEL}
        prompt=""
        onPromptChange={vi.fn()}
        onModelOptionsChange={vi.fn()}
      />,
    );

    expect(document.querySelector('button[aria-label^="Reasoning effort:"]')).toBeNull();
  });

  it("replaces composer controls when the provider instance and model change", async () => {
    const mounted = await mount(
      <ComposerTraitControls
        provider={CLAUDE}
        instanceId={ProviderInstanceId.make("first")}
        models={modelsWith([effort, booleanDescriptor("fastMode", "Fast Mode")])}
        model={MODEL}
        prompt=""
        modelOptions={selections(["effort", "high"], ["fastMode", true])}
        onPromptChange={vi.fn()}
        onModelOptionsChange={vi.fn()}
      />,
    );

    await act(async () =>
      mounted.root.render(
        <ComposerTraitControls
          provider={CLAUDE}
          instanceId={ProviderInstanceId.make("second")}
          models={[
            {
              slug: "other-model",
              name: "Other Model",
              isCustom: false,
              capabilities: {
                optionDescriptors: [
                  selectDescriptor("effort", "Effort", [
                    { id: "low", label: "Low", isDefault: true },
                  ]),
                  booleanDescriptor("fastMode", "Fast Mode"),
                  selectDescriptor("agent", "Agent", [
                    { id: "reviewer", label: "reviewer", isDefault: true },
                  ]),
                ],
              },
            },
          ]}
          model="other-model"
          prompt=""
          modelOptions={selections(["effort", "low"], ["fastMode", false], ["agent", "reviewer"])}
          onPromptChange={vi.fn()}
          onModelOptionsChange={vi.fn()}
        />,
      ),
    );

    expect(document.querySelector('button[aria-label="Disable fast mode"]')).toBeNull();
    expect(document.querySelector('button[aria-label="Enable fast mode"]')).not.toBeNull();
    expect(document.querySelector('button[aria-label="Reasoning effort: Low"]')).not.toBeNull();
    expect(document.body.textContent).not.toContain("High");
    expect(document.body.textContent).not.toContain("reviewer");
  });

  it("does not fabricate Codex Fast through partial and empty capability snapshots", async () => {
    const partialModels = modelsWith([
      selectDescriptor("reasoningEffort", "Reasoning", [
        { id: "medium", label: "Medium", isDefault: true },
        { id: "high", label: "High" },
      ]),
      selectDescriptor("serviceTier", "Service Tier", [
        { id: "default", label: "Standard", isDefault: true },
      ]),
    ]);
    const modelOptions = selections(["reasoningEffort", "high"], ["serviceTier", "fast"]);
    expect(
      shouldRenderTraitsControls({
        provider: CODEX,
        models: modelsWith([]),
        model: MODEL,
        prompt: "",
        modelOptions: selections(["serviceTier", "fast"]),
      }),
    ).toBe(false);

    await mount(
      <TraitsPicker
        provider={CODEX}
        models={partialModels}
        model={MODEL}
        prompt=""
        modelOptions={modelOptions}
        onPromptChange={vi.fn()}
        onModelOptionsChange={vi.fn()}
      />,
    );

    const trigger = buttonContaining("High");
    expect(trigger.textContent).not.toContain("Fast");
    await click(trigger);
    expect(radioItem("Standard")).toBeDefined();
    expect(document.body.textContent).not.toContain("Fast");
  });

  it("injects ultrathink into an empty prompt from the rendered option", async () => {
    const onPromptChange = vi.fn();
    await mount(
      <TraitsPicker
        provider={CLAUDE}
        models={modelsWith([effort])}
        model={MODEL}
        prompt="   "
        onPromptChange={onPromptChange}
        onModelOptionsChange={vi.fn()}
      />,
    );

    await click(buttonContaining("High"));
    await click(radioItem("Ultrathink"));
    expect(onPromptChange).toHaveBeenCalledWith("Ultrathink:\n");
  });

  it("shows a raw prompt-injected session default as the selected effort", async () => {
    await mount(
      <TraitsPicker
        provider={CLAUDE}
        models={modelsWith([effort])}
        model={MODEL}
        prompt=""
        modelOptions={selections(["effort", "ultrathink"])}
        onPromptChange={vi.fn()}
        onModelOptionsChange={vi.fn()}
      />,
    );

    const trigger = buttonContaining("Ultrathink");
    await click(trigger);
    expect(radioItem("Ultrathink").getAttribute("aria-checked")).toBe("true");
  });

  it("replaces a raw prompt-injected session default with a native effort", async () => {
    const onPromptChange = vi.fn();
    const onModelOptionsChange = vi.fn();
    await mount(
      <TraitsPicker
        provider={CLAUDE}
        models={modelsWith([effort])}
        model={MODEL}
        prompt=""
        modelOptions={selections(["effort", "ultrathink"])}
        onPromptChange={onPromptChange}
        onModelOptionsChange={onModelOptionsChange}
      />,
    );

    const trigger = document.querySelector<HTMLButtonElement>("button");
    expect(trigger).not.toBeNull();
    expect(trigger?.textContent).toContain("Ultrathink");
    await click(trigger!);
    await click(radioItem("High"));

    expect(onPromptChange).not.toHaveBeenCalled();
    expect(onModelOptionsChange).toHaveBeenCalledWith(
      expect.arrayContaining([expect.objectContaining({ id: "effort", value: "high" })]),
    );
  });

  it("removes the generated ultrathink prefix before applying another effort", async () => {
    const onPromptChange = vi.fn();
    const onModelOptionsChange = vi.fn();
    await mount(
      <TraitsPicker
        provider={CLAUDE}
        models={modelsWith([effort])}
        model={MODEL}
        prompt={"Ultrathink:\nImplement this"}
        onPromptChange={onPromptChange}
        onModelOptionsChange={onModelOptionsChange}
      />,
    );

    await click(buttonContaining("Ultrathink"));
    await click(radioItem("Low"));
    expect(onPromptChange).toHaveBeenCalledWith("Implement this");
    expect(onModelOptionsChange).toHaveBeenCalledWith(
      expect.arrayContaining([expect.objectContaining({ id: "effort", value: "low" })]),
    );
  });

  it("locks effort controls when ultrathink is part of the prompt body", async () => {
    const onPromptChange = vi.fn();
    const onModelOptionsChange = vi.fn();
    await mount(
      <TraitsPicker
        provider={CLAUDE}
        models={modelsWith([effort])}
        model={MODEL}
        prompt="Please ultrathink about this"
        onPromptChange={onPromptChange}
        onModelOptionsChange={onModelOptionsChange}
      />,
    );

    await click(buttonContaining("Ultrathink"));
    expect(document.body.textContent).toContain("Remove it to change this option.");
    expect(radioItem("Low").getAttribute("aria-disabled")).toBe("true");
    await click(radioItem("Low"));
    expect(onPromptChange).not.toHaveBeenCalled();
    expect(onModelOptionsChange).not.toHaveBeenCalled();
  });

  it("disables prompt injection when requested and persists boolean changes to a draft", async () => {
    const onPromptChange = vi.fn();
    const draftId = "draft-traits" as never;
    await mount(
      <TraitsPicker
        provider={CLAUDE}
        instanceId={ProviderInstanceId.make("claude_work")}
        models={modelsWith([effort, booleanDescriptor("thinking", "Thinking")])}
        model={MODEL}
        prompt="Ultrathink: body"
        allowPromptInjectedEffort={false}
        modelOptions={selections(["effort", "high"], ["thinking", false])}
        onPromptChange={onPromptChange}
        draftId={draftId}
      />,
    );

    await click(buttonContaining("High"));
    await click(radioItem("On"));
    expect(onPromptChange).not.toHaveBeenCalled();
    expect(testState.setProviderModelOptions).toHaveBeenCalledWith(
      draftId,
      CLAUDE,
      expect.arrayContaining([expect.objectContaining({ id: "thinking", value: true })]),
      {
        instanceId: ProviderInstanceId.make("claude_work"),
        model: MODEL,
        persistSticky: true,
      },
    );
  });

  it("keeps rendered controls usable when no persistence target is supplied", async () => {
    const mounted = await mount(<NoPersistenceHarness />);

    expect(buttonContaining("High").textContent).toContain("Thinking Off");
    await click(buttonContaining("High"));
    await click(radioItem("Ultrathink"));
    expect(document.querySelector('[data-testid="prompt"]')?.textContent).toBe("Ultrathink:\n");
    expect(buttonContaining("Ultrathink").textContent).toContain("Thinking Off");
    expect(testState.setProviderModelOptions).not.toHaveBeenCalled();

    await act(async () => mounted.root.unmount());
    mountedTrees.splice(mountedTrees.indexOf(mounted), 1);
    mounted.container.remove();
    await mount(
      <TraitsPicker
        provider={CLAUDE}
        models={modelsWith([booleanDescriptor("thinking", "Thinking")])}
        model={MODEL}
        prompt=""
        onPromptChange={vi.fn()}
      />,
    );
    await click(buttonContaining("Thinking Off"));
    await click(radioItem("On"));
    expect(testState.setProviderModelOptions).not.toHaveBeenCalled();
    expect(document.activeElement?.textContent).toContain("On");
  });
});
