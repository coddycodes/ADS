// @vitest-environment happy-dom

import type { DesktopBridge, DesktopUpdateState } from "@bibcode/contracts";
import { act } from "react";
import type { ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { AsyncResult } from "effect/reactivity";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { appAtomRegistry, resetAppAtomRegistryForTests } from "../../rpc/atomRegistry";

const harness = vi.hoisted(() => ({
  retryEnvironment: vi.fn(),
  retryRegistries: [] as unknown[],
  retryProjectData: vi.fn(),
  restartApp: vi.fn(),
  getUpdateState: vi.fn(),
  toastAdd: vi.fn(),
  toastClose: vi.fn(),
  onDismiss: (_open: boolean) => undefined as void,
}));

vi.mock("../../state/shell", () => ({
  environmentAvailabilityCommands: {
    retry: {
      label: "retry",
      run: (registry: unknown, id: string) => {
        harness.retryRegistries.push(registry);
        return harness.retryEnvironment(id);
      },
    },
  },
}));
vi.mock("../ui/toast", () => ({
  toastManager: { add: harness.toastAdd, close: harness.toastClose },
  stackedThreadToast: (toast: unknown) => toast,
}));

vi.mock("../ui/dialog", async () => {
  const passthrough = ({ children }: { children?: ReactNode }) => <div>{children}</div>;
  return {
    Dialog: ({
      open,
      children,
      onOpenChange,
    }: {
      open: boolean;
      children?: React.ReactNode;
      onOpenChange: (open: boolean) => void;
    }) => {
      harness.onDismiss = onOpenChange;
      return open ? <div>{children}</div> : null;
    },
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
vi.mock("../ui/checkbox", () => ({
  Checkbox: ({
    checked,
    onCheckedChange,
    ...props
  }: React.ButtonHTMLAttributes<HTMLButtonElement> & {
    checked?: boolean;
    onCheckedChange?: (checked: boolean) => void;
  }) => <button {...props} aria-pressed={checked} onClick={() => onCheckedChange?.(!checked)} />,
}));

import { UpdateProtectionDialog } from "./UpdateProtectionDialog";

const baseState: DesktopUpdateState = {
  enabled: true,
  status: "downloaded",
  currentVersion: "1.0.0",
  hostArch: "x64",
  appArch: "x64",
  runningUnderArm64Translation: false,
  availableVersion: "1.1.0",
  downloadedVersion: "1.1.0",
  downloadPercent: 100,
  checkedAt: null,
  message: null,
  errorContext: null,
  canRetry: false,
  phase: "failed",
  protection: [],
};

const recoveryState: DesktopUpdateState = {
  ...baseState,
  status: "error",
  errorContext: "install",
  message: "Installer failed; Local: address in use.",
  backendRecovery: [
    { environmentId: "primary", label: "Local", reason: "port-in-use", port: 14373 },
  ],
};

let container: HTMLDivElement;
let root: Root;

async function render(
  state: DesktopUpdateState,
  installUpdate = vi.fn(),
  onDiagnostics = vi.fn(),
  options: {
    open?: boolean;
    onError?: (message: string) => void;
    onOpenChange?: (open: boolean) => void;
  } = {},
) {
  await act(async () => {
    root.render(
      <UpdateProtectionDialog
        open={options.open ?? true}
        state={state}
        onOpenChange={options.onOpenChange ?? (() => undefined)}
        installUpdate={installUpdate}
        onDiagnostics={onDiagnostics}
        {...(options.onError ? { onError: options.onError } : {})}
      />,
    );
  });
  return installUpdate;
}

function button(text: string): HTMLButtonElement {
  const match = Array.from(container.querySelectorAll("button")).find((entry) =>
    entry.textContent?.includes(text),
  );
  expect(match).toBeDefined();
  return match!;
}

beforeEach(() => {
  vi.clearAllMocks();
  harness.retryProjectData.mockReset().mockResolvedValue(undefined);
  harness.restartApp.mockReset().mockResolvedValue(undefined);
  harness.retryEnvironment.mockReset().mockResolvedValue(AsyncResult.success(undefined));
  harness.retryRegistries = [];
  harness.getUpdateState.mockReset().mockResolvedValue(baseState);
  harness.toastAdd
    .mockReset()
    .mockImplementation(() => `toast-${harness.toastAdd.mock.calls.length}`);
  window.desktopBridge = {
    retryProjectData: harness.retryProjectData,
    restartApp: harness.restartApp,
    getUpdateState: harness.getUpdateState,
  } as unknown as DesktopBridge;
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  // A fresh observer of resolved recovery clears toast ownership retained across unmounts.
  await act(async () => root.render(null));
  await render(baseState, vi.fn(), vi.fn(), { open: false });
  await act(async () => root.unmount());
  container.remove();
  delete window.desktopBridge;
});

describe("UpdateProtectionDialog", () => {
  it("shows recovery actions and explains why installation cannot be retried", async () => {
    const onOpenChange = vi.fn();
    await render(
      {
        ...recoveryState,
        protection: [
          { environmentId: "primary", label: "Local", status: "failed", message: "Backup failed." },
        ],
      },
      vi.fn(),
      vi.fn(),
      { onOpenChange },
    );

    expect(container.textContent).toContain("Update not installed");
    expect(container.textContent).toContain(
      "BiBCode's local server couldn't restart: port 14373 is in use by another program. Quit that program, then choose Restart server.",
    );
    expect(button("Retry installation").disabled).toBe(true);
    expect(container.textContent).toContain("Restart the server before retrying the installation.");
    expect(button("Restart server").disabled).toBe(false);
    expect(container.textContent).not.toContain("Continue without a backup");
    expect(container.textContent).not.toContain("Backup failed.");
    expect(button("Details").getAttribute("aria-expanded")).toBe("false");
    await act(async () => button("Details").click());
    expect(button("Details").getAttribute("aria-expanded")).toBe("true");
    expect(container.textContent).toContain(recoveryState.message);
    await act(async () => button("Close").click());
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("restarts each server sequentially before reconnecting and returns to installation without remounting", async () => {
    const calls: string[] = [];
    let finishPrimary!: () => void;
    let finishSecondary!: () => void;
    harness.retryProjectData.mockImplementation((id: string) => {
      calls.push(`restart:${id}`);
      return new Promise<void>((resolve) => {
        if (id === "primary") finishPrimary = resolve;
        else finishSecondary = resolve;
      });
    });
    harness.retryEnvironment.mockImplementation((id: string) => {
      calls.push(`reconnect:${id}`);
      return new Promise(() => undefined);
    });
    const onOpenChange = vi.fn();
    await render(
      {
        ...recoveryState,
        backendRecovery: [
          ...recoveryState.backendRecovery!,
          { environmentId: "wsl:Ubuntu", label: "Ubuntu", reason: "other", port: 14374 },
        ],
      },
      vi.fn(),
      vi.fn(),
      { onOpenChange },
    );
    await act(async () => button("Restart server").click());
    try {
      expect(calls).toEqual(["restart:primary"]);
      expect(button("Restarting server…").disabled).toBe(true);
      expect([...container.querySelectorAll("button")].every((entry) => entry.disabled)).toBe(true);
      await act(async () => harness.onDismiss(false));
      expect(onOpenChange).not.toHaveBeenCalled();
      await act(async () => {
        finishPrimary();
        await vi.dynamicImportSettled();
      });
      // Loading the reconnect command must not delay the next exclusive native restart.
      expect(calls).toEqual(["restart:primary", "restart:wsl:Ubuntu", "reconnect:primary"]);
    } finally {
      await act(async () => {
        finishPrimary();
        await Promise.resolve();
        finishSecondary?.();
        await vi.dynamicImportSettled();
      });
    }
    expect(calls).toEqual([
      "restart:primary",
      "restart:wsl:Ubuntu",
      "reconnect:primary",
      "reconnect:wsl:Ubuntu",
    ]);
    await render({ ...baseState, backendRecovery: [] });
    expect(button("Retry installation").disabled).toBe(false);
    expect(container.textContent).not.toContain("Update not installed");
  });

  it("continues after a rejected restart and shows the new reason, raw error and application restart refusal", async () => {
    const newError = "BiBCode's local server couldn't restart: access denied";
    harness.retryProjectData.mockRejectedValueOnce(newError).mockResolvedValueOnce(undefined);
    await render({
      ...recoveryState,
      backendRecovery: [
        ...recoveryState.backendRecovery!,
        { environmentId: "wsl:Ubuntu", label: "Ubuntu", reason: "other", port: 14374 },
      ],
    });
    await act(async () => button("Restart server").click());
    expect(harness.retryProjectData.mock.calls).toEqual([["primary"], ["wsl:Ubuntu"]]);
    expect(harness.retryEnvironment.mock.calls).toEqual([["wsl:Ubuntu"]]);
    await render({
      ...recoveryState,
      backendRecovery: [{ environmentId: "primary", label: "Local", reason: "other", port: 14373 }],
    });
    expect(container.textContent).toContain(
      "BiBCode's local server couldn't restart. Choose Restart server. If that fails, restart BiBCode.",
    );
    expect(button("Restart server").getAttribute("variant")).toBe("outline");
    await act(async () => button("Details").click());
    expect(container.textContent).toContain(newError);
    expect(container.textContent).not.toContain(recoveryState.message);
    harness.restartApp.mockRejectedValueOnce(
      "BiBCode can't restart while an update or project-data operation is running.",
    );
    await act(async () => button("Restart BiBCode").click());
    expect(harness.restartApp).toHaveBeenCalledOnce();
    expect(container.querySelector('[role="alert"]')?.textContent).toContain(
      "BiBCode can't restart while an update or project-data operation is running.",
    );
  });

  it("hides application restart when the desktop bridge does not support it", async () => {
    delete window.desktopBridge!.restartApp;
    harness.retryProjectData.mockRejectedValueOnce(new Error("Still unavailable."));
    await render(recoveryState);
    await act(async () => button("Restart server").click());
    expect(
      [...container.querySelectorAll("button")].some(
        (entry) => entry.textContent === "Restart BiBCode",
      ),
    ).toBe(false);
    await act(async () => button("Details").click());
    expect(container.textContent).toContain("Still unavailable.");
  });

  it("offers recovery from the install failure toast without calling the generic error callback", async () => {
    const onError = vi.fn();
    const installUpdate = vi
      .fn()
      .mockResolvedValue({ accepted: true, completed: false, state: recoveryState });
    await render(baseState, installUpdate, vi.fn(), { onError });
    await act(async () => button("Retry installation").click());
    expect(onError).not.toHaveBeenCalled();
    expect(harness.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "error",
        title: "Update not installed",
        timeout: 0,
        description:
          "BiBCode's local server couldn't restart: port 14373 is in use by another program. Quit that program, then choose Restart server.",
        actionProps: expect.objectContaining({ children: "Restart server" }),
      }),
    );
    const toast = harness.toastAdd.mock.calls[0]![0];
    const toastId = harness.toastAdd.mock.results[0]!.value;
    await act(async () => toast.actionProps.onClick());
    expect(harness.retryProjectData).toHaveBeenCalledWith("primary");
    expect(harness.retryEnvironment).toHaveBeenCalledWith("primary");
    expect(harness.toastClose).toHaveBeenCalledWith(toastId);
    expect(harness.toastAdd).toHaveBeenLastCalledWith(
      expect.objectContaining({ type: "success", title: "Server restarted" }),
    );
    expect(harness.toastAdd.mock.lastCall![0]).not.toHaveProperty("timeout");
  });

  it("closes the previous recovery toast before adding another", async () => {
    const installUpdate = vi
      .fn()
      .mockResolvedValue({ accepted: true, completed: false, state: recoveryState });
    await render(baseState, installUpdate);
    await act(async () => button("Retry installation").click());
    const firstToastId = harness.toastAdd.mock.results[0]!.value;
    expect(harness.toastClose).not.toHaveBeenCalled();

    await act(async () => button("Retry installation").click());

    expect(harness.toastAdd).toHaveBeenCalledTimes(2);
    expect(harness.toastClose).toHaveBeenCalledExactlyOnceWith(firstToastId);
    expect(harness.toastClose.mock.invocationCallOrder[0]).toBeLessThan(
      harness.toastAdd.mock.invocationCallOrder[1]!,
    );
  });

  it("closes the recovery toast after a successful dialog restart before state catches up", async () => {
    const installUpdate = vi
      .fn()
      .mockResolvedValue({ accepted: true, completed: false, state: recoveryState });
    await render(baseState, installUpdate);
    await act(async () => button("Retry installation").click());
    const toastId = harness.toastAdd.mock.results[0]!.value;
    await render(recoveryState);
    expect(harness.toastClose).not.toHaveBeenCalled();

    await act(async () => button("Restart server").click());

    expect(harness.retryProjectData).toHaveBeenCalledExactlyOnceWith("primary");
    expect(harness.toastClose).toHaveBeenCalledExactlyOnceWith(toastId);
  });

  it.each([
    { name: "empty", state: { ...baseState, backendRecovery: [] } },
    { name: "absent", state: baseState },
  ])(
    "closes recovery when a mounted, closed dialog observes $name backendRecovery",
    async ({ state }) => {
      const installUpdate = vi
        .fn()
        .mockResolvedValue({ accepted: true, completed: false, state: recoveryState });
      await render(baseState, installUpdate);
      await act(async () => button("Retry installation").click());
      const toastId = harness.toastAdd.mock.results[0]!.value;
      await render(recoveryState, installUpdate, vi.fn(), { open: false });
      expect(harness.toastClose).not.toHaveBeenCalled();

      await render(state, installUpdate, vi.fn(), { open: false });

      expect(harness.retryProjectData).not.toHaveBeenCalled();
      expect(harness.toastClose).toHaveBeenCalledExactlyOnceWith(toastId);
    },
  );

  it("reconnects from a toast after unmount using the current application registry", async () => {
    const installUpdate = vi
      .fn()
      .mockResolvedValue({ accepted: true, completed: false, state: recoveryState });
    await render(baseState, installUpdate);
    await act(async () => button("Retry installation").click());
    const toast = harness.toastAdd.mock.calls[0]![0];
    const toastId = harness.toastAdd.mock.results[0]!.value;
    await act(async () => root.render(null));
    expect(harness.toastClose).not.toHaveBeenCalled();
    resetAppAtomRegistryForTests();

    await act(async () => {
      toast.actionProps.onClick();
      await vi.dynamicImportSettled();
    });

    expect(harness.retryProjectData).toHaveBeenCalledWith("primary");
    expect(harness.retryEnvironment).toHaveBeenCalledWith("primary");
    expect(harness.retryRegistries).toEqual([appAtomRegistry]);
    expect(harness.toastClose).toHaveBeenCalledWith(toastId);
    expect(harness.toastAdd).toHaveBeenLastCalledWith(
      expect.objectContaining({ type: "success", title: "Server restarted" }),
    );
  });

  it("serializes server restarts across dialog instances without reporting a refused click", async () => {
    let finishRestart!: () => void;
    harness.retryProjectData.mockImplementation(
      () => new Promise<void>((resolve) => (finishRestart = resolve)),
    );
    await act(async () => {
      root.render(
        <>
          <UpdateProtectionDialog
            open
            state={recoveryState}
            onOpenChange={() => undefined}
            installUpdate={vi.fn()}
            onDiagnostics={() => undefined}
          />
          <UpdateProtectionDialog
            open
            state={recoveryState}
            onOpenChange={() => undefined}
            installUpdate={vi.fn()}
            onDiagnostics={() => undefined}
          />
        </>,
      );
    });
    const restartButtons = [...container.querySelectorAll("button")].filter(
      (entry) => entry.textContent === "Restart server",
    );
    expect(restartButtons).toHaveLength(2);
    await act(async () => restartButtons[0]!.click());
    const finishFirst = finishRestart;
    try {
      await act(async () => restartButtons[1]!.click());
      expect(harness.retryProjectData).toHaveBeenCalledOnce();
      expect(container.querySelector('[role="alert"]')).toBeNull();
      expect(container.textContent).not.toContain("Restart BiBCode");
      expect(harness.toastAdd).not.toHaveBeenCalled();
    } finally {
      await act(async () => {
        finishFirst();
        finishRestart();
        await vi.dynamicImportSettled();
      });
    }
  });

  it("shares the restart guard with application restart toast actions after unmount", async () => {
    harness.retryProjectData.mockRejectedValueOnce("Still unavailable.");
    const installUpdate = vi
      .fn()
      .mockResolvedValue({ accepted: true, completed: false, state: recoveryState });
    await render(baseState, installUpdate);
    await act(async () => button("Retry installation").click());
    await act(async () => harness.toastAdd.mock.lastCall![0].actionProps.onClick());
    const restartToast = harness.toastAdd.mock.lastCall![0];
    await act(async () => root.render(null));
    await render(recoveryState);
    let finishRestart!: () => void;
    harness.restartApp.mockImplementation(
      () => new Promise<void>((resolve) => (finishRestart = resolve)),
    );

    await act(async () => restartToast.actionProps.onClick());
    try {
      await act(async () => {
        button("Restart server").click();
        restartToast.actionProps.onClick();
      });
      expect(harness.restartApp).toHaveBeenCalledOnce();
      expect(harness.retryProjectData).toHaveBeenCalledOnce();
      expect(container.querySelector('[role="alert"]')).toBeNull();
      expect(harness.toastAdd).toHaveBeenCalledTimes(2);
    } finally {
      await act(async () => finishRestart());
    }

    await act(async () => button("Restart server").click());
    expect(harness.retryProjectData).toHaveBeenCalledTimes(2);
  });

  it("reports an application restart toast failure without reopening its dialog", async () => {
    harness.retryProjectData.mockRejectedValueOnce("Still unavailable.");
    harness.restartApp.mockRejectedValueOnce("Restart refused.");
    const onOpenChange = vi.fn();
    const installUpdate = vi
      .fn()
      .mockResolvedValue({ accepted: true, completed: false, state: recoveryState });
    await render(baseState, installUpdate, vi.fn(), { onOpenChange });
    await act(async () => button("Retry installation").click());
    await act(async () => harness.toastAdd.mock.lastCall![0].actionProps.onClick());
    await act(async () => harness.toastAdd.mock.lastCall![0].actionProps.onClick());

    expect(onOpenChange).not.toHaveBeenCalled();
    expect(harness.toastAdd).toHaveBeenLastCalledWith(
      expect.objectContaining({
        title: "Could not restart BiBCode",
        description: "Restart refused.",
      }),
    );
  });

  it.each(["server", "app"] as const)(
    "keeps the %s restart label while the host clears recovery before the command finishes",
    async (target) => {
      await render(recoveryState);
      if (target === "app") {
        harness.retryProjectData.mockRejectedValueOnce("Still unavailable.");
        await act(async () => button("Restart server").click());
      }
      let finishRestart!: () => void;
      const command = target === "server" ? harness.retryProjectData : harness.restartApp;
      command.mockImplementation(() => new Promise<void>((resolve) => (finishRestart = resolve)));
      await act(async () =>
        button(target === "server" ? "Restart server" : "Restart BiBCode").click(),
      );
      try {
        await render({ ...baseState, backendRecovery: [] });
        expect(container.textContent).toContain(
          target === "server" ? "Restarting server…" : "Restarting BiBCode…",
        );
        expect(container.textContent).not.toContain("Installing update…");
        expect([...container.querySelectorAll("button")].every((entry) => entry.disabled)).toBe(
          true,
        );
      } finally {
        await act(async () => {
          finishRestart();
          await vi.dynamicImportSettled();
        });
      }
      expect(button("Retry installation").disabled).toBe(false);
    },
  );

  it.each([
    { entries: recoveryState.backendRecovery!, message: "The server still couldn't restart." },
    {
      entries: [
        ...recoveryState.backendRecovery!,
        { environmentId: "wsl:Ubuntu", label: "Ubuntu", reason: "other" as const, port: 14374 },
      ],
      message: "The servers still couldn't restart.",
    },
  ])(
    "announces a failed retry even when the reason is unchanged: $message",
    async ({ entries, message }) => {
      harness.retryProjectData.mockRejectedValue(
        "BiBCode's local server couldn't restart: address in use.",
      );
      await render({ ...recoveryState, backendRecovery: entries });
      await act(async () => button("Restart server").click());

      expect(container.querySelector('[role="alert"]')?.textContent).toBe(message);
      expect(button("Details").disabled).toBe(false);
      expect(button("Restart BiBCode").disabled).toBe(false);
      expect(button("Restart server").disabled).toBe(false);
    },
  );

  it.each([false, true])(
    "preserves rejected installation strings (skip protection: %s)",
    async (skipProtection) => {
      const onError = vi.fn();
      const installUpdate = vi.fn().mockRejectedValue("The native installer refused the update.");
      await render(
        {
          ...baseState,
          protection: skipProtection
            ? [
                {
                  environmentId: "primary",
                  label: "Local",
                  status: "failed",
                  message: "Backup failed.",
                },
              ]
            : [],
        },
        installUpdate,
        vi.fn(),
        { onError },
      );
      if (skipProtection) {
        await act(async () =>
          container
            .querySelector<HTMLButtonElement>('[aria-label="Acknowledge update without backup"]')!
            .click(),
        );
      }
      await act(async () =>
        button(skipProtection ? "Install without backup" : "Retry installation").click(),
      );

      expect(onError).toHaveBeenCalledWith("An unexpected error occurred.");
    },
  );

  it("uses the settled recovery reason in a failed toast retry and offers application restart", async () => {
    const updatedState: DesktopUpdateState = {
      ...recoveryState,
      backendRecovery: [{ environmentId: "primary", label: "Local", reason: "other", port: 14373 }],
    };
    harness.getUpdateState.mockResolvedValue(updatedState);
    harness.retryProjectData.mockRejectedValueOnce(
      "BiBCode's local server couldn't restart: access denied",
    );
    const installUpdate = vi
      .fn()
      .mockResolvedValue({ accepted: true, completed: false, state: recoveryState });
    await render(baseState, installUpdate);
    await act(async () => button("Retry installation").click());
    const toast = harness.toastAdd.mock.calls[0]![0];
    const toastId = harness.toastAdd.mock.results[0]!.value;
    await act(async () => toast.actionProps.onClick());
    expect(harness.toastClose).toHaveBeenCalledExactlyOnceWith(toastId);
    expect(harness.toastAdd).toHaveBeenLastCalledWith(
      expect.objectContaining({
        type: "error",
        title: "Update not installed",
        timeout: 0,
        description:
          "BiBCode's local server couldn't restart. Choose Restart server. If that fails, restart BiBCode.",
        actionProps: expect.objectContaining({ children: "Restart BiBCode" }),
      }),
    );
    await act(async () => harness.toastAdd.mock.lastCall![0].actionProps.onClick());
    expect(harness.restartApp).toHaveBeenCalledOnce();
  });

  it("keeps ordinary installation failures on the existing error callback", async () => {
    const onError = vi.fn();
    const installUpdate = vi.fn().mockResolvedValue({
      accepted: true,
      completed: false,
      state: { ...baseState, message: "Installer failed." },
    });
    await render(baseState, installUpdate, vi.fn(), { onError });
    await act(async () => button("Retry installation").click());
    expect(onError).toHaveBeenCalledWith("Installer failed.");
    expect(harness.toastAdd).not.toHaveBeenCalled();
  });

  it("never offers exclusion when primary protection failed", async () => {
    const onDiagnostics = vi.fn();
    const installUpdate = vi.fn();
    await render(
      {
        ...baseState,
        protection: [
          {
            environmentId: "primary",
            label: "Local",
            status: "failed",
            message: "Backup failed.",
          },
        ],
      },
      installUpdate,
      onDiagnostics,
    );

    expect(container.textContent).toContain("Local");
    expect(container.textContent).toContain("Retry protection");
    expect(container.textContent).toContain("Diagnostics");
    expect(container.textContent).not.toContain("Exclude Local");
    const installWithoutBackup = button("Install without backup");
    const unprotectedInstallGroup = container.querySelector<HTMLElement>(
      '[role="group"][aria-label="Continue without a backup"]',
    );
    expect(unprotectedInstallGroup).not.toBeNull();
    expect(unprotectedInstallGroup!.contains(installWithoutBackup)).toBe(true);
    expect(installWithoutBackup.disabled).toBe(true);
    const acknowledgement = container.querySelector<HTMLButtonElement>(
      'button[aria-label="Acknowledge update without backup"]',
    );
    expect(acknowledgement).not.toBeNull();
    await act(async () => acknowledgement!.click());
    expect(installWithoutBackup.disabled).toBe(false);
    await act(async () => installWithoutBackup.click());
    expect(installUpdate).toHaveBeenCalledWith({ skipProtection: true });
    await act(async () => button("Diagnostics").click());
    expect(onDiagnostics).toHaveBeenCalledOnce();
  });

  it("requires an exact named secondary exclusion before retrying install", async () => {
    const installUpdate = await render({
      ...baseState,
      protection: [
        { environmentId: "primary", label: "Local", status: "protected", message: null },
        {
          environmentId: "wsl:Ubuntu",
          label: "WSL (Ubuntu)",
          status: "failed",
          message: "Distribution unavailable.",
        },
      ],
    });

    const install = button("Install with exclusions");
    expect(install.disabled).toBe(true);
    const exclusion = container.querySelector<HTMLButtonElement>(
      'button[aria-label="Exclude WSL (Ubuntu)"]',
    );
    expect(exclusion).not.toBeNull();
    await act(async () => exclusion!.click());
    expect(install.disabled).toBe(false);
    await act(async () => install.click());
    expect(installUpdate).toHaveBeenCalledWith({ excludedEnvironmentIds: ["wsl:Ubuntu"] });
  });

  it("shows protecting progress and prevents duplicate installation", async () => {
    await render({
      ...baseState,
      phase: "protecting",
      protection: [
        {
          environmentId: "primary",
          label: "Local",
          status: "pending",
          message: null,
          stage: "waiting-for-mutations",
          elapsedMs: 12_000,
          blockedOperationCount: 1,
        },
      ],
    });

    expect(container.textContent).toContain("Protecting Local");
    expect(container.textContent).toContain("Waiting for active operations");
    expect(container.textContent).toContain("1 active operation");
    expect(container.textContent).toContain("12s elapsed");
    expect(button("Protecting projects").disabled).toBe(true);
  });
});
