// @vitest-environment happy-dom

import type { DeviceCatalog } from "@bibcode/contracts";
import { EnvironmentId } from "@bibcode/contracts";
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

type AnyProps = Record<string, unknown>;

const h = vi.hoisted(() => ({
  availability: { known: true, supported: true, enabled: false },
  catalog: null as unknown,
  queried: [] as unknown[],
  updates: [] as Array<{ environmentId: string; patch: unknown }>,
}));

vi.mock("../device/deviceAvailability", () => ({
  useDevicePreviewsAvailability: () => h.availability,
}));

vi.mock("../../state/environments", () => ({
  useEnvironment: (environmentId: string) => ({ environmentId, label: "Work Server" }),
}));

vi.mock("../../state/device", () => ({
  deviceEnvironment: { catalog: (args: unknown) => ({ __q: "catalog", args }) },
}));

vi.mock("../../state/query", () => ({
  useEnvironmentQuery: (atom: unknown) => {
    h.queried.push(atom);
    return { data: atom === null ? null : h.catalog };
  },
}));

vi.mock("../../hooks/useSettings", () => ({
  useUpdateEnvironmentSettings: (environmentId: string) => (patch: unknown) => {
    h.updates.push({ environmentId, patch });
    return Promise.resolve({ _tag: "Success" });
  },
}));

vi.mock("../ui/switch", () => ({
  Switch: (props: AnyProps) => (
    <button
      type="button"
      role="switch"
      aria-label={props["aria-label"] as string}
      aria-checked={Boolean(props.checked)}
      disabled={Boolean(props.disabled)}
      onClick={() => (props.onCheckedChange as (checked: boolean) => void)(!props.checked)}
    />
  ),
}));

vi.mock("../ui/alert-dialog", () => ({
  AlertDialog: (props: AnyProps) =>
    props.open ? <div role="alertdialog">{props.children as ReactNode}</div> : null,
  AlertDialogPopup: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
  AlertDialogHeader: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
  AlertDialogFooter: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
  AlertDialogTitle: (props: AnyProps) => <h2>{props.children as ReactNode}</h2>,
  AlertDialogDescription: (props: AnyProps) => <p>{props.children as ReactNode}</p>,
  AlertDialogClose: (props: AnyProps) => (
    <button type="button">{props.children as ReactNode}</button>
  ),
}));

import { DevicePreviewsSection } from "./DevicePreviewsSection";

const ENVIRONMENT_ID = EnvironmentId.make("environment-work");

const runningCatalog: DeviceCatalog = {
  availability: { kind: "ready", message: "Ready." },
  avds: [{ name: "Pixel_8", displayName: "Pixel 8" }],
  devices: [{ deviceId: "d-1", avdName: "Pixel_8", state: "ready", orientation: "portrait" }],
};

const mounted: Array<{ root: Root; container: HTMLDivElement }> = [];

async function render(): Promise<HTMLDivElement> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  mounted.push({ root, container });
  await act(async () => {
    root.render(<DevicePreviewsSection environmentId={ENVIRONMENT_ID} />);
  });
  return container;
}

async function click(element: Element | null | undefined) {
  if (!(element instanceof HTMLElement)) throw new Error("Nothing to click");
  await act(async () => {
    element.click();
  });
}

const toggle = (container: HTMLElement) => container.querySelector("[role='switch']");

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.availability = { known: true, supported: true, enabled: false };
  h.catalog = null;
  h.queried.length = 0;
  h.updates.length = 0;
});

afterEach(async () => {
  for (const entry of mounted.splice(0)) {
    await act(async () => entry.root.unmount());
    entry.container.remove();
  }
});

describe("DevicePreviewsSection", () => {
  it("turns previews on and does not read the catalog while off", async () => {
    const container = await render();
    expect(container.textContent).toContain(
      "Run an Android emulator on this environment's server and show it in a side panel. Requires the Android SDK with at least one virtual device and hardware acceleration on the server.",
    );
    expect(h.queried.every((atom) => atom === null)).toBe(true);
    await click(toggle(container));
    expect(h.updates).toEqual([
      { environmentId: ENVIRONMENT_ID, patch: { enableDevicePreviews: true } },
    ]);
  });

  it("turns off at once when no device is running", async () => {
    h.availability = { known: true, supported: true, enabled: true };
    h.catalog = { ...runningCatalog, devices: [] };
    const container = await render();
    await click(toggle(container));
    expect(container.querySelector("[role='alertdialog']")).toBeNull();
    expect(h.updates).toEqual([
      { environmentId: ENVIRONMENT_ID, patch: { enableDevicePreviews: false } },
    ]);
  });

  it("confirms before turning off with a running device", async () => {
    h.availability = { known: true, supported: true, enabled: true };
    h.catalog = runningCatalog;
    const container = await render();
    await click(toggle(container));
    const dialog = container.querySelector("[role='alertdialog']");
    expect(dialog?.textContent).toContain("This stops Pixel_8 on Work Server.");
    expect(h.updates).toEqual([]);
    const confirm = [...(dialog?.querySelectorAll("button") ?? [])].find(
      (button) => button.textContent === "Stop Pixel_8 and turn off",
    );
    await click(confirm);
    expect(h.updates).toEqual([
      { environmentId: ENVIRONMENT_ID, patch: { enableDevicePreviews: false } },
    ]);
    expect(container.querySelector("[role='alertdialog']")).toBeNull();
  });

  it("explains a server without device previews", async () => {
    h.availability = { known: true, supported: false, enabled: false };
    const container = await render();
    expect((toggle(container) as HTMLButtonElement).disabled).toBe(true);
    expect(container.textContent).toContain(
      "This server's BiBCode version has no device previews. Update the server to use them.",
    );
  });
});
