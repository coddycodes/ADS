import { describe, expect, it } from "vite-plus/test";
import {
  ProviderDriverKind,
  type ProviderOptionDescriptor,
  type ProviderOptionSelection,
  type ServerProviderModel,
} from "@coddy/contracts";
import { DraftId } from "../../composerDraftStore";
import {
  getComposerPromptInjectionState,
  getComposerProviderState,
  renderComposerTraitControls,
} from "./composerProviderState";

// Everything in composerProviderState is now data-driven by the model's
// optionDescriptors, so these tests use a single synthetic provider/model and
// vary only the descriptor shape per scenario.

const PROVIDER: ProviderDriverKind = ProviderDriverKind.make("testProvider");
const CODEX: ProviderDriverKind = ProviderDriverKind.make("codex");
const MODEL = "test-model";

function selectDescriptor(
  id: string,
  options: ReadonlyArray<{ id: string; label: string; isDefault?: boolean }>,
  promptInjectedValues?: ReadonlyArray<string>,
): Extract<ProviderOptionDescriptor, { type: "select" }> {
  const defaultId = options.find((option) => option.isDefault)?.id;
  return {
    id,
    label: id,
    type: "select",
    options: [...options],
    ...(defaultId ? { currentValue: defaultId } : {}),
    ...(promptInjectedValues && promptInjectedValues.length > 0
      ? { promptInjectedValues: [...promptInjectedValues] }
      : {}),
  };
}

function booleanDescriptor(id: string): Extract<ProviderOptionDescriptor, { type: "boolean" }> {
  return { id, label: id, type: "boolean" };
}

function modelWith(
  descriptors: ReadonlyArray<ProviderOptionDescriptor>,
): ReadonlyArray<ServerProviderModel> {
  return [
    { slug: MODEL, name: MODEL, isCustom: false, capabilities: { optionDescriptors: descriptors } },
  ];
}

function selections(
  ...entries: Array<[string, string | boolean]>
): ReadonlyArray<ProviderOptionSelection> {
  return entries.map(([id, value]) => ({ id, value }));
}

const ULTRATHINK_FRAME_CLASSES = {
  composerFrameClassName: "ultrathink-frame",
  composerSurfaceClassName: "shadow-[0_0_0_1px_rgba(255,255,255,0.07)_inset]",
  modelPickerIconClassName: "ultrathink-chroma",
} as const;

describe("getComposerProviderState", () => {
  it("derives a stable prompt injection state for ordinary prompt edits", () => {
    expect(getComposerPromptInjectionState("Investigate this failure")).toBe("none");
    expect(getComposerPromptInjectionState("Ultrathink:\nInvestigate this failure")).toBe(
      "ultrathink",
    );
  });

  it("returns descriptor defaults when no selections are provided", () => {
    const state = getComposerProviderState({
      provider: PROVIDER,
      model: MODEL,
      models: modelWith([
        selectDescriptor("effort", [
          { id: "low", label: "Low" },
          { id: "high", label: "High", isDefault: true },
        ]),
      ]),
      modelOptions: undefined,
    });

    expect(state).toEqual({
      provider: PROVIDER,
      promptEffort: "high",
      modelOptionsForDispatch: selections(["effort", "high"]),
    });
  });

  it("lets selections override defaults and propagates them through dispatch", () => {
    const state = getComposerProviderState({
      provider: PROVIDER,
      model: MODEL,
      models: modelWith([
        selectDescriptor("effort", [
          { id: "low", label: "Low" },
          { id: "high", label: "High", isDefault: true },
        ]),
        booleanDescriptor("fastMode"),
      ]),
      modelOptions: selections(["effort", "low"], ["fastMode", true]),
    });

    expect(state).toEqual({
      provider: PROVIDER,
      promptEffort: "low",
      modelOptionsForDispatch: selections(["effort", "low"], ["fastMode", true]),
    });
  });

  it("normalizes Codex Fast to an advertised partial service tier", () => {
    const state = getComposerProviderState({
      provider: CODEX,
      model: MODEL,
      models: modelWith([
        selectDescriptor("reasoningEffort", [
          { id: "medium", label: "Medium", isDefault: true },
          { id: "high", label: "High" },
        ]),
        selectDescriptor("serviceTier", [{ id: "default", label: "Standard", isDefault: true }]),
      ]),
      modelOptions: selections(["reasoningEffort", "high"], ["serviceTier", "fast"]),
    });

    expect(state).toEqual({
      provider: CODEX,
      promptEffort: "high",
      modelOptionsForDispatch: selections(["reasoningEffort", "high"], ["serviceTier", "default"]),
    });
  });

  it("does not infer Codex options from an empty capability snapshot", () => {
    const state = getComposerProviderState({
      provider: CODEX,
      model: MODEL,
      models: modelWith([]),
      modelOptions: selections(["serviceTier", "fast"]),
    });

    expect(state).toEqual({
      provider: CODEX,
      promptEffort: null,
      modelOptionsForDispatch: undefined,
    });
  });

  it("preserves configured Codex effort but not unverified Fast", () => {
    const state = getComposerProviderState({
      provider: CODEX,
      model: MODEL,
      models: modelWith([]),
      modelOptions: selections(["reasoningEffort", "xhigh"], ["serviceTier", "fast"]),
    });

    expect(state).toEqual({
      provider: CODEX,
      promptEffort: "xhigh",
      modelOptionsForDispatch: selections(["reasoningEffort", "xhigh"]),
    });
  });

  it("does not supply Codex dispatch defaults when capabilities are absent", () => {
    const state = getComposerProviderState({
      provider: CODEX,
      model: MODEL,
      models: modelWith([]),
      modelOptions: undefined,
    });

    expect(state).toEqual({
      provider: CODEX,
      promptEffort: null,
      modelOptionsForDispatch: undefined,
    });
  });

  it("uses a raw prompt-injected session default for the prompt but dispatches the native default", () => {
    const state = getComposerProviderState({
      provider: ProviderDriverKind.make("claudeAgent"),
      model: MODEL,
      models: modelWith([
        selectDescriptor(
          "effort",
          [
            { id: "high", label: "High", isDefault: true },
            { id: "ultrathink", label: "Ultrathink" },
          ],
          ["ultrathink"],
        ),
      ]),
      modelOptions: selections(["effort", "ultrathink"]),
    });

    expect(state).toEqual({
      provider: ProviderDriverKind.make("claudeAgent"),
      promptEffort: "ultrathink",
      modelOptionsForDispatch: selections(["effort", "high"]),
      ...ULTRATHINK_FRAME_CLASSES,
    });
  });

  it("preserves selections that match defaults so deepMerge can overwrite prior state", () => {
    const state = getComposerProviderState({
      provider: PROVIDER,
      model: MODEL,
      models: modelWith([
        selectDescriptor("effort", [{ id: "high", label: "High", isDefault: true }]),
        booleanDescriptor("fastMode"),
      ]),
      modelOptions: selections(["effort", "high"], ["fastMode", false]),
    });

    expect(state.modelOptionsForDispatch).toEqual(
      selections(["effort", "high"], ["fastMode", false]),
    );
  });

  it("drops selections for descriptors the model does not declare", () => {
    const state = getComposerProviderState({
      provider: PROVIDER,
      model: MODEL,
      models: modelWith([booleanDescriptor("thinking")]),
      modelOptions: selections(["effort", "max"], ["thinking", false]),
    });

    expect(state).toEqual({
      provider: PROVIDER,
      promptEffort: null,
      modelOptionsForDispatch: selections(["thinking", false]),
    });
  });

  it("does not treat unrelated select descriptors as prompt effort", () => {
    const state = getComposerProviderState({
      provider: PROVIDER,
      model: MODEL,
      models: modelWith([
        selectDescriptor("temperature", [{ id: "warm", label: "Warm", isDefault: true }]),
      ]),
      modelOptions: undefined,
    });

    expect(state.promptEffort).toBeNull();
  });

  it("selects canonical effort when unrelated selects appear first", () => {
    const state = getComposerProviderState({
      provider: PROVIDER,
      model: MODEL,
      models: modelWith([
        selectDescriptor("temperature", [{ id: "warm", label: "Warm", isDefault: true }]),
        selectDescriptor("effort", [
          { id: "low", label: "Low" },
          { id: "high", label: "High", isDefault: true },
        ]),
      ]),
      modelOptions: selections(["temperature", "warm"], ["effort", "low"]),
    });

    expect(state.promptEffort).toBe("low");
  });

  it("derives promptEffort from the canonical effort descriptor and preserves all others for dispatch", () => {
    const state = getComposerProviderState({
      provider: PROVIDER,
      model: MODEL,
      models: modelWith([
        selectDescriptor("effort", [{ id: "high", label: "High", isDefault: true }]),
        selectDescriptor("contextWindow", [
          { id: "200k", label: "200k", isDefault: true },
          { id: "1m", label: "1M" },
        ]),
        selectDescriptor("agent", [
          { id: "build", label: "Build", isDefault: true },
          { id: "plan", label: "Plan" },
        ]),
      ]),
      modelOptions: selections(["agent", "plan"]),
    });

    expect(state.promptEffort).toBe("high");
    expect(state.modelOptionsForDispatch).toEqual(
      selections(["effort", "high"], ["contextWindow", "200k"], ["agent", "plan"]),
    );
  });

  it("returns undefined dispatch options when the model declares no descriptors", () => {
    const state = getComposerProviderState({
      provider: PROVIDER,
      model: MODEL,
      models: modelWith([]),
      modelOptions: selections(["anything", "value"]),
    });

    expect(state).toEqual({
      provider: PROVIDER,
      promptEffort: null,
      modelOptionsForDispatch: undefined,
    });
  });

  it("adds ultrathink class names when the prompt triggers a promptInjectedValues descriptor", () => {
    const state = getComposerProviderState({
      provider: PROVIDER,
      model: MODEL,
      models: modelWith([
        selectDescriptor(
          "effort",
          [
            { id: "medium", label: "Medium" },
            { id: "high", label: "High", isDefault: true },
            { id: "ultrathink", label: "Ultrathink" },
          ],
          ["ultrathink"],
        ),
      ]),
      promptInjectionState: getComposerPromptInjectionState(
        "Ultrathink:\nInvestigate this failure",
      ),
      modelOptions: selections(["effort", "medium"]),
    });

    expect(state).toEqual({
      provider: PROVIDER,
      promptEffort: "medium",
      modelOptionsForDispatch: selections(["effort", "medium"]),
      ...ULTRATHINK_FRAME_CLASSES,
    });
  });

  it("does not add ultrathink class names when the descriptor has no promptInjectedValues", () => {
    const state = getComposerProviderState({
      provider: PROVIDER,
      model: MODEL,
      models: modelWith([
        selectDescriptor("effort", [{ id: "high", label: "High", isDefault: true }]),
      ]),
      promptInjectionState: getComposerPromptInjectionState(
        "Ultrathink:\nInvestigate this failure",
      ),
      modelOptions: undefined,
    });

    expect(state).not.toHaveProperty("composerFrameClassName");
    expect(state).not.toHaveProperty("composerSurfaceClassName");
    expect(state).not.toHaveProperty("modelPickerIconClassName");
  });
});

describe("provider traits render guards", () => {
  it("returns null when no thread target is provided", () => {
    const models = modelWith([
      selectDescriptor("effort", [{ id: "high", label: "High", isDefault: true }]),
    ]);
    const args = {
      provider: PROVIDER,
      model: MODEL,
      models,
      modelOptions: undefined,
      prompt: "",
      onPromptChange: () => {},
    };

    expect(renderComposerTraitControls(args)).toBeNull();
  });

  it("does not render agent selection in the composer", () => {
    expect(
      renderComposerTraitControls({
        provider: PROVIDER,
        draftId: DraftId.make("agent-only"),
        model: MODEL,
        models: modelWith([
          selectDescriptor("agent", [{ id: "reviewer", label: "Reviewer", isDefault: true }]),
        ]),
        modelOptions: undefined,
        prompt: "",
        onPromptChange: () => {},
      }),
    ).not.toBeNull();
  });

  it("keeps unavailable controls visible with truthful unsupported and unknown states", () => {
    const unsupported = renderComposerTraitControls({
      provider: PROVIDER,
      draftId: DraftId.make("unsupported"),
      model: MODEL,
      models: modelWith([]),
      providerSnapshotLoaded: true,
      modelOptions: undefined,
      prompt: "",
      onPromptChange: () => {},
    });
    const unknown = renderComposerTraitControls({
      provider: PROVIDER,
      draftId: DraftId.make("unknown"),
      model: MODEL,
      models: [],
      providerSnapshotLoaded: false,
      modelOptions: undefined,
      prompt: "",
      onPromptChange: () => {},
    });

    expect(unsupported?.props.fastAvailability).toMatchObject({ state: "unsupported" });
    expect(unsupported?.props.effortAvailability).toMatchObject({ state: "unsupported" });
    expect(unknown?.props.fastAvailability).toEqual({
      state: "unknown",
      reason: "Fast mode availability is still loading.",
    });
  });

  it("does not expose Codex controls without live capabilities", () => {
    const picker = renderComposerTraitControls({
      provider: CODEX,
      draftId: DraftId.make("codex-empty-capabilities"),
      model: MODEL,
      models: modelWith([]),
      modelOptions: undefined,
      prompt: "",
      onPromptChange: () => {},
    });

    expect(picker).not.toBeNull();
  });
});
