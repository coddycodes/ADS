import {
  ProviderDriverKind,
  ProviderInstanceId,
  type ResolvedKeybindingsConfig,
  type ServerProvider,
} from "@coddy/contracts";
import type { Dispatch, SetStateAction } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import type { ProviderInstanceEntry } from "../../providerInstances";
import type { ModelEsque } from "./providerIconUtils";

type EffectCallback = () => void | (() => void);

const hooks = vi.hoisted(() => {
  let cursor = 0;
  let stateSlots = new Map<number, unknown>();
  let refSlots = new Map<number, { current: unknown }>();
  let cacheSlots = new Map<number, unknown[]>();

  return {
    effects: [] as EffectCallback[],
    beginRender() {
      cursor = 0;
      this.effects = [];
    },
    reset() {
      cursor = 0;
      stateSlots = new Map();
      refSlots = new Map();
      cacheSlots = new Map();
      this.effects = [];
    },
    runEffects(): Array<() => void> {
      const cleanups: Array<() => void> = [];
      for (const effect of this.effects) {
        const cleanup = effect();
        if (typeof cleanup === "function") {
          cleanups.push(cleanup);
        }
      }
      return cleanups;
    },
    useCallback<T>(callback: T): T {
      cursor += 1;
      return callback;
    },
    useMemo<T>(factory: () => T): T {
      cursor += 1;
      return factory();
    },
    useMemoCache(size: number): unknown[] {
      const index = cursor;
      cursor += 1;
      const existing = cacheSlots.get(index);
      if (existing) {
        return existing;
      }
      const slots = Array.from({ length: size }, () => Symbol.for("react.memo_cache_sentinel"));
      cacheSlots.set(index, slots);
      return slots;
    },
    useRef<T>(initialValue: T): { current: T } {
      const index = cursor;
      cursor += 1;
      const existing = refSlots.get(index);
      if (existing) {
        return existing as { current: T };
      }
      const ref = { current: initialValue };
      refSlots.set(index, ref);
      return ref;
    },
    useState<T>(initialValue: T | (() => T)): [T, Dispatch<SetStateAction<T>>] {
      const index = cursor;
      cursor += 1;
      if (!stateSlots.has(index)) {
        stateSlots.set(
          index,
          typeof initialValue === "function" ? (initialValue as () => T)() : initialValue,
        );
      }
      const setValue: Dispatch<SetStateAction<T>> = (nextValue) => {
        const previous = stateSlots.get(index) as T;
        stateSlots.set(
          index,
          typeof nextValue === "function" ? (nextValue as (value: T) => T)(previous) : nextValue,
        );
      };
      return [stateSlots.get(index) as T, setValue];
    },
    useEffect(effect: EffectCallback): void {
      cursor += 1;
      this.effects.push(effect);
    },
    useLayoutEffect(effect: EffectCallback): void {
      cursor += 1;
      this.effects.push(effect);
    },
  };
});

const testState = vi.hoisted(() => ({
  favorites: [] as Array<{ provider: string; model: string }>,
  updateSettings: vi.fn(),
  scrollElement: null as unknown,
  inputHandle: { focus: vi.fn() },
  listHandle: {
    getScrollableNode: (): unknown => testState.scrollElement,
    scrollIndexIntoView: vi.fn(),
    scrollToOffset: vi.fn(),
  },
}));

interface CapturedRowProps {
  index: number;
  model: ModelEsque;
  instanceId: string;
  driverKind: string;
  providerDisplayName: string;
  providerAccentColor?: string | undefined;
  isFavorite: boolean;
  isSelected: boolean;
  preferShortName?: boolean;
  showNewBadge?: boolean;
  jumpLabel?: string | null;
  disabledReason?: string | null;
  onToggleFavorite: () => void;
}

interface CapturedComboboxProps {
  items: string[];
  filteredItems: string[];
  value: string;
  onItemHighlighted: (value: unknown, eventDetails: { reason: string; index: number }) => void;
  onValueChange: (value: unknown) => void;
}

interface FakeKeyboardEvent {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  defaultPrevented: boolean;
  repeat: boolean;
  preventDefault: () => void;
  stopPropagation: () => void;
  preventBaseUIHandler?: () => void;
  target?: { value: string };
}

interface CapturedInputProps {
  value: string;
  onChange: (event: { target: { value: string } }) => void;
  onKeyDown: (event: FakeKeyboardEvent) => void;
  onMouseDown: (event: { stopPropagation: () => void }) => void;
  onTouchStart: (event: { stopPropagation: () => void }) => void;
}

interface CapturedLegendProps {
  data: string[];
  keyExtractor: (item: string) => string;
  renderItem: (args: { item: string; index: number }) => unknown;
  estimatedItemSize: number;
  initialScrollIndex?: number;
  drawDistance: number;
  recycleItems: boolean;
  onLayout: () => void;
  onScroll: () => void;
  className: string;
}

const captured = vi.hoisted(() => ({
  rows: [] as CapturedRowProps[],
  combobox: [] as CapturedComboboxProps[],
  input: [] as CapturedInputProps[],
  legend: [] as CapturedLegendProps[],
  clear() {
    this.rows = [];
    this.combobox = [];
    this.input = [];
    this.legend = [];
  },
}));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  return {
    ...actual,
    useCallback: hooks.useCallback,
    useMemo: hooks.useMemo,
    useRef: hooks.useRef,
    useState: hooks.useState,
    useEffect: hooks.useEffect.bind(hooks),
    useLayoutEffect: hooks.useLayoutEffect.bind(hooks),
  };
});

vi.mock("react/compiler-runtime", () => ({
  c: hooks.useMemoCache,
}));

vi.mock("@legendapp/list/react", () => ({
  LegendList: (props: CapturedLegendProps & { ref?: { current: unknown } }) => {
    captured.legend.push(props);
    if (props.ref) {
      props.ref.current = testState.listHandle;
    }
    return (
      <div data-testid="legend-list" className={props.className}>
        {props.data.map((item, index) => (
          <div key={props.keyExtractor(item)}>{props.renderItem({ item, index }) as never}</div>
        ))}
      </div>
    );
  },
}));

vi.mock("./ModelListRow", () => ({
  ModelListRow: (props: CapturedRowProps) => {
    captured.rows.push(props);
    return (
      <div
        data-testid="model-row"
        data-model-key={`${props.instanceId}:${props.model.slug}`}
        data-selected={props.isSelected ? "true" : undefined}
        data-favorite={props.isFavorite ? "true" : undefined}
        data-disabled-reason={props.disabledReason ?? undefined}
        data-jump-label={props.jumpLabel ?? undefined}
        data-provider={props.providerDisplayName}
      >
        {props.model.name}
      </div>
    );
  },
}));

vi.mock("../ui/combobox", () => ({
  Combobox: (props: CapturedComboboxProps & { children?: unknown }) => {
    captured.combobox.push(props);
    return (
      <div data-testid="combobox" data-value={String(props.value)}>
        {props.children as never}
      </div>
    );
  },
  ComboboxInput: (props: CapturedInputProps & { ref?: { current: unknown } }) => {
    captured.input.push(props);
    if (props.ref) {
      props.ref.current = testState.inputHandle;
    }
    return <div data-testid="combobox-input" data-value={props.value} />;
  },
  ComboboxListVirtualized: (props: { children?: unknown; className?: string }) => (
    <div data-testid="combobox-list">{props.children as never}</div>
  ),
  ComboboxEmpty: (props: { children?: unknown }) => (
    <div data-testid="combobox-empty">{props.children as never}</div>
  ),
}));

vi.mock("../ui/tooltip", () => ({
  TooltipProvider: (props: { children?: unknown }) => <>{props.children as never}</>,
}));

vi.mock("~/hooks/useSettings", () => ({
  useClientSettings: <T,>(selector: (settings: { favorites: unknown }) => T): T =>
    selector({ favorites: testState.favorites }),
  useUpdateClientSettings: () => testState.updateSettings,
}));

import { ModelPickerContent } from "./ModelPickerContent";

class StubHTMLElement {
  scrollTop = 0;
  scrollHeight = 0;
  clientHeight = 0;
}

const windowListeners: Array<{ type: string; handler: (event: unknown) => void }> = [];
const removedListeners: Array<{ type: string }> = [];

function id(value: string): ProviderInstanceId {
  return ProviderInstanceId.make(value);
}

function driver(value: string): ProviderDriverKind {
  return ProviderDriverKind.make(value);
}

function entry(
  overrides: Omit<Partial<ProviderInstanceEntry>, "instanceId"> & { instanceId: string },
): ProviderInstanceEntry {
  return {
    instanceId: id(overrides.instanceId),
    driverKind: overrides.driverKind ?? driver("codex"),
    displayName: overrides.displayName ?? overrides.instanceId,
    accentColor: overrides.accentColor,
    continuationGroupKey: overrides.continuationGroupKey,
    enabled: overrides.enabled ?? true,
    installed: true,
    status: overrides.status ?? "ready",
    isDefault: overrides.isDefault ?? true,
    isAvailable: overrides.isAvailable ?? true,
    snapshot: {} as ServerProvider,
    models: [],
  };
}

const codexEntry = entry({ instanceId: "codex", displayName: "Codex" });
const claudeEntry = entry({
  instanceId: "claude",
  driverKind: driver("claude"),
  displayName: "Claude",
});
const geminiEntry = entry({
  instanceId: "gemini",
  driverKind: driver("gemini"),
  displayName: "Gemini",
  status: "error",
});
const disabledEntry = entry({
  instanceId: "hidden",
  driverKind: driver("amp"),
  displayName: "Hidden",
  enabled: false,
});

const codexModels: ModelEsque[] = [
  { slug: "gpt-5", name: "GPT-5", shortName: "5" },
  { slug: "gpt-5-codex", name: "GPT-5 Codex" },
];
const claudeModels: ModelEsque[] = [{ slug: "opus", name: "Claude Opus", subProvider: "Bedrock" }];

const jumpKeybindings = [
  {
    command: "modelPicker.jump.1",
    shortcut: {
      key: "1",
      metaKey: false,
      ctrlKey: true,
      shiftKey: false,
      altKey: false,
      modKey: false,
    },
  },
  {
    command: "modelPicker.jump.2",
    shortcut: {
      key: "2",
      metaKey: false,
      ctrlKey: true,
      shiftKey: false,
      altKey: false,
      modKey: false,
    },
  },
] as unknown as ResolvedKeybindingsConfig;

type PickerProps = Parameters<typeof ModelPickerContent>[0];

function buildProps(overrides: Partial<PickerProps> = {}): PickerProps {
  return {
    activeInstanceId: id("codex"),
    model: "gpt-5",
    lockedProvider: null,
    instanceEntries: [codexEntry, claudeEntry, geminiEntry, disabledEntry],
    keybindings: jumpKeybindings,
    modelOptionsByInstance: new Map([
      [id("codex"), codexModels],
      [id("claude"), claudeModels],
      [id("gemini"), [{ slug: "flash", name: "Gemini Flash" }]],
      [id("ghost"), [{ slug: "stale", name: "Stale" }]],
    ]),
    terminalOpen: false,
    onInstanceModelChange: vi.fn(),
    ...overrides,
  };
}

let lastProps: PickerProps = buildProps();

function render(props: PickerProps): string {
  lastProps = props;
  hooks.beginRender();
  captured.clear();
  return renderToStaticMarkup(<ModelPickerContent {...props} />);
}

function rerender(): string {
  return render(lastProps);
}

function renderedRowKeys(): string[] {
  return captured.rows.map((row) => `${row.instanceId}:${row.model.slug}`);
}

function keyEvent(overrides: Partial<FakeKeyboardEvent> = {}): FakeKeyboardEvent {
  return {
    key: "x",
    metaKey: false,
    ctrlKey: false,
    shiftKey: false,
    altKey: false,
    defaultPrevented: false,
    repeat: false,
    preventDefault: vi.fn(),
    stopPropagation: vi.fn(),
    ...overrides,
  };
}

beforeAll(() => {
  vi.stubGlobal("HTMLElement", StubHTMLElement);
  vi.stubGlobal("window", {
    requestAnimationFrame: (callback: (time: number) => void) => {
      callback(0);
      return 1;
    },
    cancelAnimationFrame: () => {},
    setTimeout: (callback: () => void) => {
      callback();
      return 1;
    },
    clearTimeout: () => {},
    addEventListener: (type: string, handler: (event: unknown) => void) => {
      windowListeners.push({ type, handler });
    },
    removeEventListener: (type: string) => {
      removedListeners.push({ type });
    },
  });
});

afterAll(() => {
  vi.unstubAllGlobals();
});

beforeEach(() => {
  hooks.reset();
  captured.clear();
  windowListeners.length = 0;
  removedListeners.length = 0;
  testState.favorites = [];
  testState.scrollElement = null;
  testState.updateSettings.mockReset();
  testState.inputHandle.focus.mockReset();
  testState.listHandle.scrollIndexIntoView.mockReset();
  testState.listHandle.scrollToOffset.mockReset();
});

describe("ModelPickerContent", () => {
  it.each(["codex", "claude", "cursor", "opencode", "grok"])(
    "locks a %s chat panel to its creating provider instance",
    (activeProvider) => {
      const providerEntries = ["codex", "claude", "cursor", "opencode", "grok"].map((provider) =>
        entry({
          instanceId: provider,
          driverKind: driver(provider),
          displayName: provider,
        }),
      );
      const modelOptionsByInstance = new Map(
        providerEntries.map((providerEntry) => [
          providerEntry.instanceId,
          [
            {
              slug: `${providerEntry.instanceId}-model`,
              name: `${providerEntry.displayName} Model`,
            },
          ],
        ]),
      );
      const onInstanceModelChange = vi.fn();

      const markup = render(
        buildProps({
          activeInstanceId: id(activeProvider),
          model: `${activeProvider}-model`,
          lockToActiveInstance: true,
          instanceEntries: providerEntries,
          modelOptionsByInstance,
          onInstanceModelChange,
        }),
      );

      expect(markup).not.toContain('data-testid="sidebar"');
      expect(captured.combobox[0]?.items).toEqual([`${activeProvider}:${activeProvider}-model`]);
      expect(renderedRowKeys()).toEqual([`${activeProvider}:${activeProvider}-model`]);

      captured.input[0]?.onChange({ target: { value: "model" } });
      rerender();
      expect(renderedRowKeys()).toEqual([`${activeProvider}:${activeProvider}-model`]);

      const foreignProvider = activeProvider === "codex" ? "claude" : "codex";
      captured.combobox[0]?.onValueChange(`${foreignProvider}:${foreignProvider}-model`);
      expect(onInstanceModelChange).not.toHaveBeenCalled();
    },
  );

  it("keeps duplicate provider instances out of a locked composer picker", () => {
    const personalEntry = entry({
      instanceId: "codex-personal",
      displayName: "Codex Personal",
      isDefault: false,
    });
    const onInstanceModelChange = vi.fn();
    const markup = render(
      buildProps({
        activeInstanceId: id("codex"),
        model: "gpt-5-primary",
        lockToActiveInstance: true,
        instanceEntries: [codexEntry, personalEntry],
        modelOptionsByInstance: new Map([
          [id("codex"), [{ slug: "gpt-5-primary", name: "GPT-5 Primary" }]],
          [id("codex-personal"), [{ slug: "gpt-5-personal", name: "GPT-5 Personal" }]],
        ]),
        onInstanceModelChange,
      }),
    );

    expect(markup).not.toContain('data-testid="sidebar"');
    expect(markup).not.toContain("Codex Personal");
    expect(markup).not.toContain("GPT-5 Personal");
    expect(captured.combobox[0]?.items).toEqual(["codex:gpt-5-primary"]);
    expect(renderedRowKeys()).toEqual(["codex:gpt-5-primary"]);

    captured.input[0]?.onChange({ target: { value: "gpt-5" } });
    rerender();
    expect(renderedRowKeys()).toEqual(["codex:gpt-5-primary"]);

    captured.combobox[0]?.onValueChange("codex-personal:gpt-5-personal");
    captured.combobox[0]?.onValueChange("codex:gpt-5-primary");
    expect(onInstanceModelChange).toHaveBeenCalledWith("codex", "gpt-5-primary");
    expect(onInstanceModelChange.mock.calls.map(([instanceId]) => instanceId)).toEqual(["codex"]);
  });

  it("allows a foreign provider before start and removes it under a started family lock", () => {
    const onInstanceModelChange = vi.fn();
    render(buildProps({ lockToActiveInstance: false, onInstanceModelChange }));

    captured.combobox[0]?.onValueChange("claude:opus");
    expect(onInstanceModelChange).toHaveBeenCalledWith("claude", "opus");

    onInstanceModelChange.mockClear();
    render(
      buildProps({
        activeInstanceId: id("codex"),
        lockToActiveInstance: false,
        lockedProvider: driver("codex"),
        onInstanceModelChange,
      }),
    );
    captured.input[0]?.onChange({ target: { value: "opus" } });
    rerender();

    expect(renderedRowKeys()).toEqual([]);
    expect(captured.combobox[0]?.filteredItems).toEqual([]);
  });

  it("shows every ready provider in one unlocked model list without a provider rail", () => {
    const markup = render(buildProps({ lockToActiveInstance: false, lockedProvider: null }));

    expect(renderedRowKeys()).toEqual(["codex:gpt-5", "codex:gpt-5-codex", "claude:opus"]);
    expect(markup).not.toContain('data-testid="sidebar"');
  });

  it("follows active instance changes while the mounted picker is locked", () => {
    render(buildProps({ lockToActiveInstance: true }));
    expect(renderedRowKeys()).toEqual(["codex:gpt-5", "codex:gpt-5-codex"]);

    render(
      buildProps({
        activeInstanceId: id("claude"),
        model: "opus",
        lockToActiveInstance: true,
      }),
    );
    hooks.runEffects();
    rerender();

    expect(renderedRowKeys()).toEqual(["claude:opus"]);
  });

  it("keeps every ready provider visible when the unlocked active instance changes", () => {
    render(buildProps());
    expect(renderedRowKeys()).toEqual(["codex:gpt-5", "codex:gpt-5-codex", "claude:opus"]);

    render(buildProps({ activeInstanceId: id("gemini"), model: "flash" }));

    expect(renderedRowKeys()).toEqual(["codex:gpt-5", "codex:gpt-5-codex", "claude:opus"]);
  });

  it("renders models from every ready instance and excludes stale instances", () => {
    const markup = render(buildProps());

    expect(markup).not.toContain('data-testid="sidebar"');
    expect(renderedRowKeys()).toEqual(["codex:gpt-5", "codex:gpt-5-codex", "claude:opus"]);
    // gemini (not ready) and ghost (no entry) never enter the flattened list.
    expect(captured.combobox[0]?.items).toEqual([
      "codex:gpt-5",
      "codex:gpt-5-codex",
      "claude:opus",
    ]);
    expect(markup).toContain("No models found");
    // The active model row is marked selected.
    const selected = captured.rows.find((row) => row.isSelected);
    expect(selected?.model.slug).toBe("gpt-5");
  });

  it("keeps the row estimate conservative without recycling picker items", () => {
    render(buildProps());

    expect(captured.legend[0]?.estimatedItemSize).toBe(48);
    expect(captured.legend[0]?.initialScrollIndex).toBe(0);
    expect(captured.legend[0]?.drawDistance).toBe(480);
    expect(captured.legend[0]?.recycleItems).toBe(false);
  });

  it("groups favorites first without hiding other ready models", () => {
    testState.favorites = [{ provider: "claude", model: "opus" }];
    render(buildProps());

    expect(renderedRowKeys()).toEqual(["claude:opus", "codex:gpt-5", "codex:gpt-5-codex"]);
    expect(captured.rows[0]?.isFavorite).toBe(true);
  });

  it("toggles favorites through client settings", () => {
    testState.favorites = [{ provider: "claude", model: "opus" }];
    render(buildProps());

    captured.rows[0]?.onToggleFavorite();
    expect(testState.updateSettings).toHaveBeenCalledWith({ favorites: [] });

    const gpt5Row = captured.rows.find((row) => row.model.slug === "gpt-5");
    gpt5Row?.onToggleFavorite();
    expect(testState.updateSettings).toHaveBeenLastCalledWith({
      favorites: [
        { provider: "claude", model: "opus" },
        { provider: "codex", model: "gpt-5" },
      ],
    });
  });

  it("locks the picker to the active driver kind and continuation group", () => {
    const codexPersonal = entry({
      instanceId: "codex_personal",
      displayName: "Codex Personal",
      isDefault: false,
      continuationGroupKey: "grp-b",
    });
    const lockedCodex = entry({
      instanceId: "codex",
      displayName: "Codex",
      continuationGroupKey: "grp-a",
    });
    const markup = render(
      buildProps({
        lockedProvider: driver("codex"),
        lockedContinuationGroupKey: "grp-a",
        instanceEntries: [claudeEntry, lockedCodex, codexPersonal],
        modelOptionsByInstance: new Map([
          [id("codex"), codexModels],
          [id("codex_personal"), [{ slug: "gpt-5", name: "GPT-5" }]],
          [id("claude"), claudeModels],
        ]),
      }),
    );

    expect(markup).not.toContain('data-testid="sidebar"');
    // Only the matching continuation group's models remain.
    expect(renderedRowKeys()).toEqual(["codex:gpt-5", "codex:gpt-5-codex"]);
    // Locked pickers render full model names rather than short names.
    expect(captured.rows[0]?.preferShortName).toBe(false);
  });

  it("groups favorites first within the locked provider", () => {
    testState.favorites = [
      { provider: "codex", model: "gpt-5" },
      { provider: "claude", model: "opus" },
    ];
    render(buildProps({ lockedProvider: driver("codex") }));

    expect(renderedRowKeys()).toEqual(["codex:gpt-5", "codex:gpt-5-codex"]);
    expect(captured.rows[0]?.isFavorite).toBe(true);
  });

  it("ranks search matches across instances without a provider rail", () => {
    const markup = render(buildProps());
    expect(markup).not.toContain('data-testid="sidebar"');

    captured.input[0]?.onChange({ target: { value: "gpt" } });
    const searching = rerender();

    expect(searching).not.toContain('data-testid="sidebar"');
    expect(renderedRowKeys()).toEqual(["codex:gpt-5", "codex:gpt-5-codex"]);
  });

  it("prefers favorites on search-score ties", () => {
    const twin = entry({
      instanceId: "codex_personal",
      displayName: "Codex",
      isDefault: false,
    });
    testState.favorites = [{ provider: "codex_personal", model: "gpt-5" }];
    render(
      buildProps({
        instanceEntries: [codexEntry, twin],
        modelOptionsByInstance: new Map([
          [id("codex"), [{ slug: "gpt-5", name: "GPT-5" }]],
          [id("codex_personal"), [{ slug: "gpt-5", name: "GPT-5" }]],
        ]),
      }),
    );

    captured.input[0]?.onChange({ target: { value: "gpt-5" } });
    rerender();
    expect(renderedRowKeys()[0]).toBe("codex_personal:gpt-5");
  });

  it("restricts search results to the locked provider", () => {
    render(buildProps({ lockedProvider: driver("claude"), activeInstanceId: id("claude") }));

    captured.input[0]?.onChange({ target: { value: "o" } });
    rerender();
    expect(renderedRowKeys()).toEqual(["claude:opus"]);
  });

  it("resolves combobox selections through resolveSelectableModel", () => {
    const onInstanceModelChange = vi.fn();
    render(buildProps({ onInstanceModelChange }));

    captured.combobox[0]?.onValueChange("codex:gpt-5-codex");
    expect(onInstanceModelChange).toHaveBeenCalledWith("codex", "gpt-5-codex");

    onInstanceModelChange.mockClear();
    // Unknown instance id: no options registered, selection is dropped.
    captured.combobox[0]?.onValueChange("nope:gpt-5");
    // Key without a colon yields an empty slug which cannot resolve.
    captured.combobox[0]?.onValueChange("codex");
    // Non-string values are ignored.
    captured.combobox[0]?.onValueChange(null);
    expect(onInstanceModelChange).not.toHaveBeenCalled();
  });

  it("blocks selection of disabled models", () => {
    const onInstanceModelChange = vi.fn();
    render(
      buildProps({
        onInstanceModelChange,
        getModelDisabledReason: (_instanceId, model) => (model === "gpt-5-codex" ? "quota" : null),
      }),
    );

    const disabledRow = captured.rows.find((row) => row.model.slug === "gpt-5-codex");
    expect(disabledRow?.disabledReason).toBe("quota");
    // Disabled models are skipped when assigning jump shortcuts.
    expect(disabledRow?.jumpLabel).toBeNull();

    captured.combobox[0]?.onValueChange("codex:gpt-5-codex");
    expect(onInstanceModelChange).not.toHaveBeenCalled();

    captured.combobox[0]?.onValueChange("codex:gpt-5");
    expect(onInstanceModelChange).toHaveBeenCalledWith("codex", "gpt-5");
  });

  it("renders jump shortcut labels for selectable models", () => {
    render(buildProps());
    expect(captured.rows[0]?.jumpLabel).toContain("1");
    expect(captured.rows[1]?.jumpLabel).toContain("2");
  });

  it("closes on Escape and selects the highlighted model on Enter", () => {
    const onRequestClose = vi.fn();
    const onInstanceModelChange = vi.fn();
    render(buildProps({ onRequestClose, onInstanceModelChange }));

    const escape = keyEvent({ key: "Escape" });
    captured.input[0]?.onKeyDown(escape);
    expect(onRequestClose).toHaveBeenCalled();
    expect(escape.preventDefault).toHaveBeenCalled();

    // Enter without a highlighted model only stops propagation.
    const plainEnter = keyEvent({ key: "Enter" });
    captured.input[0]?.onKeyDown(plainEnter);
    expect(plainEnter.preventDefault).not.toHaveBeenCalled();
    expect(plainEnter.stopPropagation).toHaveBeenCalled();

    captured.combobox[0]?.onItemHighlighted("codex:gpt-5-codex", {
      reason: "keyboard",
      index: 1,
    });
    expect(testState.listHandle.scrollIndexIntoView).toHaveBeenCalledWith({
      index: 1,
      animated: false,
    });

    const enter = keyEvent({ key: "Enter", preventBaseUIHandler: vi.fn() });
    captured.input[0]?.onKeyDown(enter);
    expect(enter.preventBaseUIHandler).toHaveBeenCalled();
    expect(onInstanceModelChange).toHaveBeenCalledWith("codex", "gpt-5-codex");

    const mouseDown = { stopPropagation: vi.fn() };
    captured.input[0]?.onMouseDown(mouseDown);
    captured.input[0]?.onTouchStart(mouseDown);
    expect(mouseDown.stopPropagation).toHaveBeenCalledTimes(2);
  });

  it("selects models from global jump shortcuts", () => {
    const onInstanceModelChange = vi.fn();
    render(buildProps({ onInstanceModelChange }));
    const cleanups = hooks.runEffects();

    const keydown = windowListeners.find((listener) => listener.type === "keydown");
    expect(keydown).toBeDefined();

    // Non-matching keys and already-handled events are ignored.
    keydown?.handler(keyEvent({ key: "z" }));
    keydown?.handler(keyEvent({ key: "1", ctrlKey: true, defaultPrevented: true }));
    keydown?.handler(keyEvent({ key: "1", ctrlKey: true, repeat: true }));
    expect(onInstanceModelChange).not.toHaveBeenCalled();

    const match = keyEvent({ key: "2", ctrlKey: true });
    keydown?.handler(match);
    expect(match.preventDefault).toHaveBeenCalled();
    expect(onInstanceModelChange).toHaveBeenCalledWith("codex", "gpt-5-codex");

    for (const cleanup of cleanups) cleanup();
    expect(removedListeners.some((listener) => listener.type === "keydown")).toBe(true);
  });

  it("ignores jump shortcuts pointing past the filtered list", () => {
    const onInstanceModelChange = vi.fn();
    render(
      buildProps({
        onInstanceModelChange,
        modelOptionsByInstance: new Map([[id("codex"), [codexModels[0]!]]]),
      }),
    );
    hooks.runEffects();

    const keydown = windowListeners.find((listener) => listener.type === "keydown");
    keydown?.handler(keyEvent({ key: "2", ctrlKey: true }));
    expect(onInstanceModelChange).not.toHaveBeenCalled();
  });

  it("updates scroll fade masks from the list's scroll position", () => {
    const scrollElement = new StubHTMLElement();
    scrollElement.scrollHeight = 400;
    scrollElement.clientHeight = 100;
    scrollElement.scrollTop = 50;
    testState.scrollElement = scrollElement;

    render(buildProps());
    hooks.runEffects();

    const markup = rerender();
    expect(markup).toContain("mask-t-from");
    expect(markup).toContain("mask-b-from");

    // Scrolled to the bottom: only the top fade remains.
    scrollElement.scrollTop = 300;
    captured.legend[0]?.onScroll();
    const bottom = rerender();
    expect(bottom).toContain("mask-t-from");
    expect(bottom).not.toContain("mask-b-from");
  });

  it("skips scroll fade updates when no scrollable node exists", () => {
    testState.scrollElement = null;
    render(buildProps());
    captured.legend[0]?.onLayout();
    const markup = rerender();
    expect(markup).not.toContain("mask-t-from");
  });
});
