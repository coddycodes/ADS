// @vitest-environment happy-dom

import { EnvironmentId } from "@coddy/contracts";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { AddProjectHostOption } from "./add-project/AddProjectDialog.logic";
import type { AddProjectWorkflow } from "./add-project/useAddProjectWorkflow";

const h = vi.hoisted(() => ({
  hosts: [] as ReadonlyArray<AddProjectHostOption>,
  current: null as AddProjectWorkflow | null,
  close: vi.fn(),
  addFolder: vi.fn(async () => true),
  clone: vi.fn(async () => ({ _tag: "Opened" as const })),
  create: vi.fn(async () => true),
  cancelClone: vi.fn(async () => ({ _tag: "Success" as const, value: { cancelled: true } })),
  pickFolder: vi.fn(async () => ({ _tag: "Cancelled" as const })),
}));

// Keep the real state hook, dialog and path step. Only host/operation ports are inert.
vi.mock("./add-project/useAddProjectWorkflow", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./add-project/useAddProjectWorkflow")>();
  return {
    ...actual,
    useAddProjectWorkflow(input: { open: boolean; onOpenChange: (open: boolean) => void }) {
      const workflow = actual.useAddProjectWorkflowState({
        ...input,
        hosts: h.hosts,
        locationLabel: "Host",
        primaryEnvironmentId: EnvironmentId.make("primary"),
        initialEnvironmentId: null,
        operations: {
          addFolder: h.addFolder,
          clone: h.clone,
          create: h.create,
          cancelClone: h.cancelClone,
        },
        pickFolder: h.pickFolder,
      });
      h.current = workflow;
      return workflow;
    },
  };
});

import { AddProjectDialog } from "./AddProjectDialog";

const primary: AddProjectHostOption = {
  environmentId: EnvironmentId.make("primary"),
  label: "Primary host",
  platform: null,
  baseDirectory: "~/",
  isPrimary: true,
  desktopInstanceId: null,
  nativePickerAvailable: false,
};
const remote: AddProjectHostOption = {
  ...primary,
  environmentId: EnvironmentId.make("remote"),
  label: "Remote host",
  baseDirectory: "/remote/",
  isPrimary: false,
};
let root: Root | undefined;
let animations: PropertyDescriptor | undefined;
const workflow = () => {
  if (!h.current) throw new Error("Workflow was not rendered.");
  return h.current;
};
const pathInput = () => {
  const value = document.querySelector("#add-project-host-path");
  if (!(value instanceof HTMLInputElement)) throw new Error("Path input was not rendered.");
  return value;
};
const openButton = () => {
  const value = pathInput().form?.querySelector('button[type="submit"]');
  if (!(value instanceof HTMLButtonElement)) throw new Error("Open button was not rendered.");
  return value;
};
const render = async () =>
  act(async () => root?.render(<AddProjectDialog open onOpenChange={h.close} />));
async function mountPath(hosts: ReadonlyArray<AddProjectHostOption> = [primary]) {
  h.hosts = hosts;
  const container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await render();
  await act(async () => {
    workflow().openHostPath();
    workflow().setHostPath("/typed/project");
  });
}
async function attempt(action: "pointer" | "enter" | "form") {
  await act(async () => {
    if (action === "pointer") openButton().click();
    else if (action === "enter")
      pathInput().dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }),
      );
    else pathInput().form!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
  });
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.current = null;
  vi.clearAllMocks();
  animations = Object.getOwnPropertyDescriptor(Element.prototype, "getAnimations");
  Object.defineProperty(Element.prototype, "getAnimations", {
    configurable: true,
    value: () => [],
  });
});
afterEach(async () => {
  await act(async () => root?.unmount());
  root = undefined;
  document.body.replaceChildren();
  if (animations) Object.defineProperty(Element.prototype, "getAnimations", animations);
  else Reflect.deleteProperty(Element.prototype, "getAnimations");
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("Add Project manual-path host readiness", () => {
  it.each(["pointer", "enter", "form"] as const)(
    "blocks %s while loading, then uses late metadata without losing input",
    async (action) => {
      await mountPath();
      await attempt(action);
      expect(workflow().error).toBeNull();
      expect(h.addFolder).not.toHaveBeenCalled();
      expect(h.close).not.toHaveBeenCalled();
      expect(openButton().disabled).toBe(true);
      expect(pathInput().disabled).toBe(false);
      expect(pathInput().form?.querySelector('[role="status"]')?.textContent).toBe(
        "Waiting for host information…",
      );
      h.hosts = [{ ...primary, platform: "Linux", baseDirectory: "/new-default/" }];
      await render();
      expect(workflow().selectedHost.environmentId).toBe(primary.environmentId);
      expect(pathInput().value).toBe("/typed/project");
      expect(openButton().disabled).toBe(false);
      expect(pathInput().form?.querySelector('[role="status"]')).toBeNull();
      expect(h.addFolder).not.toHaveBeenCalled();
      await attempt(action);
      expect(h.addFolder).toHaveBeenCalledExactlyOnceWith(
        expect.objectContaining({
          environmentId: primary.environmentId,
          workspaceRoot: "/typed/project",
        }),
      );
      expect(h.close).toHaveBeenCalledExactlyOnceWith(false);
    },
  );

  it("tracks the selected host rather than a late update for a different host", async () => {
    await mountPath([primary, remote]);
    await act(async () => {
      workflow().selectHost(remote.environmentId);
      workflow().openHostPath();
      workflow().setHostPath("/typed/remote");
    });
    h.hosts = [{ ...primary, platform: "Linux" }, remote];
    await render();
    expect(openButton().disabled).toBe(true);
    expect(workflow().selectedHost.environmentId).toBe(remote.environmentId);
    h.hosts = [
      { ...primary, platform: "Linux" },
      { ...remote, platform: "Linux", baseDirectory: "/changed/" },
    ];
    await render();
    expect(pathInput().value).toBe("/typed/remote");
    expect(openButton().disabled).toBe(false);
    await attempt("pointer");
    expect(h.addFolder).toHaveBeenCalledExactlyOnceWith(
      expect.objectContaining({
        environmentId: remote.environmentId,
        workspaceRoot: "/typed/remote",
      }),
    );
  });

  it("keeps the existing disconnect fallback and never submits the lost host", async () => {
    await mountPath([{ ...primary, platform: "Linux" }, remote]);
    await act(async () => {
      workflow().selectHost(remote.environmentId);
      workflow().openHostPath();
      workflow().setHostPath("/typed/remote");
    });
    h.hosts = [{ ...primary, platform: "Linux" }];
    await render();
    expect(workflow().step).toBe("start");
    expect(workflow().selectedHost.environmentId).toBe(primary.environmentId);
    expect(workflow().error).toBe("The selected host disconnected. Choose a host and try again.");
    expect(document.querySelector("#add-project-host-path")).toBeNull();
    expect(h.addFolder).not.toHaveBeenCalled();
  });

  it("retains submit-time validation for a ready host", async () => {
    await mountPath([{ ...primary, platform: "Linux" }]);
    await act(async () => workflow().setHostPath("relative/path"));
    await attempt("pointer");
    expect(workflow().error).toBe("Enter an absolute or home-relative path.");
    expect(h.addFolder).not.toHaveBeenCalled();
  });
});
