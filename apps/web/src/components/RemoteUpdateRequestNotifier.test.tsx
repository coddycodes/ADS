// @vitest-environment happy-dom
import type { DesktopUpdateState } from "@coddy/contracts";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
const state = vi.hoisted(() => ({
  update: null as DesktopUpdateState | null,
  toasts: [] as Record<string, unknown>[],
}));
vi.mock("../state/desktopUpdate", () => ({ useDesktopUpdateState: () => state.update }));
vi.mock("./ui/toast", () => ({
  toastManager: { add: (toast: Record<string, unknown>) => state.toasts.push(toast) },
  stackedThreadToast: (input: unknown) => input,
}));
import {
  RemoteUpdateRequestNotifier,
  remoteUpdateRequestToast,
} from "./RemoteUpdateRequestNotifier";
const base: DesktopUpdateState = {
  enabled: true,
  status: "available",
  currentVersion: "0.7.2",
  hostArch: "arm64",
  appArch: "arm64",
  runningUnderArm64Translation: false,
  availableVersion: "0.7.3",
  downloadedVersion: null,
  downloadPercent: null,
  checkedAt: null,
  message: null,
  errorContext: null,
  canRetry: false,
  phase: "available",
  protection: [],
  requestedBy: null,
};
const roots: ReturnType<typeof createRoot>[] = [];
afterEach(async () => {
  for (const root of roots.splice(0)) await act(async () => root.unmount());
  document.body.replaceChildren();
  state.toasts = [];
  state.update = null;
});
describe("remote update request notice", () => {
  it("names metadata without fabricating a missing address or version", () => {
    expect(remoteUpdateRequestToast({ label: "Tablet", detail: null }, null).description).toBe(
      "Tablet is installing an update. Coddy Studio restarts when it's done.",
    );
    expect(
      remoteUpdateRequestToast({ label: "Desktop", detail: "MacIntel" }, "0.7.3").description,
    ).toContain("Desktop on MacIntel is installing v0.7.3");
  });
  it("announces once per request and offers Manage devices", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    roots.push(root);
    const manage = vi.fn();
    const show = async (update: DesktopUpdateState) => {
      state.update = update;
      await act(async () => root.render(<RemoteUpdateRequestNotifier onManageDevices={manage} />));
    };
    await show({ ...base, requestedBy: { label: "Tablet", detail: null } });
    await show({
      ...base,
      phase: "protecting",
      downloadedVersion: "0.7.3",
      requestedBy: { label: "Tablet", detail: null },
    });
    expect(state.toasts).toHaveLength(1);
    const action = state.toasts[0]!.actionProps as { children: string; onClick: () => void };
    expect(action.children).toBe("Manage devices");
    action.onClick();
    expect(manage).toHaveBeenCalledOnce();
    await show(base);
    await show({ ...base, requestedBy: { label: "Laptop", detail: null } });
    expect(state.toasts).toHaveLength(2);
  });
});
