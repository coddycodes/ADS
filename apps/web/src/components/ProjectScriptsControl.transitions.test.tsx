// @vitest-environment happy-dom

import type { ProjectScript, ResolvedKeybindingsConfig } from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import { act, type FormEvent, Suspense } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import {
  type NewProjectScriptInput,
  type ProjectScriptActionResult,
  type ProjectScriptsControlProps,
  type ProjectScriptsController,
  useProjectScriptsController,
} from "./ProjectScriptsControl";

type InternalController = ProjectScriptsController & {
  readonly dialogOpen: boolean;
  readonly deleteConfirmOpen: boolean;
  readonly validationError: string | null;
  readonly setName: (value: string) => void;
  readonly setCommand: (value: string) => void;
  readonly submitScript: (event: FormEvent) => Promise<void>;
  readonly confirmDeleteScript: () => void;
};

const script: ProjectScript = {
  id: "dev",
  name: "Dev",
  command: "vp dev",
  icon: "play",
  runOnWorktreeCreate: false,
  autoOpenPreview: false,
};
const success = { _tag: "Success", value: undefined } as unknown as ProjectScriptActionResult;
const failure = {
  _tag: "Failure",
  cause: Cause.fail(new Error("late save failure")),
} as unknown as ProjectScriptActionResult;
const reason = "Workspace unavailable. Retry detection or remove it from BiBCode.";

describe("ProjectScriptsControl availability transitions", () => {
  let container: HTMLDivElement;
  let root: Root;
  let controller: InternalController;
  const onRunScript = vi.fn();
  const onAddScript = vi.fn<(input: NewProjectScriptInput) => Promise<ProjectScriptActionResult>>();
  const onUpdateScript =
    vi.fn<(id: string, input: NewProjectScriptInput) => Promise<ProjectScriptActionResult>>();
  const onDeleteScript = vi.fn<(id: string) => Promise<ProjectScriptActionResult>>();

  const props = (enabled = true): ProjectScriptsControlProps => ({
    scripts: [script],
    keybindings: [] as ResolvedKeybindingsConfig,
    enabled,
    disabledReason: enabled ? null : reason,
    onRunScript,
    onAddScript,
    onUpdateScript,
    onDeleteScript,
  });
  const captureController = (next: ProjectScriptsController) => {
    controller = next as InternalController;
  };
  function Harness({
    suspended,
    onController,
    ...next
  }: ProjectScriptsControlProps & {
    suspended?: Promise<never>;
    onController: (controller: ProjectScriptsController) => void;
  }) {
    onController(useProjectScriptsController(next));
    if (suspended) throw suspended;
    return null;
  }
  const render = async (enabled = true) => {
    await act(async () =>
      root.render(
        <Suspense fallback={null}>
          <Harness {...props(enabled)} onController={captureController} />
        </Suspense>,
      ),
    );
  };
  const seedForm = async () => {
    await act(async () => {
      controller.setName("Builder");
      controller.setCommand("vp build");
    });
  };

  beforeEach(async () => {
    Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    onRunScript.mockReset();
    onAddScript.mockReset().mockResolvedValue(success);
    onUpdateScript.mockReset().mockResolvedValue(success);
    onDeleteScript.mockReset().mockResolvedValue(success);
    await render();
  });

  afterEach(async () => {
    await act(async () => root.unmount());
    container.remove();
    delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT;
  });

  it("fails closed stale run, add, update, and delete callbacks after becoming unavailable", async () => {
    const staleRun = controller.runScript;
    await act(async () => controller.openAddDialog());
    await seedForm();
    const staleAdd = controller.submitScript;

    await act(async () => controller.openEditDialog(script));
    await seedForm();
    const staleUpdate = controller.submitScript;
    const staleDelete = controller.confirmDeleteScript;

    await render(false);
    expect(controller.dialogOpen).toBe(false);
    expect(controller.deleteConfirmOpen).toBe(false);

    await act(async () => {
      staleRun(script);
      await staleAdd({ preventDefault: vi.fn() } as unknown as FormEvent);
      await staleUpdate({ preventDefault: vi.fn() } as unknown as FormEvent);
      staleDelete();
    });

    expect(onRunScript).not.toHaveBeenCalled();
    expect(onAddScript).not.toHaveBeenCalled();
    expect(onUpdateScript).not.toHaveBeenCalled();
    expect(onDeleteScript).not.toHaveBeenCalled();
    expect(controller.validationError).toBe(reason);
  });

  it("keeps stale callbacks on the last committed availability during a suspended render", async () => {
    const staleRun = controller.runScript;
    const suspended = new Promise<never>(() => undefined);

    await act(async () => {
      root.render(
        <Suspense fallback={null}>
          <Harness {...props(false)} suspended={suspended} onController={captureController} />
        </Suspense>,
      );
    });
    staleRun(script);

    expect(onRunScript).toHaveBeenCalledOnce();
    expect(onRunScript).toHaveBeenCalledWith(script);
  });

  it("does not let a late async save overwrite the availability reason", async () => {
    let resolveSave!: (result: ProjectScriptActionResult) => void;
    onAddScript.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveSave = resolve;
        }),
    );
    await act(async () => controller.openAddDialog());
    await seedForm();
    const pending = controller.submitScript({ preventDefault: vi.fn() } as unknown as FormEvent);

    await render(false);
    await act(async () => resolveSave(failure));
    await act(async () => pending);

    expect(onAddScript).toHaveBeenCalledOnce();
    expect(controller.dialogOpen).toBe(false);
    expect(controller.validationError).toBe(reason);
  });
});
