// @vitest-environment happy-dom

import type { DesktopBridge, DesktopUpdateState } from "@coddy/contracts";
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  state: null as DesktopUpdateState | null,
  toastAdd: vi.fn(),
  navigate: vi.fn(),
  forceVisible: false,
}));

vi.mock("@tanstack/react-router", () => ({ useNavigate: () => harness.navigate }));
vi.mock("../../env", () => ({ isDesktopHost: true }));
vi.mock("../../state/desktopUpdate", () => ({ useDesktopUpdateState: () => harness.state }));
vi.mock("../desktopUpdate.logic", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../desktopUpdate.logic")>();
  return {
    ...actual,
    shouldShowDesktopUpdateButton: (state: DesktopUpdateState | null) =>
      harness.forceVisible || actual.shouldShowDesktopUpdateButton(state),
  };
});
vi.mock("../ui/toast", () => ({
  toastManager: { add: harness.toastAdd },
  stackedThreadToast: (value: unknown) => value,
}));
vi.mock("../desktop/UpdateProtectionDialog", () => ({
  UpdateProtectionDialog: ({ open }: { open: boolean }) =>
    open ? <div role="dialog">Update recovery</div> : null,
}));
vi.mock("../ui/alert", () => ({
  Alert: ({ children }: { children: ReactNode }) => <div>{children}</div>,
  AlertTitle: ({ children }: { children: ReactNode }) => <strong>{children}</strong>,
  AlertDescription: ({ children }: { children: ReactNode }) => <span>{children}</span>,
}));
vi.mock("../ui/tooltip", () => ({
  Tooltip: ({ children }: { children: ReactNode }) => <>{children}</>,
  TooltipTrigger: ({ render }: { render: ReactNode }) => <>{render}</>,
  TooltipPopup: ({ children }: { children: ReactNode }) => <span>{children}</span>,
}));

import { SidebarUpdatePill } from "./SidebarUpdatePill";

const baseState: DesktopUpdateState = {
  enabled: true,
  status: "idle",
  currentVersion: "1.0.0",
  hostArch: "x64",
  appArch: "x64",
  runningUnderArm64Translation: false,
  availableVersion: null,
  downloadedVersion: null,
  downloadPercent: null,
  checkedAt: null,
  message: null,
  errorContext: null,
  canRetry: false,
};
const recoveryState: DesktopUpdateState = {
  ...baseState,
  status: "error",
  backendRecovery: [
    { environmentId: "primary", label: "Local", reason: "port-in-use", port: 14373 },
  ],
};
let container: HTMLDivElement;
let root: Root;
let downloadUpdate: ReturnType<typeof vi.fn>;
let installUpdate: ReturnType<typeof vi.fn>;

async function render(state: DesktopUpdateState | null) {
  harness.state = state;
  await act(async () => root.render(<SidebarUpdatePill />));
}

function actionButton(): HTMLButtonElement {
  const button = container.querySelector<HTMLButtonElement>("button.update-main");
  expect(button).not.toBeNull();
  return button!;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.clearAllMocks();
  harness.state = null;
  harness.forceVisible = false;
  downloadUpdate = vi.fn();
  installUpdate = vi.fn();
  window.desktopBridge = { downloadUpdate, installUpdate } as unknown as DesktopBridge;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  delete window.desktopBridge;
});

describe("SidebarUpdatePill", () => {
  it("hides when neither an update nor architecture warning is visible", async () => {
    await render(null);
    expect(container.textContent).toBe("");
  });

  it("renders the Apple Silicon warning independently", async () => {
    await render({ ...baseState, hostArch: "arm64", appArch: "x64" });
    expect(container.textContent).toContain("Intel build on Apple Silicon");
    expect(container.textContent).toContain("native Apple Silicon build");
  });

  it("renders available, downloading and installation states", async () => {
    await render({ ...baseState, status: "available", availableVersion: "1.1.0" });
    expect(container.textContent).toContain("Update available");
    await render({ ...baseState, status: "downloading", downloadPercent: 42.8 });
    expect(container.textContent).toContain("Downloading (42%)");
    expect(actionButton().disabled).toBe(true);
    expect(actionButton().getAttribute("aria-disabled")).toBe("true");
    await render({ ...baseState, status: "downloading" });
    expect(container.textContent).toContain("Downloading…");
    await render({ ...baseState, status: "downloaded", downloadedVersion: "1.1.0" });
    expect(container.textContent).toContain("Restart to update");
  });

  it("guards download actions when the desktop bridge is unavailable", async () => {
    await render({ ...baseState, status: "available", availableVersion: "1.1.0" });
    delete window.desktopBridge;
    await act(async () => actionButton().click());
    expect(downloadUpdate).not.toHaveBeenCalled();
  });

  it("does not download when the disabled pill is clicked while downloading", async () => {
    await render({
      ...baseState,
      status: "downloading",
      availableVersion: "1.1.0",
      downloadPercent: 42.8,
    });
    expect(actionButton().disabled).toBe(true);
    await act(async () => actionButton().click());
    expect(downloadUpdate).not.toHaveBeenCalled();
    expect(installUpdate).not.toHaveBeenCalled();
    expect(harness.toastAdd).not.toHaveBeenCalled();
  });

  it("does not download for an action of none", async () => {
    harness.forceVisible = true;
    await render(baseState);
    expect(actionButton().disabled).toBe(false);
    await act(async () => actionButton().click());
    expect(downloadUpdate).not.toHaveBeenCalled();
    expect(installUpdate).not.toHaveBeenCalled();
    expect(harness.toastAdd).not.toHaveBeenCalled();
  });

  it("downloads updates and reports completion and action failures", async () => {
    await render({ ...baseState, status: "available", availableVersion: "1.1.0" });
    downloadUpdate.mockResolvedValueOnce({ accepted: true, completed: true, state: baseState });
    await act(async () => actionButton().click());
    expect(downloadUpdate).toHaveBeenCalledOnce();
    expect(harness.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Update downloaded" }),
    );
    downloadUpdate.mockResolvedValueOnce({
      accepted: true,
      completed: false,
      state: { ...baseState, message: "network failed" },
    });
    await act(async () => actionButton().click());
    expect(harness.toastAdd).toHaveBeenLastCalledWith(
      expect.objectContaining({
        title: "Could not download update",
        description: "network failed",
      }),
    );
  });

  it("reports rejected download attempts with error and unknown causes", async () => {
    await render({ ...baseState, status: "available", availableVersion: "1.1.0" });
    downloadUpdate.mockRejectedValueOnce(new Error("offline"));
    await act(async () => actionButton().click());
    expect(harness.toastAdd).toHaveBeenLastCalledWith(
      expect.objectContaining({ description: "offline" }),
    );
    downloadUpdate.mockRejectedValueOnce("offline");
    await act(async () => actionButton().click());
    expect(harness.toastAdd).toHaveBeenLastCalledWith(
      expect.objectContaining({ description: "An unexpected error occurred." }),
    );
  });

  it("opens typed protection instead of invoking the installer directly", async () => {
    await render({ ...baseState, status: "downloaded", downloadedVersion: "1.1.0" });
    await act(async () => actionButton().click());
    expect(container.querySelector('[role="dialog"]')).not.toBeNull();
    expect(installUpdate).not.toHaveBeenCalled();
  });

  it("shows the recovery warning, restart tooltip and recovery dialog", async () => {
    await render(recoveryState);
    expect(actionButton().textContent).toBe("Update not installed");
    expect(actionButton().getAttribute("aria-label")).toBe(
      "Update not installed: Coddy Studio's local server is stopped.",
    );
    expect(actionButton().querySelector("svg.lucide-triangle-alert")).not.toBeNull();
    await act(async () => actionButton().click());
    expect(container.querySelector('[role="dialog"]')).not.toBeNull();
    expect(installUpdate).not.toHaveBeenCalled();
  });

  it("reveals recovery even when an earlier update notification was dismissed", async () => {
    await render({ ...baseState, status: "available", availableVersion: "1.1.0" });
    await act(async () =>
      container.querySelector<HTMLButtonElement>('[aria-label="Dismiss update"]')!.click(),
    );
    expect(container.textContent).toBe("");
    await render(recoveryState);
    expect(actionButton().textContent).toBe("Update not installed");
  });
});
