// @vitest-environment happy-dom

import type { DeviceCatalog } from "@coddy/contracts";
import { EnvironmentId } from "@coddy/contracts";
import * as Cause from "effect/Cause";
import { AsyncResult } from "effect/reactivity";
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

type AnyProps = Record<string, unknown>;

const h = vi.hoisted(() => ({
  availability: { known: true, supported: true, enabled: false },
  catalog: null as unknown,
  queried: [] as unknown[],
  updates: [] as Array<{ environmentId: string; patch: unknown }>,
  updateResult: null as unknown,
  updateGate: null as Promise<void> | null,
  environments: [] as Array<{
    environmentId: string;
    label: string;
    connection: { phase: string };
  }>,
  primaryEnvironment: null as { environmentId: string; label: string } | null,
}));

vi.mock("../device/deviceAvailability", () => ({
  useDevicePreviewsAvailability: () => h.availability,
}));

vi.mock("../../state/environments", () => ({
  useEnvironment: (environmentId: string) => ({ environmentId, label: "Work Server" }),
  useEnvironments: () => ({ environments: h.environments }),
  usePrimaryEnvironment: () => h.primaryEnvironment,
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
    return (h.updateGate ?? Promise.resolve()).then(
      () => h.updateResult ?? { _tag: "Success", value: undefined },
    );
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

import { DevicePreviewsSection, MobileSettingsPanel } from "./DevicePreviewsSection";

const ENVIRONMENT_ID = EnvironmentId.make("environment-work");

const runningCatalog: DeviceCatalog = {
  platforms: [
    {
      platform: "android",
      availability: { kind: "ready", message: "Ready." },
      images: [{ id: "Pixel_8", displayName: "Pixel 8" }],
    },
  ],
  devices: [
    {
      deviceId: "d-1",
      platform: "android",
      imageId: "Pixel_8",
      state: "ready",
      orientation: "portrait",
      canRotate: true,
    },
  ],
};

const mounted: Array<{ root: Root; container: HTMLDivElement }> = [];

async function render(
  element: ReactNode = <DevicePreviewsSection environmentId={ENVIRONMENT_ID} />,
): Promise<HTMLDivElement> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  mounted.push({ root, container });
  await act(async () => {
    root.render(element);
  });
  return container;
}

const headings = (container: HTMLElement) =>
  [...container.querySelectorAll("h2")].map((heading) => heading.textContent);

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
  h.updateResult = null;
  h.updateGate = null;
  h.environments = [];
  h.primaryEnvironment = null;
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
      "Android emulators and, on macOS servers, iOS simulators.",
    );
    expect(toggle(container)?.getAttribute("aria-label")).toBe("Device previews");
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
    expect(container.textContent).toContain("Pixel 8 is running on Work Server.");
    expect(dialog?.textContent).toContain("This stops Pixel 8 on Work Server.");
    expect(h.updates).toEqual([]);
    const confirm = [...(dialog?.querySelectorAll("button") ?? [])].find(
      (button) => button.textContent === "Stop Pixel 8 and turn off",
    );
    await click(confirm);
    expect(h.updates).toEqual([
      { environmentId: ENVIRONMENT_ID, patch: { enableDevicePreviews: false } },
    ]);
    expect(container.querySelector("[role='alertdialog']")).toBeNull();
  });

  it("names every running device, iOS by its simulator name", async () => {
    h.availability = { known: true, supported: true, enabled: true };
    h.catalog = {
      platforms: [
        ...runningCatalog.platforms,
        {
          platform: "ios",
          availability: { kind: "ready", message: "Ready." },
          images: [{ id: "UDID-1", displayName: "iPhone 17 Pro · iOS 27.0" }],
        },
      ],
      devices: [
        ...runningCatalog.devices,
        { ...runningCatalog.devices[0]!, deviceId: "i-1", platform: "ios", imageId: "UDID-1" },
      ],
    } satisfies DeviceCatalog;
    const container = await render();
    expect(container.textContent).toContain(
      "Pixel 8 and iPhone 17 Pro · iOS 27.0 are running on Work Server.",
    );
    expect(container.textContent).not.toContain("UDID-1");
    await click(toggle(container));
    const dialog = container.querySelector("[role='alertdialog']");
    expect(dialog?.textContent).toContain(
      "This stops Pixel 8 and iPhone 17 Pro · iOS 27.0 on Work Server.",
    );
    expect(dialog?.textContent).toContain("Stop devices and turn off");
  });

  it("shows saving, then an actionable line when the change fails", async () => {
    let open!: () => void;
    h.updateGate = new Promise<void>((resolve) => (open = resolve));
    h.updateResult = AsyncResult.failure(Cause.fail(new Error("The server refused the change.")));
    const container = await render();
    await click(toggle(container));
    expect(container.textContent).toContain("Saving…");
    expect((toggle(container) as HTMLButtonElement).disabled).toBe(true);
    await act(async () => {
      open();
      await h.updateGate;
    });
    expect(container.querySelector("[role='alert']")?.textContent).toBe(
      "Could not turn device previews on. The server refused the change. Try again.",
    );
    expect((toggle(container) as HTMLButtonElement).disabled).toBe(false);
  });

  it("explains a server without device previews", async () => {
    h.availability = { known: true, supported: false, enabled: false };
    const container = await render();
    expect((toggle(container) as HTMLButtonElement).disabled).toBe(true);
    expect(container.textContent).toContain(
      "This server's Coddy Studio version has no device previews. Update the server to use them.",
    );
  });
});

describe("MobileSettingsPanel", () => {
  const environment = (environmentId: string, label: string, phase = "connected") => ({
    environmentId,
    label,
    connection: { phase },
  });

  it("shows one Device previews toggle for a single connected environment", async () => {
    h.environments = [environment("environment-work", "Work Server")];
    const container = await render(<MobileSettingsPanel />);
    expect(headings(container)).toEqual(["Device previews"]);
    expect(container.querySelectorAll("[role='switch']")).toHaveLength(1);
  });

  it("names each connected environment and leaves out the disconnected ones", async () => {
    h.environments = [
      environment("environment-work", "Work Server"),
      environment("environment-mac", "Mac mini"),
      environment("environment-off", "Old box", "disconnected"),
    ];
    const container = await render(<MobileSettingsPanel />);
    expect(headings(container)).toEqual([
      "Device previews · Work Server",
      "Device previews · Mac mini",
    ]);
    expect(container.querySelector("#device-previews-environment-mac")).not.toBeNull();
  });

  it("shows the primary environment while nothing has connected yet", async () => {
    h.availability = { known: false, supported: false, enabled: false };
    h.environments = [environment("environment-work", "Work Server", "connecting")];
    h.primaryEnvironment = { environmentId: "environment-work", label: "Work Server" };
    const container = await render(<MobileSettingsPanel />);
    expect(headings(container)).toEqual(["Device previews"]);
    expect(container.textContent).toContain("Connecting to Work Server…");
  });
});
