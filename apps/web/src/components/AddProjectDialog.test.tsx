// @vitest-environment happy-dom

import { EnvironmentId } from "@coddy/contracts";
import { act, type ReactElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterAll, afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import type { AddProjectWorkflow } from "./add-project/useAddProjectWorkflow";

type MutableAddProjectWorkflow = {
  -readonly [Key in keyof AddProjectWorkflow]: AddProjectWorkflow[Key];
};

const testState = vi.hoisted(() => ({
  workflow: null as unknown as MutableAddProjectWorkflow,
}));

vi.mock("./add-project/useAddProjectWorkflow", async () => {
  const React = await vi.importActual<typeof import("react")>("react");
  return {
    useAddProjectWorkflow: () => {
      const [step, setStep] = React.useState(testState.workflow.step);
      return {
        ...testState.workflow,
        step,
        back: () => {
          testState.workflow.back();
          setStep("start");
        },
        openClone: () => {
          testState.workflow.openClone();
          setStep("clone");
        },
        openCreate: () => {
          testState.workflow.openCreate();
          setStep("create");
        },
      };
    },
  };
});

vi.mock("./RemoteDirectoryBrowser", () => ({
  RemoteDirectoryBrowser: (props: {
    readonly environmentId: string;
    readonly initialPath: string;
    readonly secondaryAction?: { readonly label: string; readonly onClick: () => void };
    readonly selectLabel?: string;
    readonly onSelect: (path: string) => void;
    readonly onCancel?: () => void;
  }) => (
    <div>
      <p>
        Browsing {props.environmentId}: {props.initialPath}
      </p>
      {props.onCancel ? (
        <button type="button" onClick={props.onCancel}>
          Cancel
        </button>
      ) : null}
      {props.secondaryAction ? (
        <button type="button" onClick={props.secondaryAction.onClick}>
          {props.secondaryAction.label}
        </button>
      ) : null}
      <button type="button" onClick={() => props.onSelect("/srv/code/demo")}>
        {props.selectLabel ?? "Select folder"}
      </button>
    </div>
  ),
}));

import { AddProjectDialog } from "./AddProjectDialog";

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

async function mount(element: ReactElement): Promise<void> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  mountedTrees.push({ container, root });
  await act(async () => root.render(element));
}

async function click(element: HTMLElement): Promise<void> {
  await act(async () => element.click());
}

async function pressEscape(): Promise<void> {
  await act(async () => {
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  });
}

function buttonWithText(text: string): HTMLButtonElement {
  const button = Array.from(document.querySelectorAll<HTMLButtonElement>("button")).find((entry) =>
    entry.textContent?.includes(text),
  );
  expect(button).toBeDefined();
  return button!;
}

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

  const environmentId = EnvironmentId.make("local-test");
  const selectedHost = {
    environmentId,
    label: "This device",
    platform: "MacIntel",
    baseDirectory: "~/",
    isPrimary: true,
    desktopInstanceId: null,
    nativePickerAvailable: true,
  } as const;
  testState.workflow = {
    hosts: [selectedHost],
    locationLabel: null,
    selectedHost,
    step: "start",
    busy: false,
    dismissible: true,
    cloneProgress: "idle",
    hostPath: "~/",
    cloneUrl: "",
    cloneParent: "~/",
    createName: "",
    createParent: "~/",
    error: null,
    notice: null,
    selectHost: vi.fn(),
    back: vi.fn(),
    browse: vi.fn(async () => {}),
    setHostPath: vi.fn(),
    submitHostPath: vi.fn(async () => {}),
    openHostPath: vi.fn(),
    selectBrowsedFolder: vi.fn(async () => {}),
    openClone: vi.fn(),
    setCloneUrl: vi.fn(),
    setCloneParent: vi.fn(),
    pickCloneParent: vi.fn(async () => {}),
    submitClone: vi.fn(async () => {}),
    cancelClone: vi.fn(),
    openCreate: vi.fn(),
    setCreateName: vi.fn(),
    setCreateParent: vi.fn(),
    pickCreateParent: vi.fn(async () => {}),
    submitCreate: vi.fn(async () => {}),
  };
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
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

afterAll(() => {
  expect(Object.getOwnPropertyDescriptor(Element.prototype, "getAnimations")).toEqual(
    suiteGetAnimationsDescriptor,
  );
});

describe("AddProjectDialog mounted interactions", () => {
  it("offers Browse on a remote clone form without native picking", async () => {
    testState.workflow.step = "clone";
    testState.workflow.selectedHost = {
      ...testState.workflow.selectedHost,
      environmentId: EnvironmentId.make("remote-selected"),
      label: "Build server",
      isPrimary: false,
    };
    await mount(<AddProjectDialog open onOpenChange={vi.fn()} />);

    await click(buttonWithText("Browse…"));
    expect(testState.workflow.pickCloneParent).toHaveBeenCalledTimes(1);
    expect(document.querySelector<HTMLInputElement>("#add-project-clone-parent")).not.toBeNull();
  });

  it("offers Browse on a remote create form and browses that server", async () => {
    testState.workflow.step = "create";
    testState.workflow.selectedHost = {
      ...testState.workflow.selectedHost,
      environmentId: EnvironmentId.make("remote-selected"),
      label: "Build server",
      isPrimary: false,
      nativePickerAvailable: false,
    };
    await mount(<AddProjectDialog open onOpenChange={vi.fn()} />);

    await click(buttonWithText("Git repository in"));
    await click(buttonWithText("Browse…"));
    expect(testState.workflow.pickCreateParent).toHaveBeenCalledTimes(1);
  });

  it("identifies the server and starts the parent browser at the entered create path", async () => {
    testState.workflow.step = "create-parent-browse";
    testState.workflow.createParent = "/srv/projects";
    testState.workflow.selectedHost = {
      ...testState.workflow.selectedHost,
      environmentId: EnvironmentId.make("remote-selected"),
      label: "Build server",
      isPrimary: false,
    };
    await mount(<AddProjectDialog open onOpenChange={vi.fn()} />);

    expect(document.body.textContent).toContain("Choose parent folder on Build server");
    expect(document.body.textContent).toContain("The new project folder will be created inside");
    expect(document.body.textContent).toContain("Browsing remote-selected: /srv/projects");
  });

  it("identifies the server and starts the parent browser at the entered clone path", async () => {
    testState.workflow.step = "clone-parent-browse";
    testState.workflow.cloneParent = "/srv/entered";
    testState.workflow.selectedHost = {
      ...testState.workflow.selectedHost,
      environmentId: EnvironmentId.make("remote-selected"),
      label: "Build server",
      isPrimary: false,
    };
    await mount(<AddProjectDialog open onOpenChange={vi.fn()} />);

    expect(document.body.textContent).toContain("Choose parent folder on Build server");
    expect(document.body.textContent).toContain("Browsing remote-selected: /srv/entered");
    await click(buttonWithText("Cancel"));
    expect(testState.workflow.back).toHaveBeenCalledTimes(1);
  });

  it("renders the start step and opens clone and create steps", async () => {
    await mount(<AddProjectDialog open onOpenChange={vi.fn()} />);
    expect(document.body.textContent).toContain("Add a project");

    await click(buttonWithText("Clone from URL"));
    expect(document.body.textContent).toContain("Enter the Git URL and choose where to clone it.");
    await click(buttonWithText("Back"));

    await click(buttonWithText("Create new project"));
    expect(document.body.textContent).toContain(
      "Name it and Coddy Studio will create a real project with sensible defaults.",
    );
  });

  it("names and describes the dialog and keeps step content padded and scrollable", async () => {
    await mount(<AddProjectDialog open onOpenChange={vi.fn()} />);

    const dialog = document.querySelector('[role="dialog"]');
    if (!(dialog instanceof HTMLElement)) throw new Error("Missing add project dialog");
    const labelledBy = dialog.getAttribute("aria-labelledby");
    const describedBy = dialog.getAttribute("aria-describedby");
    expect(labelledBy).not.toBeNull();
    expect(describedBy).not.toBeNull();
    expect(document.getElementById(labelledBy!)?.textContent).toBe("Add a project");
    const description = document.getElementById(describedBy!);
    expect(description?.textContent).toBe("Choose how to add a project.");
    expect(description?.textContent).not.toMatch(/host/i);

    const content = dialog.querySelector('[data-add-project-content="true"]');
    expect(content?.textContent).not.toMatch(/host/i);
    expect(content?.classList.contains("overflow-y-auto")).toBe(true);
    expect(content?.classList.contains("px-6")).toBe(true);
  });

  it("never renders nested repository import UI", async () => {
    await mount(<AddProjectDialog open onOpenChange={vi.fn()} />);
    expect(document.body.textContent).not.toContain("Repositories found");
    expect(document.body.textContent).not.toContain("Import selected");
    expect(document.querySelector('input[type="checkbox"]')).toBeNull();
  });

  it("does not render a redundant host selector for one desktop location", async () => {
    await mount(<AddProjectDialog open onOpenChange={vi.fn()} />);

    expect(document.querySelector('[role="combobox"]')).toBeNull();
    expect(document.body.textContent).not.toContain("Host");
    expect(document.body.textContent).not.toContain("Location");
  });

  it("keeps the clone form open with Cancel wired to the workflow while cloning", async () => {
    testState.workflow.step = "clone";
    testState.workflow.cloneUrl = "https://example.test/demo.git";
    testState.workflow.busy = true;
    testState.workflow.dismissible = false;
    testState.workflow.cloneProgress = "cloning";
    const onOpenChange = vi.fn();
    await mount(<AddProjectDialog open onOpenChange={onOpenChange} />);

    const urlInput = document.querySelector<HTMLInputElement>("#add-project-clone-url");
    expect(urlInput?.value).toBe("https://example.test/demo.git");
    expect(buttonWithText("Cloning…").disabled).toBe(true);
    expect(buttonWithText("Back").disabled).toBe(true);
    await pressEscape();
    expect(onOpenChange).not.toHaveBeenCalledWith(false);

    await click(buttonWithText("Cancel clone"));
    expect(testState.workflow.cancelClone).toHaveBeenCalledTimes(1);
  });

  it("shows a cancelled clone notice on the clone form", async () => {
    testState.workflow.step = "clone";
    testState.workflow.cloneUrl = "https://example.test/demo.git";
    testState.workflow.notice = "Clone cancelled.";
    await mount(<AddProjectDialog open onOpenChange={vi.fn()} />);

    expect(document.querySelector('[role="status"]')?.textContent).toBe("Clone cancelled.");
    expect(buttonWithText("Clone").disabled).toBe(false);
  });

  it("prevents dismissal while a mutation is pending", async () => {
    testState.workflow.busy = true;
    testState.workflow.dismissible = false;
    const onOpenChange = vi.fn();
    await mount(<AddProjectDialog open onOpenChange={onOpenChange} />);
    await pressEscape();
    expect(onOpenChange).not.toHaveBeenCalledWith(false);
  });

  it.each([
    [
      "reconnecting",
      "Lost the connection to Remote. The clone continues on the server; reconnecting…",
    ],
    ["cancelling", "The clone stops when Remote reconnects."],
  ] as const)("offers Close while the clone waits for the host (%s)", async (progress, notice) => {
    testState.workflow.step = "clone";
    testState.workflow.cloneUrl = "https://example.test/demo.git";
    testState.workflow.busy = true;
    testState.workflow.dismissible = true;
    testState.workflow.cloneProgress = progress;
    testState.workflow.notice = notice;
    const onOpenChange = vi.fn();
    await mount(<AddProjectDialog open onOpenChange={onOpenChange} />);

    expect(buttonWithText("Back").disabled).toBe(true);
    const close = document.querySelector<HTMLButtonElement>('button[aria-label="Close"]');
    expect(close).not.toBeNull();
    await pressEscape();
    expect(onOpenChange).toHaveBeenCalledWith(false);
    onOpenChange.mockClear();
    await click(close!);
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it.each(["cloning", "registering"] as const)(
    "hides Close and ignores Escape while %s",
    async (progress) => {
      testState.workflow.step = "clone";
      testState.workflow.cloneUrl = "https://example.test/demo.git";
      testState.workflow.busy = true;
      testState.workflow.dismissible = false;
      testState.workflow.cloneProgress = progress;
      const onOpenChange = vi.fn();
      await mount(<AddProjectDialog open onOpenChange={onOpenChange} />);

      expect(document.querySelector('button[aria-label="Close"]')).toBeNull();
      await pressEscape();
      expect(onOpenChange).not.toHaveBeenCalledWith(false);
    },
  );

  it("renders the server directory browser for the remote-browse step", async () => {
    testState.workflow.step = "remote-browse";
    testState.workflow.selectedHost = {
      ...testState.workflow.selectedHost,
      label: "Remote",
    };
    await mount(<AddProjectDialog open onOpenChange={vi.fn()} />);
    expect(document.body.textContent).toContain("Open project folder on Remote");
    expect(document.body.textContent).toContain("Type a path instead");
    expect(document.body.textContent).toContain("Open project");
  });

  it("disables the remote browser controls while registration is busy", async () => {
    testState.workflow.step = "remote-browse";
    testState.workflow.selectedHost = {
      ...testState.workflow.selectedHost,
      label: "Remote",
    };
    testState.workflow.busy = true;
    await mount(<AddProjectDialog open onOpenChange={vi.fn()} />);
    const busyFieldset = document.querySelector("fieldset");
    expect(busyFieldset).not.toBeNull();
    expect(busyFieldset?.hasAttribute("disabled")).toBe(true);
  });

  it("leaves the remote browser controls enabled when not busy", async () => {
    testState.workflow.step = "remote-browse";
    testState.workflow.selectedHost = {
      ...testState.workflow.selectedHost,
      label: "Remote",
    };
    await mount(<AddProjectDialog open onOpenChange={vi.fn()} />);
    const fieldset = document.querySelector("fieldset");
    expect(fieldset).not.toBeNull();
    expect(fieldset?.hasAttribute("disabled")).toBe(false);
  });

  it("wires the remote-browse step's browser actions to the workflow", async () => {
    testState.workflow.step = "remote-browse";
    testState.workflow.selectedHost = {
      ...testState.workflow.selectedHost,
      label: "Remote",
    };
    testState.workflow.error = "Folder is not readable.";
    await mount(<AddProjectDialog open onOpenChange={vi.fn()} />);

    const alert = document.querySelector('[role="alert"]');
    expect(alert?.textContent).toContain("Folder is not readable.");

    await click(buttonWithText("Open project"));
    expect(testState.workflow.selectBrowsedFolder).toHaveBeenCalledWith("/srv/code/demo");

    await click(buttonWithText("Type a path instead"));
    expect(testState.workflow.openHostPath).toHaveBeenCalledTimes(1);

    // The dialog's own "Back" link (asserted elsewhere) is the only way back from this
    // step; the browser must not render its own duplicate Cancel exit here.
    expect(
      Array.from(document.querySelectorAll("button")).some(
        (button) => button.textContent?.trim() === "Cancel",
      ),
    ).toBe(false);
  });
});
