import type {
  KeybindingCommand,
  KeybindingShortcut,
  KeybindingWhenNode,
  ResolvedKeybindingRule,
  ResolvedKeybindingsConfig,
} from "@coddy/contracts";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

/**
 * Instrumented replacements for the stateful React hooks. The component tree is
 * rendered once per scenario with `renderToStaticMarkup`; state can be seeded
 * per-scenario, setter/dispatch calls are recorded (functional updaters are
 * executed against the rendered value so their bodies run), and effects are
 * captured so tests can run them against fake DOM containers.
 */
const harness = vi.hoisted(() => {
  type Matcher = (initial: unknown) => boolean;

  const state = {
    stateSeeds: [] as Array<{ match: Matcher; value: unknown }>,
    reducerSeeds: [] as Array<{ match: Matcher; patch: Record<string, unknown> }>,
    setStateCalls: [] as Array<{ initial: unknown; next: unknown; applied: unknown }>,
    dispatchCalls: [] as Array<Record<string, unknown>>,
    effects: [] as Array<() => void | (() => void)>,
    refs: [] as Array<{ current: unknown }>,
    reset() {
      state.stateSeeds.length = 0;
      state.reducerSeeds.length = 0;
      state.setStateCalls.length = 0;
      state.dispatchCalls.length = 0;
      state.effects.length = 0;
      state.refs.length = 0;
    },
    seedState(match: Matcher, value: unknown) {
      state.stateSeeds.push({ match, value });
    },
    seedReducer(match: Matcher, patch: Record<string, unknown>) {
      state.reducerSeeds.push({ match, patch });
    },
    runEffects(): Array<() => void> {
      const cleanups: Array<() => void> = [];
      for (const effect of state.effects) {
        const cleanup = effect();
        if (typeof cleanup === "function") cleanups.push(cleanup);
      }
      return cleanups;
    },
  };
  return state;
});

/** Registry of rendered element props so tests can look up and invoke handlers. */
const ui = vi.hoisted(() => {
  const registry = {
    entries: [] as Array<{ kind: string; props: Record<string, unknown> }>,
    reset() {
      registry.entries.length = 0;
    },
    record(kind: string, props: unknown) {
      if (props && typeof props === "object") {
        registry.entries.push({ kind, props: props as Record<string, unknown> });
      }
    },
    filter(kind: string, predicate?: (props: Record<string, unknown>) => boolean) {
      return registry.entries
        .filter((entry) => entry.kind === kind && (predicate?.(entry.props) ?? true))
        .map((entry) => entry.props);
    },
    find(kind: string, predicate: (props: Record<string, unknown>) => boolean) {
      const found = registry.entries.find(
        (entry) => entry.kind === kind && predicate(entry.props),
      )?.props;
      if (!found) throw new Error(`No recorded "${kind}" element matched`);
      return found;
    },
    byLabel(kind: string, label: string) {
      return registry.find(kind, (props) => props["aria-label"] === label);
    },
  };
  return registry;
});

const testState = vi.hoisted(() => ({
  atoms: {
    keybindings: Symbol("primaryServerKeybindingsAtom"),
    configPath: Symbol("primaryServerKeybindingsConfigPathAtom"),
    editors: Symbol("primaryServerAvailableEditorsAtom"),
  },
  commands: {
    upsert: { label: "upsertKeybinding" },
    remove: { label: "removeKeybinding" },
  },
  atomValues: new Map<unknown, unknown>(),
  upsertKeybinding: vi.fn<(input: unknown) => Promise<unknown>>(),
  removeKeybinding: vi.fn<(input: unknown) => Promise<unknown>>(),
  openInPreferredEditor: vi.fn<(path: string) => Promise<unknown>>(),
  toastAdd: vi.fn(),
  primaryEnvironment: null as { environmentId: string } | null,
}));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();

  const resolveInitial = (initial: unknown): unknown =>
    typeof initial === "function" ? (initial as () => unknown)() : initial;

  const useState = (initial?: unknown) => {
    const resolved = resolveInitial(initial);
    const seedIndex = harness.stateSeeds.findIndex((seed) => seed.match(resolved));
    const value = seedIndex >= 0 ? harness.stateSeeds.splice(seedIndex, 1)[0]!.value : resolved;
    const setValue = (next: unknown) => {
      const applied =
        typeof next === "function" ? (next as (value: unknown) => unknown)(value) : next;
      harness.setStateCalls.push({ initial: resolved, next, applied });
    };
    return [value, setValue];
  };

  const useReducer = (
    reducer: (state: unknown, action: unknown) => unknown,
    initialArg: unknown,
    init?: (arg: unknown) => unknown,
  ) => {
    const base = init ? init(initialArg) : initialArg;
    const seedIndex = harness.reducerSeeds.findIndex((seed) => seed.match(base));
    const value =
      seedIndex >= 0
        ? Object.assign({}, base, harness.reducerSeeds.splice(seedIndex, 1)[0]!.patch)
        : base;
    const dispatch = (action: unknown) => {
      harness.dispatchCalls.push(action as Record<string, unknown>);
      reducer(value, action);
    };
    return [value, dispatch];
  };

  const useEffect = (effect: () => void | (() => void)) => {
    harness.effects.push(effect);
  };

  const useRef = (initial?: unknown) => {
    const ref = { current: initial ?? null };
    harness.refs.push(ref);
    return ref;
  };

  return {
    ...actual,
    useState: useState as typeof actual.useState,
    useReducer: useReducer as typeof actual.useReducer,
    useEffect: useEffect as typeof actual.useEffect,
    useRef: useRef as typeof actual.useRef,
  };
});

vi.mock("react/jsx-runtime", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react/jsx-runtime")>();
  return {
    ...actual,
    jsx: ((type: unknown, props: unknown, key: unknown) => {
      if (typeof type === "string") ui.record(type, props);
      return (actual.jsx as (...args: Array<unknown>) => unknown)(type, props, key);
    }) as typeof actual.jsx,
    jsxs: ((type: unknown, props: unknown, key: unknown) => {
      if (typeof type === "string") ui.record(type, props);
      return (actual.jsxs as (...args: Array<unknown>) => unknown)(type, props, key);
    }) as typeof actual.jsxs,
  };
});

vi.mock("react/jsx-dev-runtime", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react/jsx-dev-runtime")>();
  return {
    ...actual,
    jsxDEV: ((type: unknown, props: unknown, ...rest: Array<unknown>) => {
      if (typeof type === "string") ui.record(type, props);
      return (actual.jsxDEV as (...args: Array<unknown>) => unknown)(type, props, ...rest);
    }) as typeof actual.jsxDEV,
  };
});

vi.mock("@effect/atom-react", () => ({
  useAtomValue: (atom: unknown) => testState.atomValues.get(atom),
}));

vi.mock("../../state/server", () => ({
  primaryServerKeybindingsAtom: testState.atoms.keybindings,
  primaryServerKeybindingsConfigPathAtom: testState.atoms.configPath,
  primaryServerAvailableEditorsAtom: testState.atoms.editors,
  serverEnvironment: {
    upsertKeybinding: testState.commands.upsert,
    removeKeybinding: testState.commands.remove,
  },
}));

vi.mock("../../state/environments", () => ({
  usePrimaryEnvironment: () => testState.primaryEnvironment,
}));

vi.mock("../../state/use-atom-command", () => ({
  useAtomCommand: (command: unknown) =>
    command === testState.commands.upsert ? testState.upsertKeybinding : testState.removeKeybinding,
}));

vi.mock("../../editorPreferences", () => ({
  useOpenInPreferredEditor: () => testState.openInPreferredEditor,
}));

vi.mock("@coddy/client-runtime/state/runtime", () => ({
  isAtomCommandInterrupted: (result: { _tag: string }) => result._tag === "Interrupted",
  squashAtomCommandFailure: (result: { error?: unknown }) => result.error,
}));

vi.mock("../ui/toast", () => ({
  toastManager: { add: (toast: unknown) => testState.toastAdd(toast) },
}));

vi.mock("../ui/button", () => ({
  Button: ({ children, ...props }: Record<string, unknown> & { children?: React.ReactNode }) => {
    ui.record("Button", { children, ...props });
    return (
      <button
        type="button"
        aria-label={props["aria-label"] as string | undefined}
        disabled={Boolean(props.disabled)}
      >
        {children}
      </button>
    );
  },
}));

vi.mock("../ui/input", () => ({
  Input: (props: Record<string, unknown>) => {
    ui.record("Input", props);
    return (
      <input
        aria-label={props["aria-label"] as string | undefined}
        aria-invalid={props["aria-invalid"] as boolean | undefined}
        value={props.value as string | undefined}
        placeholder={props.placeholder as string | undefined}
        readOnly
      />
    );
  },
}));

vi.mock("../ui/kbd", () => ({
  Kbd: ({ children }: { children?: React.ReactNode }) => <kbd>{children}</kbd>,
  KbdGroup: ({ children }: { children?: React.ReactNode }) => <span>{children}</span>,
}));

vi.mock("../ui/menu", () => ({
  Menu: ({ children }: { children?: React.ReactNode }) => <div>{children}</div>,
  MenuTrigger: ({ render, children }: { render?: React.ReactNode; children?: React.ReactNode }) => (
    <span>
      {render}
      {children}
    </span>
  ),
  MenuPopup: ({ children }: { children?: React.ReactNode }) => <div>{children}</div>,
  MenuItem: ({ children, ...props }: Record<string, unknown> & { children?: React.ReactNode }) => {
    ui.record("MenuItem", { children, ...props });
    return <div data-menu-item>{children}</div>;
  },
}));

vi.mock("../ui/popover", () => ({
  Popover: ({ children }: { children?: React.ReactNode }) => <div>{children}</div>,
  PopoverTrigger: ({
    children,
    ...props
  }: Record<string, unknown> & { children?: React.ReactNode }) => {
    ui.record("PopoverTrigger", { children, ...props });
    return <button type="button">{children}</button>;
  },
  PopoverContent: ({ children }: { children?: React.ReactNode }) => <div>{children}</div>,
}));

vi.mock("../ui/scroll-area", () => ({
  ScrollArea: ({ children }: { children?: React.ReactNode }) => <div>{children}</div>,
}));

vi.mock("../ui/select", () => ({
  Select: ({ children, ...props }: Record<string, unknown> & { children?: React.ReactNode }) => {
    ui.record("Select", { children, ...props });
    return <div data-select-value={String(props.value ?? "")}>{children}</div>;
  },
  SelectTrigger: ({ children }: { children?: React.ReactNode }) => <div>{children}</div>,
  SelectValue: ({ placeholder }: { placeholder?: string }) => <span>{placeholder ?? ""}</span>,
  SelectContent: ({ children }: { children?: React.ReactNode }) => <div>{children}</div>,
  SelectItem: ({ children, value }: { children?: React.ReactNode; value?: string }) => (
    <div data-select-item={value}>{children}</div>
  ),
}));

vi.mock("../ui/toggle", () => ({
  Toggle: ({ children, ...props }: Record<string, unknown> & { children?: React.ReactNode }) => {
    ui.record("Toggle", { children, ...props });
    return (
      <button
        type="button"
        aria-label={props["aria-label"] as string | undefined}
        data-pressed={String(Boolean(props.pressed))}
      >
        {children}
      </button>
    );
  },
}));

vi.mock("../ui/tooltip", () => ({
  Tooltip: ({ children }: { children?: React.ReactNode }) => <>{children}</>,
  TooltipTrigger: ({
    render,
    children,
  }: {
    render?: React.ReactNode;
    children?: React.ReactNode;
  }) => (
    <span>
      {render}
      {children}
    </span>
  ),
  TooltipPopup: ({ children }: { children?: React.ReactNode }) => <div>{children}</div>,
}));

import { KeybindingsSettingsPanel } from "./KeybindingsSettings";

function shortcut(
  key: string,
  modifiers: Partial<Omit<KeybindingShortcut, "key">> = {},
): KeybindingShortcut {
  return {
    key,
    metaKey: false,
    ctrlKey: false,
    shiftKey: false,
    altKey: false,
    modKey: false,
    ...modifiers,
  };
}

const identifier = (name: string): KeybindingWhenNode => ({ type: "identifier", name });
const notNode = (node: KeybindingWhenNode): KeybindingWhenNode => ({ type: "not", node });
const andNode = (left: KeybindingWhenNode, right: KeybindingWhenNode): KeybindingWhenNode => ({
  type: "and",
  left,
  right,
});
const orNode = (left: KeybindingWhenNode, right: KeybindingWhenNode): KeybindingWhenNode => ({
  type: "or",
  left,
  right,
});

function binding(
  command: KeybindingCommand,
  bindingShortcut: KeybindingShortcut,
  whenAst?: KeybindingWhenNode,
): ResolvedKeybindingRule {
  return whenAst
    ? { command, shortcut: bindingShortcut, whenAst }
    : { command, shortcut: bindingShortcut };
}

/** A default binding straight from the shipped defaults: sidebar.toggle → mod+b. */
const defaultSidebarToggle = binding("sidebar.toggle", shortcut("b", { modKey: true }));
/** A customized binding whose command has a shipped default (terminal.split → mod+d when terminalFocus). */
const customTerminalSplit = binding(
  "terminal.split",
  shortcut("d", { modKey: true, altKey: true }),
  identifier("terminalFocus"),
);
/** A project script binding. */
const projectScript = binding(
  "script.deploy.run",
  shortcut("p", { ctrlKey: true, shiftKey: true }),
  andNode(identifier("terminalFocus"), notNode(identifier("mysteryVar"))),
);

function setKeybindings(config: ResolvedKeybindingsConfig): void {
  testState.atomValues.set(testState.atoms.keybindings, config);
}

function renderPanel(): string {
  ui.reset();
  harness.effects.length = 0;
  harness.refs.length = 0;
  harness.setStateCalls.length = 0;
  harness.dispatchCalls.length = 0;
  return renderToStaticMarkup(<KeybindingsSettingsPanel />);
}

/** Seed panel-level `useState(false)` slots: [isSearchOpen, isAddingBinding]. */
function seedPanelFlags({ searchOpen = false, adding = false } = {}): void {
  harness.seedState((initial) => initial === false, searchOpen);
  harness.seedState((initial) => initial === false, adding);
}

/**
 * Seed the new-binding row's command draft. The panel's `query` state also
 * starts as an empty string and renders first, so absorb that slot before
 * targeting `commandDraft`.
 */
function seedNewBindingCommand(command: string): void {
  harness.seedState((initial) => initial === "", "");
  harness.seedState((initial) => initial === "", command);
}

function keyEvent(
  key: string,
  modifiers: Partial<{
    metaKey: boolean;
    ctrlKey: boolean;
    altKey: boolean;
    shiftKey: boolean;
  }> = {},
  target: unknown = null,
) {
  const flags = { metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...modifiers };
  return {
    key,
    ...flags,
    target,
    preventDefault: vi.fn(),
    nativeEvent: { key, ...flags },
  };
}

function reactKeyEvent(
  key: string,
  modifiers: Partial<{
    metaKey: boolean;
    ctrlKey: boolean;
    altKey: boolean;
    shiftKey: boolean;
  }> = {},
): ReactKeyboardEvent<HTMLInputElement> {
  return keyEvent(key, modifiers) as unknown as ReactKeyboardEvent<HTMLInputElement>;
}

function changeEvent(value: string) {
  return { currentTarget: { value } } as unknown as React.ChangeEvent<HTMLInputElement>;
}

function lastWhenDraftDispatch(): KeybindingWhenNode | undefined {
  const entries = harness.dispatchCalls.filter((call) => "whenDraft" in call);
  const last = entries[entries.length - 1];
  if (!last) throw new Error("No whenDraft dispatch was recorded");
  return last.whenDraft as KeybindingWhenNode | undefined;
}

async function flushPromises(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

beforeEach(() => {
  harness.reset();
  ui.reset();
  testState.atomValues.clear();
  testState.upsertKeybinding.mockReset().mockResolvedValue({ _tag: "Success" });
  testState.removeKeybinding.mockReset().mockResolvedValue({ _tag: "Success" });
  testState.openInPreferredEditor.mockReset().mockResolvedValue({ _tag: "Success" });
  testState.toastAdd.mockReset();
  testState.primaryEnvironment = { environmentId: "environment-primary" };
  setKeybindings([defaultSidebarToggle, customTerminalSplit, projectScript]);
  testState.atomValues.set(testState.atoms.configPath, "/home/user/.coddy/studio/keybindings.json");
  testState.atomValues.set(testState.atoms.editors, []);
  vi.stubGlobal("navigator", { platform: "Win32" });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("KeybindingsSettingsPanel rendering", () => {
  it("renders command labels, key pills, sources, and the browser notice", () => {
    const markup = renderPanel();

    expect(markup).toContain("Sidebar: Toggle");
    expect(markup).toContain("Terminal: Split");
    expect(markup).toContain("Run Script: Deploy");
    // Pill parts on a non-mac platform: mod → Ctrl, alt → Alt, single char uppercased.
    expect(markup).toContain(">Ctrl</kbd>");
    expect(markup).toContain(">Alt</kbd>");
    expect(markup).toContain(">B</kbd>");
    expect(markup).toContain(">D</kbd>");
    // The when column shows the expression, or "Always" when empty.
    expect(markup).toContain("terminalFocus");
    expect(markup).toContain("Always");
    // Not running in a desktop host, so the browser warning renders.
    expect(markup).toContain("Use the desktop app for better keybinding support.");
    expect(markup).toContain("3 bindings");
    expect(markup).toContain('aria-label="Search keybindings"');
    expect(markup).toContain('aria-label="Add keybinding"');
    expect(markup).toContain('aria-label="Open keybindings.json"');
  });

  it("marks unknown when-variables with a warning", () => {
    const markup = renderPanel();
    expect(markup).toContain("Unknown condition: mysteryVar");
    expect(markup).toContain("does not recognize this condition yet");
  });

  it("flags conflicting bindings that can match at the same time", () => {
    setKeybindings([
      customTerminalSplit,
      binding("chat.new", shortcut("d", { modKey: true, altKey: true })),
    ]);
    const markup = renderPanel();
    expect(markup).toContain('aria-label="Conflicts with Chat: New."');
    expect(markup).toContain('aria-label="Conflicts with Terminal: Split."');
    expect(markup).toContain("The most recent matching binding wins");
  });

  it("uses the singular label for a single binding", () => {
    setKeybindings([defaultSidebarToggle]);
    const markup = renderPanel();
    expect(markup).toContain("1 binding");
    expect(markup).not.toContain("1 bindings");
  });

  it("shows the empty state when a search matches nothing", () => {
    harness.seedState((initial) => initial === "", "zzz-no-match");
    const markup = renderPanel();
    expect(markup).toContain("No keybindings match your search.");
    expect(markup).toContain("0 bindings");
  });
});

describe("header search", () => {
  it("opens the search field from the collapsed button", () => {
    renderPanel();
    const button = ui.byLabel("Button", "Search keybindings");
    (button.onClick as () => void)();
    expect(harness.setStateCalls.some((call) => call.next === true)).toBe(true);
  });

  it("renders an expanded input that clears and closes on Escape", () => {
    seedPanelFlags({ searchOpen: true });
    const markup = renderPanel();
    expect(markup).toContain('placeholder="Search keybindings"');

    const input = ui.find("input", (props) => props["aria-label"] === "Search keybindings");
    (input.onChange as (event: unknown) => void)(changeEvent("chat"));
    expect(harness.setStateCalls.some((call) => call.next === "chat")).toBe(true);

    const escape = keyEvent("Escape");
    (input.onKeyDown as (event: unknown) => void)(escape);
    expect(escape.preventDefault).toHaveBeenCalled();
    expect(harness.setStateCalls.some((call) => call.next === "")).toBe(true);
    expect(harness.setStateCalls.some((call) => call.next === false)).toBe(true);

    const ignored = keyEvent("a");
    (input.onKeyDown as (event: unknown) => void)(ignored);
    expect(ignored.preventDefault).not.toHaveBeenCalled();
  });

  it("collapses on blur only while the query is empty", () => {
    seedPanelFlags({ searchOpen: true });
    renderPanel();
    const input = ui.find("input", (props) => props["aria-label"] === "Search keybindings");
    harness.setStateCalls.length = 0;
    (input.onBlur as () => void)();
    expect(harness.setStateCalls.some((call) => call.next === false)).toBe(true);

    harness.seedState((initial) => initial === "", "chat");
    seedPanelFlags({ searchOpen: true });
    renderPanel();
    const filledInput = ui.find("input", (props) => props["aria-label"] === "Search keybindings");
    harness.setStateCalls.length = 0;
    (filledInput.onBlur as () => void)();
    expect(harness.setStateCalls).toHaveLength(0);
  });

  it("focuses search on mod+f unless typing in another field", () => {
    const listeners = new Map<string, (event: unknown) => void>();
    const rafCallbacks: Array<() => void> = [];
    class FakeHTMLElement {
      tagName = "DIV";
      isContentEditable = false;
      focus = vi.fn();
      select = vi.fn();
    }
    vi.stubGlobal("HTMLElement", FakeHTMLElement);
    vi.stubGlobal("window", {
      addEventListener: (type: string, handler: (event: unknown) => void) => {
        listeners.set(type, handler);
      },
      removeEventListener: (type: string) => {
        listeners.delete(type);
      },
    });
    vi.stubGlobal("requestAnimationFrame", (callback: () => void) => {
      rafCallbacks.push(callback);
      return rafCallbacks.length;
    });

    renderPanel();
    const cleanups = harness.runEffects();
    const handler = listeners.get("keydown");
    expect(handler).toBeDefined();

    const searchInputRef = harness.refs[0]!;
    const searchElement = new FakeHTMLElement();
    searchElement.tagName = "INPUT";
    searchInputRef.current = searchElement;

    // Not the mod+f chord → ignored.
    const plain = keyEvent("f");
    handler!(plain);
    expect(plain.preventDefault).not.toHaveBeenCalled();

    const withAlt = keyEvent("f", { ctrlKey: true, altKey: true });
    handler!(withAlt);
    expect(withAlt.preventDefault).not.toHaveBeenCalled();

    // Typing in some other input → ignored.
    const otherInput = new FakeHTMLElement();
    otherInput.tagName = "INPUT";
    const inInput = keyEvent("f", { ctrlKey: true }, otherInput);
    handler!(inInput);
    expect(inInput.preventDefault).not.toHaveBeenCalled();

    const editable = new FakeHTMLElement();
    editable.isContentEditable = true;
    const inEditable = keyEvent("f", { ctrlKey: true }, editable);
    handler!(inEditable);
    expect(inEditable.preventDefault).not.toHaveBeenCalled();

    // From a non-editable target → open + focus the search input.
    const fromBody = keyEvent("F", { metaKey: true }, {});
    handler!(fromBody);
    expect(fromBody.preventDefault).toHaveBeenCalled();
    expect(harness.setStateCalls.some((call) => call.next === true)).toBe(true);
    expect(rafCallbacks.length).toBeGreaterThan(0);
    for (const callback of rafCallbacks) callback();
    expect(searchElement.focus).toHaveBeenCalled();
    expect(searchElement.select).toHaveBeenCalled();

    // When focus is already in the search input itself, mod+f still applies.
    const fromSearch = keyEvent("f", { ctrlKey: true }, searchElement);
    handler!(fromSearch);
    expect(fromSearch.preventDefault).toHaveBeenCalled();

    for (const cleanup of cleanups) cleanup();
    expect(listeners.has("keydown")).toBe(false);
  });
});

describe("adding a keybinding", () => {
  it("renders the new-binding row with command options and a cancel action", () => {
    seedPanelFlags({ adding: true });
    const markup = renderPanel();
    expect(markup).toContain("Command");
    expect(markup).toContain('aria-label="Cancel new keybinding"');
    expect(markup).toContain('aria-label="Keybinding for new keybinding"');
    expect(markup).toContain("4 bindings");
    // Command options come from the defaults.
    expect(markup).toContain('data-select-item="sidebar.toggle"');

    const cancel = ui.byLabel("Button", "Cancel new keybinding");
    harness.setStateCalls.length = 0;
    (cancel.onClick as () => void)();
    expect(harness.setStateCalls.some((call) => call.next === false)).toBe(true);

    const commandSelect = ui.find("Select", (props) => props.value === "");
    (commandSelect.onValueChange as (value: string) => void)("chat.new");
    expect(harness.setStateCalls.some((call) => call.next === "chat.new")).toBe(true);
  });

  it("records shortcuts from keyboard events and ignores incomplete chords", () => {
    seedPanelFlags({ adding: true });
    renderPanel();
    const input = ui.find(
      "Input",
      (props) => props["aria-label"] === "Keybinding for new keybinding",
    );

    (input.onFocus as () => void)();
    expect(harness.dispatchCalls.some((call) => call.isRecording === true)).toBe(true);

    (input.onBlur as () => void)();
    expect(harness.dispatchCalls.some((call) => call.isRecording === false)).toBe(true);

    (input.onChange as (event: unknown) => void)(changeEvent("mod+q"));
    expect(harness.dispatchCalls.some((call) => call.keyDraft === "mod+q")).toBe(true);

    harness.dispatchCalls.length = 0;
    const tab = reactKeyEvent("Tab");
    (input.onKeyDown as (event: ReactKeyboardEvent<HTMLInputElement>) => void)(tab);
    expect(harness.dispatchCalls).toHaveLength(0);

    const escape = reactKeyEvent("Escape");
    (input.onKeyDown as (event: ReactKeyboardEvent<HTMLInputElement>) => void)(escape);
    expect(harness.dispatchCalls.some((call) => call.keyDraft === "")).toBe(true);

    harness.dispatchCalls.length = 0;
    const chord = reactKeyEvent("S", { ctrlKey: true });
    (input.onKeyDown as (event: ReactKeyboardEvent<HTMLInputElement>) => void)(chord);
    expect(harness.dispatchCalls.some((call) => call.keyDraft === "mod+s")).toBe(true);

    harness.dispatchCalls.length = 0;
    const bare = reactKeyEvent("x");
    (input.onKeyDown as (event: ReactKeyboardEvent<HTMLInputElement>) => void)(bare);
    expect(harness.dispatchCalls).toHaveLength(0);
  });

  it("does not save while no command is selected", () => {
    seedPanelFlags({ adding: true });
    renderPanel();
    const save = ui.find("Button", (props) => props.children === "Save");
    expect(save.disabled).toBe(true);
    (save.onClick as () => void)();
    expect(testState.upsertKeybinding).not.toHaveBeenCalled();
  });

  it("saves a new binding and closes the editor on success", async () => {
    seedPanelFlags({ adding: true });
    seedNewBindingCommand("chat.new");
    harness.seedReducer((base) => (base as { keyDraft: string }).keyDraft === "", {
      keyDraft: "mod+alt+z",
    });
    renderPanel();

    const save = ui.find("Button", (props) => props.children === "Save");
    expect(save.disabled).toBe(false);
    harness.setStateCalls.length = 0;
    (save.onClick as () => void)();
    expect(testState.upsertKeybinding).toHaveBeenCalledWith({
      environmentId: "environment-primary",
      input: { command: "chat.new", key: "mod+alt+z" },
    });
    await flushPromises();
    // Saving command set + cleared, and the add row closed on success.
    expect(harness.setStateCalls.some((call) => call.next === "chat.new")).toBe(true);
    expect(harness.setStateCalls.some((call) => call.next === null)).toBe(true);
    expect(harness.setStateCalls.some((call) => call.next === false)).toBe(true);
    expect(testState.toastAdd).not.toHaveBeenCalled();
  });

  it("reports save failures with the error message", async () => {
    testState.upsertKeybinding.mockResolvedValue({
      _tag: "Failure",
      error: new Error("write denied"),
    });
    seedPanelFlags({ adding: true });
    seedNewBindingCommand("chat.new");
    harness.seedReducer((base) => (base as { keyDraft: string }).keyDraft === "", {
      keyDraft: "mod+alt+z",
      whenDraft: identifier("terminalFocus"),
    });
    renderPanel();

    const save = ui.find("Button", (props) => props.children === "Save");
    (save.onClick as () => void)();
    expect(testState.upsertKeybinding).toHaveBeenCalledWith({
      environmentId: "environment-primary",
      input: { command: "chat.new", key: "mod+alt+z", when: "terminalFocus" },
    });
    await flushPromises();
    expect(testState.toastAdd).toHaveBeenCalledWith({
      title: "Unable to save keybinding",
      description: "write denied",
      type: "error",
    });
  });

  it("uses a generic failure description for non-Error failures and stays quiet on interrupts", async () => {
    testState.upsertKeybinding.mockResolvedValue({ _tag: "Failure", error: "denied" });
    seedPanelFlags({ adding: true });
    seedNewBindingCommand("chat.new");
    harness.seedReducer((base) => (base as { keyDraft: string }).keyDraft === "", {
      keyDraft: "mod+1",
    });
    renderPanel();
    (ui.find("Button", (props) => props.children === "Save").onClick as () => void)();
    await flushPromises();
    expect(testState.toastAdd).toHaveBeenCalledWith({
      title: "Unable to save keybinding",
      description: "The keybinding was not saved.",
      type: "error",
    });

    testState.toastAdd.mockReset();
    testState.upsertKeybinding.mockResolvedValue({ _tag: "Interrupted" });
    seedPanelFlags({ adding: true });
    seedNewBindingCommand("chat.new");
    harness.seedReducer((base) => (base as { keyDraft: string }).keyDraft === "", {
      keyDraft: "mod+2",
    });
    renderPanel();
    (ui.find("Button", (props) => props.children === "Save").onClick as () => void)();
    await flushPromises();
    expect(testState.toastAdd).not.toHaveBeenCalled();
  });

  it("does nothing without a primary environment", () => {
    testState.primaryEnvironment = null;
    seedPanelFlags({ adding: true });
    seedNewBindingCommand("chat.new");
    harness.seedReducer((base) => (base as { keyDraft: string }).keyDraft === "", {
      keyDraft: "mod+3",
    });
    renderPanel();
    (ui.find("Button", (props) => props.children === "Save").onClick as () => void)();
    expect(testState.upsertKeybinding).not.toHaveBeenCalled();
  });
});

describe("editing an existing row", () => {
  it("shows the pill for a clean row and enters recording mode on click", () => {
    setKeybindings([defaultSidebarToggle]);
    const markup = renderPanel();
    expect(markup).toContain('aria-label="Edit shortcut for Sidebar: Toggle"');
    expect(markup).toContain(">Edit</span>");

    const pill = ui.find(
      "button",
      (props) => props["aria-label"] === "Edit shortcut for Sidebar: Toggle",
    );
    harness.dispatchCalls.length = 0;
    (pill.onClick as () => void)();
    expect(harness.dispatchCalls.some((call) => call.isRecording === true)).toBe(true);
  });

  it("shows a recording input while capturing and restores the row key on Escape", () => {
    setKeybindings([defaultSidebarToggle]);
    harness.seedReducer((base) => (base as { keyDraft: string }).keyDraft === "mod+b", {
      isRecording: true,
    });
    const markup = renderPanel();
    expect(markup).toContain('placeholder="Press shortcut"');

    const input = ui.find(
      "Input",
      (props) => props["aria-label"] === "Keybinding for Sidebar: Toggle",
    );
    harness.dispatchCalls.length = 0;
    const escape = reactKeyEvent("Escape");
    (input.onKeyDown as (event: ReactKeyboardEvent<HTMLInputElement>) => void)(escape);
    expect(harness.dispatchCalls.some((call) => call.keyDraft === "mod+b")).toBe(true);

    const chord = reactKeyEvent("K", { ctrlKey: true, shiftKey: true });
    (input.onKeyDown as (event: ReactKeyboardEvent<HTMLInputElement>) => void)(chord);
    expect(harness.dispatchCalls.some((call) => call.keyDraft === "mod+shift+k")).toBe(true);
  });

  it("saves a dirty row with its replace target including the when clause", () => {
    setKeybindings([customTerminalSplit]);
    harness.seedReducer((base) => (base as { keyDraft: string }).keyDraft === "mod+alt+d", {
      keyDraft: "mod+alt+p",
    });
    renderPanel();

    const save = ui.find("Button", (props) => props.children === "Save");
    (save.onClick as () => void)();
    expect(testState.upsertKeybinding).toHaveBeenCalledWith({
      environmentId: "environment-primary",
      input: {
        command: "terminal.split",
        key: "mod+alt+p",
        when: "terminalFocus",
        replace: { command: "terminal.split", key: "mod+alt+d", when: "terminalFocus" },
      },
    });
  });

  it("omits the when clause from the replace target when the row has none", () => {
    const customSidebar = binding(
      "sidebar.toggle",
      shortcut("b", { modKey: true, shiftKey: true }),
    );
    setKeybindings([customSidebar]);
    harness.seedReducer((base) => (base as { keyDraft: string }).keyDraft === "mod+shift+b", {
      keyDraft: "mod+shift+y",
    });
    renderPanel();

    (ui.find("Button", (props) => props.children === "Save").onClick as () => void)();
    expect(testState.upsertKeybinding).toHaveBeenCalledWith({
      environmentId: "environment-primary",
      input: {
        command: "sidebar.toggle",
        key: "mod+shift+y",
        replace: { command: "sidebar.toggle", key: "mod+shift+b" },
      },
    });
  });

  it("resets a customized binding back to its default", () => {
    setKeybindings([customTerminalSplit]);
    renderPanel();

    const reset = ui.find("MenuItem", (props) => props.children === "Reset to default");
    (reset.onClick as () => void)();
    expect(testState.upsertKeybinding).toHaveBeenCalledWith({
      environmentId: "environment-primary",
      input: {
        command: "terminal.split",
        key: "mod+d",
        when: "terminalFocus",
        replace: { command: "terminal.split", key: "mod+alt+d", when: "terminalFocus" },
      },
    });
  });

  it("omits an empty default when clause when resetting", () => {
    const customSidebar = binding(
      "sidebar.toggle",
      shortcut("b", { modKey: true, shiftKey: true }),
    );
    setKeybindings([customSidebar]);
    renderPanel();

    (ui.find("MenuItem", (props) => props.children === "Reset to default").onClick as () => void)();
    const input = testState.upsertKeybinding.mock.calls[0]![0] as {
      input: Record<string, unknown>;
    };
    expect(input.input).toEqual({
      command: "sidebar.toggle",
      key: "mod+b",
      replace: { command: "sidebar.toggle", key: "mod+shift+b" },
    });
  });

  it("hides row actions for pure default rows", () => {
    setKeybindings([defaultSidebarToggle]);
    renderPanel();
    expect(ui.filter("MenuItem")).toHaveLength(0);
  });

  it("removes a binding and reports failures", async () => {
    setKeybindings([customTerminalSplit]);
    renderPanel();
    (ui.find("MenuItem", (props) => props.children === "Remove").onClick as () => void)();
    expect(testState.removeKeybinding).toHaveBeenCalledWith({
      environmentId: "environment-primary",
      input: { command: "terminal.split", key: "mod+alt+d", when: "terminalFocus" },
    });
    await flushPromises();
    expect(testState.toastAdd).not.toHaveBeenCalled();

    testState.removeKeybinding.mockResolvedValue({
      _tag: "Failure",
      error: new Error("cannot remove"),
    });
    renderPanel();
    (ui.find("MenuItem", (props) => props.children === "Remove").onClick as () => void)();
    await flushPromises();
    expect(testState.toastAdd).toHaveBeenCalledWith({
      title: "Unable to remove keybinding",
      description: "cannot remove",
      type: "error",
    });
  });

  it("skips removal without a primary environment", () => {
    testState.primaryEnvironment = null;
    setKeybindings([customTerminalSplit]);
    renderPanel();
    (ui.find("MenuItem", (props) => props.children === "Remove").onClick as () => void)();
    expect(testState.removeKeybinding).not.toHaveBeenCalled();
  });
});

describe("opening the keybindings file", () => {
  it("opens the config file in the preferred editor", async () => {
    renderPanel();
    const open = ui.byLabel("Button", "Open keybindings.json");
    expect(open.disabled).toBe(false);
    (open.onClick as () => void)();
    await flushPromises();
    expect(testState.openInPreferredEditor).toHaveBeenCalledWith(
      "/home/user/.coddy/studio/keybindings.json",
    );
    expect(testState.toastAdd).not.toHaveBeenCalled();
  });

  it("reports failures when the file cannot be opened", async () => {
    testState.openInPreferredEditor.mockResolvedValue({
      _tag: "Failure",
      error: new Error("no editor"),
    });
    renderPanel();
    (ui.byLabel("Button", "Open keybindings.json").onClick as () => void)();
    await flushPromises();
    expect(testState.toastAdd).toHaveBeenCalledWith({
      title: "Unable to open keybindings file",
      description: "no editor",
      type: "error",
    });

    testState.toastAdd.mockReset();
    testState.openInPreferredEditor.mockResolvedValue({ _tag: "Failure", error: "nope" });
    renderPanel();
    (ui.byLabel("Button", "Open keybindings.json").onClick as () => void)();
    await flushPromises();
    expect(testState.toastAdd).toHaveBeenCalledWith({
      title: "Unable to open keybindings file",
      description: "The keybindings file was not opened.",
      type: "error",
    });
  });

  it("disables the action while the config path is unknown", () => {
    testState.atomValues.set(testState.atoms.configPath, null);
    renderPanel();
    const open = ui.byLabel("Button", "Open keybindings.json");
    expect(open.disabled).toBe(true);
    (open.onClick as () => void)();
    expect(testState.openInPreferredEditor).not.toHaveBeenCalled();
  });
});

describe("when expression builder", () => {
  it("propagates valid expression edits and flags invalid drafts", () => {
    setKeybindings([customTerminalSplit]);
    renderPanel();
    const input = ui.find("Input", (props) => props["aria-label"] === "When expression");
    expect(input.value).toBe("terminalFocus");

    harness.dispatchCalls.length = 0;
    (input.onChange as (event: unknown) => void)(changeEvent("terminalOpen"));
    expect(lastWhenDraftDispatch()).toEqual(identifier("terminalOpen"));
    expect(harness.dispatchCalls.some((call) => call.isWhenDraftValid === true)).toBe(true);

    harness.dispatchCalls.length = 0;
    (input.onChange as (event: unknown) => void)(changeEvent("terminalFocus &&"));
    expect(harness.dispatchCalls.some((call) => call.isWhenDraftValid === false)).toBe(true);
    expect(harness.dispatchCalls.every((call) => !("whenDraft" in call))).toBe(true);

    harness.dispatchCalls.length = 0;
    (input.onChange as (event: unknown) => void)(changeEvent("  "));
    expect(lastWhenDraftDispatch()).toBeUndefined();
  });

  it("renders the parse error state over the visual editor", () => {
    setKeybindings([customTerminalSplit]);
    harness.seedState((initial) => initial === "terminalFocus", "terminalFocus &&");
    const markup = renderPanel();
    expect(markup).toContain("Use variables with !, &amp;&amp;, ||, and parentheses.");
    expect(markup).toContain("Fix the expression above to continue editing visually.");
  });

  it("edits conditions inside a boolean group", () => {
    setKeybindings([projectScript]);
    renderPanel();

    // Negate the plain condition.
    harness.dispatchCalls.length = 0;
    const negateFocus = ui.byLabel("Toggle", "Negate terminalFocus");
    expect(negateFocus.pressed).toBe(false);
    (negateFocus.onPressedChange as (pressed: boolean) => void)(true);
    expect(lastWhenDraftDispatch()).toEqual(
      andNode(notNode(identifier("terminalFocus")), notNode(identifier("mysteryVar"))),
    );

    // Un-negate the negated condition.
    harness.dispatchCalls.length = 0;
    const negateMystery = ui.byLabel("Toggle", "Negate mysteryVar");
    expect(negateMystery.pressed).toBe(true);
    (negateMystery.onPressedChange as (pressed: boolean) => void)(false);
    expect(lastWhenDraftDispatch()).toEqual(
      andNode(identifier("terminalFocus"), identifier("mysteryVar")),
    );

    // Swap the identifier through the variable select.
    harness.dispatchCalls.length = 0;
    const variableSelect = ui.find("Select", (props) => props.value === "terminalFocus");
    (variableSelect.onValueChange as (value: string) => void)("terminalOpen");
    expect(lastWhenDraftDispatch()).toEqual(
      andNode(identifier("terminalOpen"), notNode(identifier("mysteryVar"))),
    );
    // Empty values are ignored.
    harness.dispatchCalls.length = 0;
    (variableSelect.onValueChange as (value: string) => void)("");
    expect(harness.dispatchCalls).toHaveLength(0);

    // Remove the first condition; a single child collapses the group.
    harness.dispatchCalls.length = 0;
    const removeButtons = ui.filter(
      "Button",
      (props) => props["aria-label"] === "Remove condition",
    );
    expect(removeButtons.length).toBe(2);
    (removeButtons[0]!.onClick as () => void)();
    expect(lastWhenDraftDispatch()).toEqual(notNode(identifier("mysteryVar")));

    // Change the boolean operator.
    harness.dispatchCalls.length = 0;
    const operatorSelect = ui.find("Select", (props) => props.value === "and");
    (operatorSelect.onValueChange as (value: string) => void)("or");
    expect(lastWhenDraftDispatch()).toEqual(
      orNode(identifier("terminalFocus"), notNode(identifier("mysteryVar"))),
    );
    harness.dispatchCalls.length = 0;
    (operatorSelect.onValueChange as (value: string) => void)("and");
    expect(harness.dispatchCalls).toHaveLength(0);

    // Add a condition and a nested group to the boolean group.
    const conditionButtons = ui.filter("Button", (props) => {
      const children = props.children;
      return Array.isArray(children) && children.includes("Condition");
    });
    const groupButtons = ui.filter("Button", (props) => {
      const children = props.children;
      return Array.isArray(children) && children.includes("Group");
    });
    expect(conditionButtons.length).toBe(2);
    expect(groupButtons.length).toBe(2);

    harness.dispatchCalls.length = 0;
    (conditionButtons[1]!.onClick as () => void)();
    expect(lastWhenDraftDispatch()).toEqual(
      andNode(
        andNode(identifier("terminalFocus"), notNode(identifier("mysteryVar"))),
        identifier("terminalFocus"),
      ),
    );

    harness.dispatchCalls.length = 0;
    (groupButtons[1]!.onClick as () => void)();
    expect(lastWhenDraftDispatch()).toEqual(
      andNode(
        andNode(identifier("terminalFocus"), notNode(identifier("mysteryVar"))),
        orNode(identifier("terminalFocus"), notNode(identifier("terminalFocus"))),
      ),
    );

    // Root-level additions wrap the existing expression in an and-node.
    harness.dispatchCalls.length = 0;
    (conditionButtons[0]!.onClick as () => void)();
    expect(lastWhenDraftDispatch()).toEqual(
      andNode(
        andNode(identifier("terminalFocus"), notNode(identifier("mysteryVar"))),
        identifier("terminalFocus"),
      ),
    );

    harness.dispatchCalls.length = 0;
    (groupButtons[0]!.onClick as () => void)();
    expect(lastWhenDraftDispatch()).toEqual(
      andNode(
        andNode(identifier("terminalFocus"), notNode(identifier("mysteryVar"))),
        orNode(identifier("terminalFocus"), notNode(identifier("terminalFocus"))),
      ),
    );

    // Removing the whole group clears the expression.
    harness.dispatchCalls.length = 0;
    const removeGroup = ui.byLabel("Button", "Remove group");
    (removeGroup.onClick as () => void)();
    expect(lastWhenDraftDispatch()).toBeUndefined();
  });

  it("edits a negated group", () => {
    setKeybindings([
      binding(
        "terminal.split",
        shortcut("d", { modKey: true, altKey: true }),
        notNode(andNode(identifier("terminalFocus"), identifier("terminalOpen"))),
      ),
    ]);
    const markup = renderPanel();
    expect(markup).toContain('aria-label="Negate group"');

    // Un-negating the group unwraps its child.
    harness.dispatchCalls.length = 0;
    const negateGroup = ui.byLabel("Toggle", "Negate group");
    (negateGroup.onPressedChange as (pressed: boolean) => void)(false);
    expect(lastWhenDraftDispatch()).toEqual(
      andNode(identifier("terminalFocus"), identifier("terminalOpen")),
    );

    // Edits inside the negated group re-wrap in the not-node.
    harness.dispatchCalls.length = 0;
    const negateFocus = ui.byLabel("Toggle", "Negate terminalFocus");
    (negateFocus.onPressedChange as (pressed: boolean) => void)(true);
    expect(lastWhenDraftDispatch()).toEqual(
      notNode(andNode(notNode(identifier("terminalFocus")), identifier("terminalOpen"))),
    );

    // The negated group can be removed entirely (root-level remove).
    harness.dispatchCalls.length = 0;
    const removeNegated = ui.byLabel("Button", "Remove negated group");
    (removeNegated.onClick as () => void)();
    expect(lastWhenDraftDispatch()).toBeUndefined();
  });

  it("starts an expression from the empty state", () => {
    const customSidebar = binding(
      "sidebar.toggle",
      shortcut("b", { modKey: true, shiftKey: true }),
    );
    setKeybindings([customSidebar]);
    const markup = renderPanel();
    expect(markup).toContain("Always");

    const conditionButtons = ui.filter("Button", (props) => {
      const children = props.children;
      return Array.isArray(children) && children.includes("Condition");
    });
    const groupButtons = ui.filter("Button", (props) => {
      const children = props.children;
      return Array.isArray(children) && children.includes("Group");
    });
    // Header pair + empty-state pair.
    expect(conditionButtons.length).toBe(2);
    expect(groupButtons.length).toBe(2);

    harness.dispatchCalls.length = 0;
    (conditionButtons[1]!.onClick as () => void)();
    expect(lastWhenDraftDispatch()).toEqual(identifier("terminalFocus"));

    harness.dispatchCalls.length = 0;
    (groupButtons[1]!.onClick as () => void)();
    expect(lastWhenDraftDispatch()).toEqual(
      orNode(identifier("terminalFocus"), notNode(identifier("terminalFocus"))),
    );
  });
});
