// @vitest-environment happy-dom

import type { DesktopProjectDataEnvironmentStatus } from "@coddy/contracts";
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

vi.mock("../ui/dialog", () => {
  const passthrough = ({ children }: { children?: ReactNode }) => <div>{children}</div>;
  return {
    Dialog: ({ open, children }: { open: boolean; children?: ReactNode }) =>
      open ? <div>{children}</div> : null,
    DialogDescription: passthrough,
    DialogFooter: passthrough,
    DialogHeader: passthrough,
    DialogPanel: passthrough,
    DialogPopup: passthrough,
    DialogTitle: passthrough,
  };
});
vi.mock("../ui/button", () => ({
  Button: (props: React.ButtonHTMLAttributes<HTMLButtonElement>) => <button {...props} />,
}));

import { ProjectDataRecoveryDialog } from "./ProjectDataRecoveryDialog";

const status: DesktopProjectDataEnvironmentStatus = {
  environmentId: "primary",
  label: "Local",
  runningDistro: null,
  status: "recovery-required",
  requestedRoot: "/Users/user/.coddy/studio",
  effectiveRoot: "/Volumes/Data/.coddy/studio",
  isFilesystemAlias: true,
  storageInstanceId: "b102f72a-c63b-4801-8f14-fba7a16856b8",
  issue: "The database is missing.",
  backups: [
    {
      backupId: "26b6ca53-27d3-401a-b51f-d7bdf534081f",
      createdAt: "2026-08-10T12:30:00Z",
      trigger: "pre-update",
      appVersion: "0.3.10",
      schemaVersion: 38,
      sizeBytes: 1024,
    },
  ],
};

let container: HTMLDivElement;
let root: Root;

function button(text: string): HTMLButtonElement {
  const match = Array.from(container.querySelectorAll("button")).find((entry) =>
    entry.textContent?.includes(text),
  );
  expect(match).toBeDefined();
  return match!;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

describe("ProjectDataRecoveryDialog", () => {
  it("shows environment diagnostics and requires a selected backup confirmation", async () => {
    const onRestore = vi.fn(async () => undefined);
    await act(async () => {
      root.render(
        <ProjectDataRecoveryDialog
          open
          status={status}
          busy={false}
          error={null}
          onOpenChange={() => undefined}
          onRetry={() => undefined}
          onRestore={onRestore}
          onStartEmpty={async () => undefined}
          onOpenPath={() => undefined}
          onExportDiagnostics={() => undefined}
        />,
      );
    });

    expect(container.textContent).toContain("Local");
    expect(container.textContent).toContain("/Users/user/.coddy/studio");
    expect(container.textContent).toContain("/Volumes/Data/.coddy/studio");
    expect(container.textContent).toContain("filesystem alias");
    expect(button("Restore selected backup").disabled).toBe(true);

    const backup = container.querySelector<HTMLInputElement>(
      'input[value="26b6ca53-27d3-401a-b51f-d7bdf534081f"]',
    );
    expect(backup).not.toBeNull();
    await act(async () => backup!.click());
    await act(async () => button("Restore selected backup").click());
    expect(container.textContent).toContain("replace the active database");
    await act(async () => button("Confirm restore").click());
    expect(onRestore).toHaveBeenCalledWith("26b6ca53-27d3-401a-b51f-d7bdf534081f");
  });

  it("uses a separate start-empty confirmation and says files are preserved", async () => {
    const onStartEmpty = vi.fn(async () => undefined);
    const onAdoptStorage = vi.fn();
    await act(async () => {
      root.render(
        <ProjectDataRecoveryDialog
          open
          status={{ ...status, backups: [] }}
          busy={false}
          error={null}
          onOpenChange={() => undefined}
          onRetry={() => undefined}
          onRestore={async () => undefined}
          onStartEmpty={onStartEmpty}
          onOpenPath={() => undefined}
          onExportDiagnostics={() => undefined}
          requiresStorageAdoption
          onAdoptStorage={onAdoptStorage}
        />,
      );
    });

    expect(container.textContent).toContain("No verified backup is available");
    expect(container.textContent).toContain("new storage identity");
    await act(async () => button("Use new storage identity").click());
    expect(onAdoptStorage).toHaveBeenCalledOnce();
    await act(async () => button("Start empty").click());
    expect(container.textContent).toContain("preserved, not deleted");
    await act(async () => button("Confirm start empty").click());
    expect(onStartEmpty).toHaveBeenCalledOnce();
  });

  it("requires a successful retry before adopting after a committed restart failure", async () => {
    await act(async () => {
      root.render(
        <ProjectDataRecoveryDialog
          open
          status={status}
          busy={false}
          error={null}
          restartError="The backend did not restart."
          requiresStorageAdoption
          onOpenChange={() => undefined}
          onRetry={() => undefined}
          onRestore={async () => undefined}
          onStartEmpty={async () => undefined}
          onOpenPath={() => undefined}
          onExportDiagnostics={() => undefined}
          onAdoptStorage={() => undefined}
        />,
      );
    });

    expect(container.textContent).toContain("Recovery committed");
    expect(container.textContent).not.toContain("Use new storage identity");
  });
});
