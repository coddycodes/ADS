/**
 * Unit tests for ProjectScriptsControl.
 *
 * The control is a stateful dialog-driven component, so it uses the repo's
 * instrumented-hooks pattern (see ChatView.hooks.test.tsx / Sidebar.test.tsx):
 * a partial `vi.mock("react")` seeds `useState` by call index (guarded by an
 * expected-initial check so hook-order drift fails loudly), and records setter
 * calls. Every leaf UI primitive is a
 * capture-mock that records its props during a `renderToStaticMarkup` pass;
 * tests then look up host/handler props (menu items, inputs, the form, the icon
 * grid) and invoke them directly with fake events.
 */
import type { ProjectScript, ResolvedKeybindingsConfig } from "@coddy/contracts";
import { renderToStaticMarkup } from "react-dom/server";
import * as React from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import type {
  NewProjectScriptInput,
  ProjectScriptActionResult,
  ProjectScriptsController,
} from "./ProjectScriptsControl";

const h = vi.hoisted(() => {
  const state = {
    React: null as unknown,
    captures: [] as Array<{ name: string; props: Record<string, unknown> }>,
    // react instrumentation
    stateCalls: [] as Array<{ index: number; initial: unknown }>,
    stateSeeds: new Map<number, { value: unknown; expectInitial: (value: unknown) => boolean }>(),
    setStateCalls: [] as Array<{ index: number; next: unknown; applied: unknown }>,
    // logic seams
    capturedKeybinding: null as string | null,
    keybindingValue: null as string | null,
    shortcutLabel: "Mod+T" as string | null,
    decodeThrows: false,
    interrupted: false,
  };
  return state;
});

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();

  const resolveInitial = (initial: unknown): unknown =>
    typeof initial === "function" ? (initial as () => unknown)() : initial;

  const useState = (initial?: unknown) => {
    const resolved = resolveInitial(initial);
    const index = h.stateCalls.length;
    h.stateCalls.push({ index, initial: resolved });
    const seed = h.stateSeeds.get(index);
    if (seed && !seed.expectInitial(resolved)) {
      throw new Error(
        `useState seed mismatch at index ${index}: initial ${String(resolved)} did not match the expected shape`,
      );
    }
    const value = seed ? seed.value : resolved;
    const setValue = (next: unknown) => {
      const applied =
        typeof next === "function" ? (next as (value: unknown) => unknown)(value) : next;
      h.setStateCalls.push({ index, next, applied });
    };
    return [value, setValue];
  };

  return {
    ...actual,
    useState: useState as typeof actual.useState,
  };
});

vi.mock("@coddy/client-runtime/state/runtime", () => ({
  isAtomCommandInterrupted: (_result: unknown) => h.interrupted,
  squashAtomCommandFailure: (result: { error?: unknown }) => result.error,
}));

vi.mock("~/lib/projectScriptKeybindings", () => ({
  decodeProjectScriptKeybindingRule: (input: { keybinding: string | null | undefined }) => {
    if (h.decodeThrows) throw new Error("Invalid keybinding.");
    const trimmed = input.keybinding?.trim() ?? "";
    return trimmed.length > 0 ? { key: trimmed } : null;
  },
  keybindingValueForCommand: () => h.keybindingValue,
}));

vi.mock("~/components/settings/KeybindingsSettings.logic", () => ({
  keybindingFromKeyboardEvent: () => h.capturedKeybinding,
}));

vi.mock("~/keybindings", () => ({
  shortcutLabelForCommand: () => h.shortcutLabel,
}));

// ── capture-mock factory for leaf UI primitives ──────────────────────────────

function makeMock(name: string, tag = "div") {
  const Comp = (props: Record<string, unknown>) => {
    h.captures.push({ name, props });
    const R = h.React as typeof import("react");
    const { children, render } = props as { children?: unknown; render?: unknown };
    const passthrough: Record<string, unknown> = { "data-mock": name };
    if (props["data-testid"] !== undefined) passthrough["data-testid"] = props["data-testid"];
    if (props["aria-label"] !== undefined) passthrough["aria-label"] = props["aria-label"];
    if (props["id"] !== undefined) passthrough["id"] = props["id"];
    if (render !== undefined && R.isValidElement(render)) {
      return children === undefined
        ? R.cloneElement(render as never, passthrough as never)
        : R.cloneElement(render as never, passthrough as never, children as never);
    }
    return R.createElement(tag, passthrough, children as never);
  };
  Comp.displayName = name;
  return Comp;
}

vi.mock("./ui/group", () => ({
  Group: makeMock("Group"),
  GroupSeparator: makeMock("GroupSeparator", "hr"),
}));
vi.mock("./ui/button", () => ({ Button: makeMock("Button", "button") }));
vi.mock("./ui/input", () => ({ Input: makeMock("Input", "input") }));
vi.mock("./ui/textarea", () => ({ Textarea: makeMock("Textarea", "textarea") }));
vi.mock("./ui/label", () => ({ Label: makeMock("Label", "label") }));
vi.mock("./ui/switch", () => ({ Switch: makeMock("Switch", "button") }));
vi.mock("./ui/tooltip", () => ({
  Tooltip: makeMock("Tooltip", "span"),
  TooltipPopup: makeMock("TooltipPopup", "span"),
  TooltipTrigger: makeMock("TooltipTrigger", "span"),
}));
vi.mock("./ui/menu", () => ({
  Menu: makeMock("Menu"),
  MenuItem: makeMock("MenuItem", "div"),
  MenuPopup: makeMock("MenuPopup"),
  MenuShortcut: makeMock("MenuShortcut", "span"),
  MenuTrigger: makeMock("MenuTrigger", "button"),
}));
vi.mock("./ui/popover", () => ({
  Popover: makeMock("Popover"),
  PopoverPopup: makeMock("PopoverPopup"),
  PopoverTrigger: makeMock("PopoverTrigger", "button"),
}));
vi.mock("./ui/dialog", () => ({
  Dialog: makeMock("Dialog"),
  DialogDescription: makeMock("DialogDescription"),
  DialogFooter: makeMock("DialogFooter"),
  DialogHeader: makeMock("DialogHeader"),
  DialogPanel: makeMock("DialogPanel"),
  DialogPopup: makeMock("DialogPopup"),
  DialogTitle: makeMock("DialogTitle"),
}));
vi.mock("./ui/alert-dialog", () => ({
  AlertDialog: makeMock("AlertDialog"),
  AlertDialogClose: makeMock("AlertDialogClose", "button"),
  AlertDialogDescription: makeMock("AlertDialogDescription"),
  AlertDialogFooter: makeMock("AlertDialogFooter"),
  AlertDialogHeader: makeMock("AlertDialogHeader"),
  AlertDialogPopup: makeMock("AlertDialogPopup"),
  AlertDialogTitle: makeMock("AlertDialogTitle"),
}));

import ProjectScriptsControl, {
  ProjectScriptsDialogs,
  ProjectScriptsExpandedActions,
  ProjectScriptsMenuItems,
  useProjectScriptsController,
} from "./ProjectScriptsControl";

type Props = Parameters<typeof ProjectScriptsControl>[0];

// ── state seeding (host useState call order) ──────────────────────────────────

const isNull = (value: unknown) => value === null;
const isFalse = (value: unknown) => value === false;
const isEmptyStr = (value: unknown) => value === "";
const isPlay = (value: unknown) => value === "play";

const STATE = {
  editingScriptId: { index: 0, expectInitial: isNull },
  dialogOpen: { index: 1, expectInitial: isFalse },
  name: { index: 2, expectInitial: isEmptyStr },
  command: { index: 3, expectInitial: isEmptyStr },
  icon: { index: 4, expectInitial: isPlay },
  iconPickerOpen: { index: 5, expectInitial: isFalse },
  runOnWorktreeCreate: { index: 6, expectInitial: isFalse },
  keybinding: { index: 7, expectInitial: isEmptyStr },
  previewUrl: { index: 8, expectInitial: isEmptyStr },
  autoOpenPreview: { index: 9, expectInitial: isFalse },
  validationError: { index: 10, expectInitial: isNull },
  deleteConfirmOpen: { index: 11, expectInitial: isFalse },
} as const;

function seed(name: keyof typeof STATE, value: unknown): void {
  const { index, expectInitial } = STATE[name];
  h.stateSeeds.set(index, { value, expectInitial });
}

function setCallsFor(name: keyof typeof STATE): Array<{ next: unknown; applied: unknown }> {
  const { index } = STATE[name];
  return h.setStateCalls.filter((call) => call.index === index);
}

function appliedValues(name: keyof typeof STATE): unknown[] {
  return setCallsFor(name).map((call) => call.applied);
}

// ── fixtures ──────────────────────────────────────────────────────────────────

function makeScript(overrides: Partial<ProjectScript> = {}): ProjectScript {
  return {
    id: "dev",
    name: "Dev",
    command: "pnpm dev",
    icon: "play",
    runOnWorktreeCreate: false,
    ...overrides,
  } as ProjectScript;
}

const successResult = { _tag: "Success", value: undefined } as unknown as ProjectScriptActionResult;
const failureResult = {
  _tag: "Failure",
  error: new Error("save failed"),
} as unknown as ProjectScriptActionResult;

const onRunScript = vi.fn<(script: ProjectScript) => void>();
const onAddScript = vi.fn<(input: NewProjectScriptInput) => Promise<ProjectScriptActionResult>>();
const onUpdateScript =
  vi.fn<(scriptId: string, input: NewProjectScriptInput) => Promise<ProjectScriptActionResult>>();
const onDeleteScript = vi.fn<(scriptId: string) => Promise<ProjectScriptActionResult>>();

function baseProps(overrides: Partial<Props> = {}): Props {
  return {
    scripts: [
      makeScript({ id: "dev", name: "Dev", icon: "play" }),
      makeScript({ id: "setup", name: "Setup", icon: "configure", runOnWorktreeCreate: true }),
      makeScript({ id: "test", name: "Test", icon: "test" }),
    ],
    keybindings: [] as ResolvedKeybindingsConfig,
    onRunScript,
    onAddScript,
    onUpdateScript,
    onDeleteScript,
    ...overrides,
  };
}

function renderControl(
  props: Props = baseProps(),
  seeds: Partial<Record<keyof typeof STATE, unknown>> = {},
): string {
  h.captures.length = 0;
  h.stateCalls.length = 0;
  h.setStateCalls.length = 0;
  h.stateSeeds.clear();
  for (const [name, value] of Object.entries(seeds)) {
    seed(name as keyof typeof STATE, value);
  }
  return renderToStaticMarkup(<ProjectScriptsControl {...props} />);
}

function renderControllerSurfaces(
  props: Props = baseProps(),
  seeds: Partial<Record<keyof typeof STATE, unknown>> = {},
): string {
  h.captures.length = 0;
  h.stateCalls.length = 0;
  h.setStateCalls.length = 0;
  h.stateSeeds.clear();
  for (const [name, value] of Object.entries(seeds)) {
    seed(name as keyof typeof STATE, value);
  }

  function ControllerSurfaces() {
    const controller = useProjectScriptsController(props);
    return (
      <>
        <ProjectScriptsExpandedActions controller={controller} />
        <ProjectScriptsMenuItems controller={controller} />
        <ProjectScriptsDialogs controller={controller} />
      </>
    );
  }

  return renderToStaticMarkup(<ControllerSurfaces />);
}

// ── capture lookup helpers ────────────────────────────────────────────────────

function byName(name: string): Array<Record<string, unknown>> {
  return h.captures.filter((entry) => entry.name === name).map((entry) => entry.props);
}

function collectElements(node: unknown, out: React.ReactElement[]): void {
  if (node === null || node === undefined || typeof node !== "object") return;
  if (Array.isArray(node)) {
    for (const child of node) collectElements(child, out);
    return;
  }
  if (React.isValidElement(node)) {
    out.push(node);
    const props = node.props as Record<string, unknown>;
    collectElements(props["children"], out);
    collectElements(props["render"], out);
  }
}

function allElementProps(): Array<Record<string, unknown>> {
  const result: Array<Record<string, unknown>> = [];
  for (const entry of h.captures) {
    result.push(entry.props);
    const els: React.ReactElement[] = [];
    collectElements(entry.props["children"], els);
    collectElements(entry.props["render"], els);
    for (const el of els) result.push(el.props as Record<string, unknown>);
  }
  return result;
}

function findProps(
  predicate: (props: Record<string, unknown>) => boolean,
): Record<string, unknown> | null {
  return allElementProps().find(predicate) ?? null;
}

function mustFind(
  predicate: (props: Record<string, unknown>) => boolean,
  label: string,
): Record<string, unknown> {
  const props = findProps(predicate);
  if (!props) throw new Error(`could not find element: ${label}`);
  return props;
}

function buttonByText(text: string): Record<string, unknown> {
  const found = byName("Button").find((props) => props["children"] === text);
  if (!found) throw new Error(`could not find Button with text ${text}`);
  return found;
}

function invoke(props: Record<string, unknown>, handler: string, event: unknown): void {
  const fn = props[handler];
  if (typeof fn !== "function") throw new Error(`no ${handler} handler`);
  (fn as (event: unknown) => void)(event);
}

function keyEvent(key: string): unknown {
  return { key, preventDefault: vi.fn(), stopPropagation: vi.fn() };
}

function submitEvent(): unknown {
  return { preventDefault: vi.fn() };
}

const flush = async () => {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
};

beforeEach(() => {
  h.React = React;
  h.captures.length = 0;
  h.stateCalls.length = 0;
  h.setStateCalls.length = 0;
  h.stateSeeds.clear();
  h.capturedKeybinding = null;
  h.keybindingValue = null;
  h.shortcutLabel = "Mod+T";
  h.decodeThrows = false;
  h.interrupted = false;
  onRunScript.mockReset();
  onAddScript.mockReset().mockResolvedValue(successResult);
  onUpdateScript.mockReset().mockResolvedValue(successResult);
  onDeleteScript.mockReset().mockResolvedValue(successResult);
  vi.stubGlobal("navigator", { platform: "MacIntel" });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

// ─────────────────────────────────────────────────────────────────────────────

describe("rendering", () => {
  it("renders the primary run button, script menu, and every script icon", () => {
    const markup = renderControl();
    // Primary is the first non-setup script.
    expect(markup).toContain("Dev");
    // Setup scripts get a "(setup)" suffix in the menu.
    expect(markup).toContain("Setup (setup)");
    expect(markup).toContain("Add action");
    // Add-dialog defaults to the create title.
    expect(markup).toContain("Add Action");
    // The icon grid renders every selectable icon (covers all ScriptIcon arms).
    const iconGrid = byName("PopoverPopup");
    expect(iconGrid.length).toBeGreaterThan(0);
  });

  it("renders nothing in the header slot when there are no scripts", () => {
    const markup = renderControl(baseProps({ scripts: [] }));
    // The primary group is gone but the dialog subtree still renders.
    expect(byName("Group")).toHaveLength(0);
    expect(byName("Dialog")).toHaveLength(1);
    expect(markup).toContain("Add Action");
  });

  it("prefers the explicitly preferred script for the primary slot", () => {
    renderControl(baseProps({ preferredScriptId: "test" }));
    const runButton = mustFind((props) => props["aria-label"] === "Run Test", "run test button");
    expect(runButton).toBeDefined();
  });

  it("falls back to the computed primary when the preferred id is unknown", () => {
    renderControl(baseProps({ preferredScriptId: "missing" }));
    const runButton = mustFind((props) => props["aria-label"] === "Run Dev", "run dev button");
    expect(runButton).toBeDefined();
  });

  it("omits the shortcut label when none is configured", () => {
    h.shortcutLabel = null;
    renderControl();
    expect(byName("MenuShortcut")).toHaveLength(0);
  });
});

describe("shared controller presentations", () => {
  it("routes expanded primary and dropdown actions through the controller", () => {
    const scripts = [
      makeScript({ id: "build", name: "Build", icon: "build" }),
      makeScript({ id: "test", name: "Test", icon: "test" }),
    ];
    const runScript = vi.fn<(script: ProjectScript) => void>();
    const openEditDialog = vi.fn<(script: ProjectScript) => void>();
    const controller = {
      scripts,
      primaryScript: scripts[0]!,
      disabledReason: null,
      openAddDialog: vi.fn(),
      openEditDialog,
      runScript,
    } satisfies ProjectScriptsController;

    renderToStaticMarkup(<ProjectScriptsExpandedActions controller={controller} />);
    invoke(
      mustFind((props) => props["aria-label"] === "Run Build", "run Build"),
      "onClick",
      {},
    );
    invoke(
      mustFind((props) => props["aria-label"] === "Edit Test", "edit Test"),
      "onClick",
      {
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      },
    );

    expect(runScript).toHaveBeenCalledWith(scripts[0]);
    expect(openEditDialog).toHaveBeenCalledWith(scripts[1]);
  });

  it("renders every script, shortcut, edit action, and Add action as bare menu items", () => {
    const scripts = [
      makeScript({ id: "build", name: "Build", icon: "build" }),
      makeScript({ id: "test", name: "Test", icon: "test" }),
    ];
    const controller = {
      scripts,
      primaryScript: scripts[0]!,
      disabledReason: null,
      openAddDialog: vi.fn(),
      openEditDialog: vi.fn(),
      runScript: vi.fn(),
    } satisfies ProjectScriptsController;

    const markup = renderToStaticMarkup(<ProjectScriptsMenuItems controller={controller} />);

    expect(byName("Menu")).toHaveLength(0);
    expect(byName("MenuPopup")).toHaveLength(0);
    expect(byName("MenuItem")).toHaveLength(3);
    expect(byName("MenuShortcut")).toHaveLength(2);
    expect(markup).toContain("Build");
    expect(markup).toContain("Test");
    expect(markup).toContain("Add action");
    expect(mustFind((props) => props["aria-label"] === "Edit Build", "edit Build")).toBeDefined();
    expect(mustFind((props) => props["aria-label"] === "Edit Test", "edit Test")).toBeDefined();
  });

  it("shares edit dialog state between menu items and dialogs", () => {
    const props = baseProps({
      scripts: [makeScript({ id: "build", name: "Build", command: "vp build", icon: "build" })],
    });
    renderControllerSurfaces(props);

    const editBuild = mustFind((entry) => entry["aria-label"] === "Edit Build", "edit Build");
    invoke(editBuild, "onClick", {
      preventDefault: vi.fn(),
      stopPropagation: vi.fn(),
    });
    expect(appliedValues("editingScriptId")).toContain("build");
    expect(appliedValues("dialogOpen")).toContain(true);

    renderControllerSurfaces(props, {
      editingScriptId: "build",
      dialogOpen: true,
      name: "Build",
      command: "vp build",
      icon: "build",
    });
    expect(byName("Dialog")[0]?.["open"]).toBe(true);
    expect(byName("DialogTitle")[0]?.["children"]).toBe("Edit Action");
    expect(mustFind((entry) => entry["id"] === "script-name", "script name")["value"]).toBe(
      "Build",
    );
  });

  it("shares add dialog state between the controller action and dialogs", () => {
    const props = baseProps();

    renderControllerSurfaces(props);
    const addAction = byName("MenuItem").find((entry) => {
      const children = entry["children"];
      return Array.isArray(children) && children.includes("Add action");
    });
    if (!addAction) throw new Error("could not find Add action menu item");
    invoke(addAction, "onClick", {});
    expect(appliedValues("editingScriptId")).toContain(null);
    expect(appliedValues("dialogOpen")).toContain(true);

    renderControllerSurfaces(props, { dialogOpen: true });
    expect(byName("Dialog")[0]?.["open"]).toBe(true);
    expect(byName("DialogTitle")[0]?.["children"]).toBe("Add Action");
  });
});

describe("running scripts", () => {
  it("runs the primary script from the run button", () => {
    const props = baseProps();
    renderControl(props);
    const runButton = mustFind((p) => p["aria-label"] === "Run Dev", "run button");
    invoke(runButton, "onClick", {});
    expect(onRunScript).toHaveBeenCalledWith(props.scripts[0]);
  });

  it("runs a script from its menu entry", () => {
    const props = baseProps();
    renderControl(props);
    const menuItem = byName("MenuItem").find((p) => typeof p["onClick"] === "function")!;
    invoke(menuItem, "onClick", {});
    expect(onRunScript).toHaveBeenCalledTimes(1);
  });
});

describe("dialog open/close handlers", () => {
  it("opens the add dialog from the menu 'Add action' entry", () => {
    renderControl();
    const addItem = mustFind(
      (props) =>
        typeof props["onClick"] === "function" &&
        Array.isArray(props["children"]) &&
        (props["children"] as unknown[]).includes("Add action"),
      "add action menu item",
    );
    invoke(addItem, "onClick", {});
    expect(appliedValues("dialogOpen")).toContain(true);
    expect(appliedValues("editingScriptId")).toContain(null);
    expect(appliedValues("icon")).toContain("play");
  });

  it("opens the edit dialog from a script's settings button", () => {
    const props = baseProps();
    h.keybindingValue = "mod+d";
    renderControl(props);
    const editButton = mustFind((p) => p["aria-label"] === "Edit Dev", "edit dev button");
    const pointer = { preventDefault: vi.fn(), stopPropagation: vi.fn() };
    invoke(editButton, "onPointerDown", pointer);
    expect(pointer.preventDefault).toHaveBeenCalled();
    expect(pointer.stopPropagation).toHaveBeenCalled();

    const click = { preventDefault: vi.fn(), stopPropagation: vi.fn() };
    invoke(editButton, "onClick", click);
    expect(appliedValues("editingScriptId")).toContain("dev");
    expect(appliedValues("name")).toContain("Dev");
    expect(appliedValues("keybinding")).toContain("mod+d");
    expect(appliedValues("dialogOpen")).toContain(true);
  });

  it("routes the Dialog open state and resets the icon picker on close", () => {
    renderControl();
    const dialog = byName("Dialog")[0]!;
    invoke(dialog, "onOpenChange", true);
    expect(appliedValues("dialogOpen")).toContain(true);

    invoke(dialog, "onOpenChange", false);
    expect(appliedValues("dialogOpen")).toContain(false);
    expect(appliedValues("iconPickerOpen")).toContain(false);
  });

  it("resets the form after the close transition completes", () => {
    renderControl(baseProps(), { editingScriptId: "dev" });
    const dialog = byName("Dialog")[0]!;

    // Opening completion is a no-op.
    invoke(dialog, "onOpenChangeComplete", true);
    expect(h.setStateCalls).toHaveLength(0);

    invoke(dialog, "onOpenChangeComplete", false);
    expect(appliedValues("editingScriptId")).toContain(null);
    expect(appliedValues("name")).toContain("");
    expect(appliedValues("icon")).toContain("play");
    expect(appliedValues("validationError")).toContain(null);
  });

  it("closes the dialog from the Cancel button", () => {
    renderControl();
    const cancel = buttonByText("Cancel");
    invoke(cancel, "onClick", {});
    expect(appliedValues("dialogOpen")).toContain(false);
  });
});

describe("keybinding capture", () => {
  function keybindingInput(): Record<string, unknown> {
    return mustFind((props) => props["id"] === "script-keybinding", "keybinding input");
  }

  it("ignores Tab so focus can move on", () => {
    renderControl();
    invoke(keybindingInput(), "onKeyDown", keyEvent("Tab"));
    expect(setCallsFor("keybinding")).toHaveLength(0);
  });

  it("clears the binding on Backspace/Delete", () => {
    renderControl();
    const input = keybindingInput();
    invoke(input, "onKeyDown", keyEvent("Backspace"));
    invoke(input, "onKeyDown", keyEvent("Delete"));
    expect(appliedValues("keybinding")).toEqual(["", ""]);
  });

  it("records a captured shortcut and ignores keys that decode to nothing", () => {
    renderControl();
    const input = keybindingInput();

    h.capturedKeybinding = "mod+k";
    invoke(input, "onKeyDown", keyEvent("k"));
    expect(appliedValues("keybinding")).toContain("mod+k");

    h.capturedKeybinding = null;
    invoke(input, "onKeyDown", keyEvent("q"));
    // No additional keybinding update for an unmapped key.
    expect(appliedValues("keybinding")).toEqual(["mod+k"]);
  });
});

describe("submitAddScript", () => {
  function formProps(): Record<string, unknown> {
    return mustFind((props) => typeof props["onSubmit"] === "function", "add-script form");
  }

  it("requires a name", async () => {
    renderControl(baseProps(), { name: "  ", command: "echo" });
    await (formProps().onSubmit as (event: unknown) => Promise<void>)(submitEvent());
    expect(appliedValues("validationError")).toContain("Name is required.");
    expect(onAddScript).not.toHaveBeenCalled();
  });

  it("requires a command", async () => {
    renderControl(baseProps(), { name: "Builder", command: "  " });
    await (formProps().onSubmit as (event: unknown) => Promise<void>)(submitEvent());
    expect(appliedValues("validationError")).toContain("Command is required.");
    expect(onAddScript).not.toHaveBeenCalled();
  });

  it("surfaces a keybinding decode error", async () => {
    h.decodeThrows = true;
    renderControl(baseProps(), { name: "Builder", command: "make", keybinding: "??" });
    await (formProps().onSubmit as (event: unknown) => Promise<void>)(submitEvent());
    expect(appliedValues("validationError")).toContain("Invalid keybinding.");
    expect(onAddScript).not.toHaveBeenCalled();
  });

  it("creates a script and closes the dialog on success", async () => {
    renderControl(baseProps(), {
      name: "  Builder  ",
      command: "  make  ",
      keybinding: "mod+b",
      previewUrl: "  https://localhost:3000  ",
      autoOpenPreview: true,
    });
    await (formProps().onSubmit as (event: unknown) => Promise<void>)(submitEvent());

    expect(onAddScript).toHaveBeenCalledTimes(1);
    const payload = onAddScript.mock.calls[0]![0];
    expect(payload).toMatchObject({
      name: "Builder",
      command: "make",
      icon: "play",
      keybinding: "mod+b",
      previewUrl: "https://localhost:3000",
      autoOpenPreview: true,
    });
    expect(appliedValues("dialogOpen")).toContain(false);
    expect(appliedValues("iconPickerOpen")).toContain(false);
  });

  it("drops auto-open when no preview url is provided", async () => {
    renderControl(baseProps(), {
      name: "Builder",
      command: "make",
      previewUrl: "   ",
      autoOpenPreview: true,
    });
    await (formProps().onSubmit as (event: unknown) => Promise<void>)(submitEvent());
    const payload = onAddScript.mock.calls[0]![0];
    expect(payload.previewUrl).toBeNull();
    expect(payload.autoOpenPreview).toBe(false);
  });

  it("updates an existing script when editing", async () => {
    renderControl(baseProps(), {
      editingScriptId: "dev",
      name: "Dev v2",
      command: "pnpm dev",
    });
    await (formProps().onSubmit as (event: unknown) => Promise<void>)(submitEvent());
    expect(onUpdateScript).toHaveBeenCalledTimes(1);
    expect(onUpdateScript.mock.calls[0]![0]).toBe("dev");
    expect(onAddScript).not.toHaveBeenCalled();
  });

  it("shows the failure message when saving fails", async () => {
    onAddScript.mockResolvedValue(failureResult);
    renderControl(baseProps(), { name: "Builder", command: "make" });
    await (formProps().onSubmit as (event: unknown) => Promise<void>)(submitEvent());
    await flush();
    expect(appliedValues("validationError")).toContain("save failed");
    expect(appliedValues("dialogOpen")).not.toContain(false);
  });

  it("stays silent when the save is interrupted", async () => {
    onAddScript.mockResolvedValue(failureResult);
    h.interrupted = true;
    renderControl(baseProps(), { name: "Builder", command: "make" });
    await (formProps().onSubmit as (event: unknown) => Promise<void>)(submitEvent());
    await flush();
    // The first validation reset happens, but no failure message is set.
    expect(appliedValues("validationError")).not.toContain("save failed");
    expect(appliedValues("dialogOpen")).not.toContain(false);
  });
});

describe("form field handlers", () => {
  it("updates the name, command, and preview url fields", () => {
    renderControl();
    const name = mustFind((p) => p["id"] === "script-name", "name input");
    invoke(name, "onChange", { target: { value: "New name" } });
    expect(appliedValues("name")).toContain("New name");

    const command = mustFind((p) => p["id"] === "script-command", "command textarea");
    invoke(command, "onChange", { target: { value: "run it" } });
    expect(appliedValues("command")).toContain("run it");

    const preview = mustFind((p) => p["id"] === "script-preview-url", "preview input");
    invoke(preview, "onChange", { target: { value: "http://x" } });
    expect(appliedValues("previewUrl")).toContain("http://x");
  });

  it("toggles the worktree-create and auto-open switches", () => {
    renderControl();
    const switches = byName("Switch");
    expect(switches.length).toBe(2);
    invoke(switches[0]!, "onCheckedChange", true);
    expect(appliedValues("runOnWorktreeCreate")).toContain(true);
    invoke(switches[1]!, "onCheckedChange", 0);
    expect(appliedValues("autoOpenPreview")).toContain(false);
  });

  it("selects an icon from the picker and closes it", () => {
    renderControl();
    const iconButton = mustFind(
      (props) =>
        typeof props["onClick"] === "function" &&
        typeof props["className"] === "string" &&
        (props["className"] as string).includes("flex-col"),
      "icon grid button",
    );
    invoke(iconButton, "onClick", {});
    expect(appliedValues("icon").length).toBeGreaterThan(0);
    expect(appliedValues("iconPickerOpen")).toContain(false);
  });

  it("routes the icon picker open state", () => {
    renderControl();
    const popover = byName("Popover")[0]!;
    invoke(popover, "onOpenChange", true);
    expect(appliedValues("iconPickerOpen")).toContain(true);
  });
});

describe("delete flow", () => {
  it("opens the delete confirmation from the editing footer", () => {
    renderControl(baseProps(), { editingScriptId: "dev" });
    const deleteButton = buttonByText("Delete");
    invoke(deleteButton, "onClick", {});
    expect(appliedValues("deleteConfirmOpen")).toContain(true);
  });

  it("confirms and deletes the editing script", () => {
    renderControl(baseProps(), { editingScriptId: "dev" });
    const confirm = buttonByText("Delete action");
    invoke(confirm, "onClick", {});
    expect(appliedValues("deleteConfirmOpen")).toContain(false);
    expect(appliedValues("dialogOpen")).toContain(false);
    expect(onDeleteScript).toHaveBeenCalledWith("dev");
  });

  it("does nothing when confirming without an editing script", () => {
    renderControl();
    const confirm = buttonByText("Delete action");
    invoke(confirm, "onClick", {});
    expect(onDeleteScript).not.toHaveBeenCalled();
  });

  it("routes the alert dialog open state", () => {
    renderControl();
    const alert = byName("AlertDialog")[0]!;
    invoke(alert, "onOpenChange", true);
    expect(appliedValues("deleteConfirmOpen")).toContain(true);
  });
});
