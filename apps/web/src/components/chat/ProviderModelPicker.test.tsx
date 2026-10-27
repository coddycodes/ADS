// @vitest-environment happy-dom

import {
  ProviderInstanceId,
  type ServerProvider,
  type ResolvedKeybindingsConfig,
} from "@coddy/contracts";
import {
  act,
  cloneElement,
  type ComponentPropsWithoutRef,
  type ReactElement,
  type ReactNode,
} from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import type { ProviderInstanceEntry } from "../../providerInstances";
import type { ModelEsque } from "./providerIconUtils";

const captured = vi.hoisted(() => ({
  contentProps: null as null | Record<string, unknown>,
}));

vi.mock("lucide-react", () => ({
  ChevronDownIcon: () => <span data-icon="chevron" />,
}));

vi.mock("../ui/button", () => ({
  buttonVariants: () => "button",
  Button: ({
    variant: _variant,
    size: _size,
    ...props
  }: ComponentPropsWithoutRef<"button"> & { variant?: string; size?: string }) => (
    <button {...props} />
  ),
}));

vi.mock("../ui/popover", () => ({
  Popover: ({
    children,
    open,
    onOpenChange,
  }: {
    children?: ReactNode;
    open: boolean;
    onOpenChange: (open: boolean) => void;
  }) => (
    <div data-popover-open={String(open)}>
      <button type="button" data-open-menu onClick={() => onOpenChange(true)} />
      <button type="button" data-close-menu onClick={() => onOpenChange(false)} />
      {children}
    </div>
  ),
  PopoverTrigger: ({
    render,
    children,
    ...props
  }: {
    render: ReactElement<Record<string, unknown>>;
    children?: ReactNode;
    [key: string]: unknown;
  }) => cloneElement(render, props, children),
  PopoverPopup: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
}));

vi.mock("../ui/tooltip", () => ({
  Tooltip: ({ children }: { children?: ReactNode }) => <>{children}</>,
  TooltipPopup: ({ children }: { children?: ReactNode }) => <span data-tooltip>{children}</span>,
  TooltipTrigger: ({
    render,
    children,
  }: {
    render: ReactElement<Record<string, unknown>>;
    children?: ReactNode;
  }) => cloneElement(render, { "data-tooltip-trigger": true }, children),
}));

vi.mock("~/lib/utils", () => ({
  cn: (...values: ReadonlyArray<unknown>) => values.filter(Boolean).join(" "),
}));

vi.mock("./ProviderInstanceIcon", () => ({
  ProviderInstanceIcon: (props: { displayName: string; showBadge: boolean; className: string }) => (
    <span
      data-provider-icon={props.displayName}
      data-show-badge={String(props.showBadge)}
      data-icon-class={props.className}
    />
  ),
}));

vi.mock("./ModelPickerContent", () => ({
  ModelPickerContent: (props: Record<string, unknown>) => {
    captured.contentProps = props;
    return (
      <div data-model-picker-content>
        <button
          type="button"
          data-select-model
          onClick={() =>
            (props.onInstanceModelChange as (instanceId: string, model: string) => void)(
              "claude",
              "opus",
            )
          }
        />
        <button
          type="button"
          data-request-close
          onClick={() => (props.onRequestClose as () => void)()}
        />
      </div>
    );
  },
}));

import { ProviderModelPicker } from "./ProviderModelPicker";

interface MountedTree {
  readonly container: HTMLDivElement;
  readonly root: Root;
}

const mountedTrees: MountedTree[] = [];
const onInstanceModelChange = vi.fn();
const onOpenChange = vi.fn();

function entry(
  instanceId: string,
  overrides: Partial<ProviderInstanceEntry> = {},
): ProviderInstanceEntry {
  return {
    instanceId: ProviderInstanceId.make(instanceId),
    driverKind: "codex" as ProviderInstanceEntry["driverKind"],
    displayName: instanceId,
    accentColor: undefined,
    continuationGroupKey: undefined,
    enabled: true,
    installed: true,
    status: "ready",
    isDefault: true,
    isAvailable: true,
    snapshot: {} as ServerProvider,
    models: [],
    ...overrides,
  };
}

const codex = entry("codex", { displayName: "Codex" });
const claude = entry("claude", {
  driverKind: "claude" as ProviderInstanceEntry["driverKind"],
  displayName: "Claude",
});
const codexModels: ReadonlyArray<ModelEsque> = [
  { slug: "gpt-5", name: "GPT-5", shortName: "5" },
  { slug: "gpt-5-codex", name: "GPT-5 Codex", shortName: "Codex" },
];

function renderPicker(
  overrides: Partial<React.ComponentProps<typeof ProviderModelPicker>> = {},
): ReactElement {
  return (
    <ProviderModelPicker
      activeInstanceId={codex.instanceId}
      model="gpt-5"
      lockedProvider={null}
      instanceEntries={[codex, claude]}
      modelOptionsByInstance={new Map([[codex.instanceId, codexModels]])}
      onInstanceModelChange={onInstanceModelChange}
      {...overrides}
    />
  );
}

async function mount(element: ReactElement): Promise<MountedTree> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const mounted = { container, root };
  mountedTrees.push(mounted);
  await act(async () => root.render(element));
  return mounted;
}

async function click(container: HTMLElement, selector: string): Promise<void> {
  const element = container.querySelector<HTMLElement>(selector);
  if (element === null) throw new Error(`Missing ${selector}.`);
  await act(async () => element.click());
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  captured.contentProps = null;
  onInstanceModelChange.mockReset();
  onOpenChange.mockReset();
  document.documentElement.style.overscrollBehavior = "";
  document.body.style.overflow = "";
  document.body.style.paddingRight = "";
});

afterEach(async () => {
  for (const mounted of mountedTrees.splice(0)) {
    await act(async () => mounted.root.unmount());
    mounted.container.remove();
  }
  document.body.replaceChildren();
  vi.restoreAllMocks();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("ProviderModelPicker", () => {
  it("renders the active instance and selected model", async () => {
    const mounted = await mount(renderPicker());

    expect(mounted.container.querySelector("button[data-tooltip-trigger]")).not.toBeNull();
    expect(mounted.container.querySelector('[data-provider-icon="Codex"]')).toMatchObject({
      dataset: { showBadge: "false", iconClass: "size-4" },
    });
    expect(mounted.container.textContent).toBe("5Codex · GPT-5");
    expect(captured.contentProps).toMatchObject({
      activeInstanceId: codex.instanceId,
      model: "gpt-5",
      lockToActiveInstance: false,
      lockedProvider: null,
      lockedContinuationGroupKey: null,
      terminalOpen: false,
    });
  });

  it("locks duplicate provider instances to the active model and describes it fully", async () => {
    const work = entry("codex-work", { displayName: "Codex Work", isDefault: false });
    const workModels: ReadonlyArray<ModelEsque> = [
      { slug: "gpt-5-work", name: "GPT-5 Work", shortName: "Work" },
    ];
    const mounted = await mount(
      renderPicker({
        lockToActiveInstance: true,
        instanceEntries: [codex, work],
        modelOptionsByInstance: new Map([
          [codex.instanceId, codexModels],
          [work.instanceId, workModels],
        ]),
      }),
    );

    expect(mounted.container.querySelector("[data-tooltip]")?.textContent).toBe("Codex · GPT-5");
    await click(mounted.container, "[data-open-menu]");
    expect(captured.contentProps).toMatchObject({
      activeInstanceId: codex.instanceId,
      model: "gpt-5",
      lockToActiveInstance: true,
    });
  });

  it("falls back to the first active model and handles a missing instance", async () => {
    const mounted = await mount(renderPicker({ model: "foreign-model" }));
    expect(mounted.container.textContent).toBe("5Codex · GPT-5");

    const missingId = ProviderInstanceId.make("missing");
    await act(async () =>
      mounted.root.render(
        renderPicker({
          activeInstanceId: missingId,
          model: "raw-model",
          instanceEntries: [codex],
          modelOptionsByInstance: new Map(),
        }),
      ),
    );
    expect(mounted.container.querySelector("[data-provider-icon]")).toBeNull();
    expect(mounted.container.textContent).toContain("raw-model");
  });

  it("shows an instance badge for accent colors or duplicate drivers", async () => {
    const accented = entry("accented", { displayName: "Accented", accentColor: "#ff00aa" });
    const duplicate = entry("codex-work", { displayName: "Codex Work" });
    const mounted = await mount(
      renderPicker({
        activeInstanceId: accented.instanceId,
        instanceEntries: [accented, duplicate],
        modelOptionsByInstance: new Map([[accented.instanceId, codexModels]]),
      }),
    );

    expect(mounted.container.querySelector('[data-provider-icon="Accented"]')).toMatchObject({
      dataset: { showBadge: "true", iconClass: "h-5" },
    });
  });

  it("opens and closes an uncontrolled picker around selection", async () => {
    const mounted = await mount(renderPicker({ onOpenChange }));

    await click(mounted.container, "[data-open-menu]");
    expect(onOpenChange).toHaveBeenCalledWith(true);
    expect(
      mounted.container.querySelector("[data-popover-open]")?.getAttribute("data-popover-open"),
    ).toBe("true");

    await click(mounted.container, "[data-select-model]");
    expect(onInstanceModelChange).toHaveBeenCalledWith("claude", "opus");
    expect(onOpenChange).toHaveBeenLastCalledWith(false);
    expect(
      mounted.container.querySelector("[data-popover-open]")?.getAttribute("data-popover-open"),
    ).toBe("false");
  });

  it("keeps controlled state external and blocks disabled actions", async () => {
    const mounted = await mount(renderPicker({ open: true, disabled: true, onOpenChange }));

    await click(mounted.container, "[data-open-menu]");
    await click(mounted.container, "[data-select-model]");

    expect(onOpenChange).toHaveBeenCalledWith(false);
    expect(onInstanceModelChange).not.toHaveBeenCalled();
    expect(
      mounted.container.querySelector("[data-popover-open]")?.getAttribute("data-popover-open"),
    ).toBe("true");
  });

  it("passes optional content props and handles explicit close requests", async () => {
    const keybindings = [] as unknown as ResolvedKeybindingsConfig;
    const getModelDisabledReason = vi.fn(() => "Locked");
    const mounted = await mount(
      renderPicker({
        open: true,
        onOpenChange,
        keybindings,
        lockToActiveInstance: true,
        lockedContinuationGroupKey: "group-1",
        terminalOpen: true,
        getModelDisabledReason,
      }),
    );

    expect(captured.contentProps).toMatchObject({
      keybindings,
      lockToActiveInstance: true,
      lockedContinuationGroupKey: "group-1",
      terminalOpen: true,
      getModelDisabledReason,
    });
    await click(mounted.container, "[data-request-close]");
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("locks background scroll while open and allows picker overlay events", async () => {
    document.documentElement.style.overscrollBehavior = "auto";
    document.body.style.overflow = "scroll";
    document.body.style.paddingRight = "3px";
    const mounted = await mount(renderPicker({ open: true }));

    expect(document.documentElement.style.overscrollBehavior).toBe("contain");
    expect(document.body.style.overflow).toBe("hidden");
    expect(document.body.style.paddingRight).not.toBe("3px");

    const backgroundWheel = new WheelEvent("wheel", { bubbles: true, cancelable: true });
    document.body.dispatchEvent(backgroundWheel);
    expect(backgroundWheel.defaultPrevented).toBe(true);

    const overlayWheel = new WheelEvent("wheel", { bubbles: true, cancelable: true });
    mounted.container.querySelector("[data-model-picker-content]")!.dispatchEvent(overlayWheel);
    expect(overlayWheel.defaultPrevented).toBe(false);

    const backgroundTouch = new TouchEvent("touchmove", { bubbles: true, cancelable: true });
    document.body.dispatchEvent(backgroundTouch);
    expect(backgroundTouch.defaultPrevented).toBe(true);

    await act(async () => mounted.root.unmount());
    mountedTrees.splice(mountedTrees.indexOf(mounted), 1);
    expect(document.documentElement.style.overscrollBehavior).toBe("auto");
    expect(document.body.style.overflow).toBe("scroll");
    expect(document.body.style.paddingRight).toBe("3px");
  });
});
