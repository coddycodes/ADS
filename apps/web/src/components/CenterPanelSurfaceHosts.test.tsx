// @vitest-environment happy-dom

import { act, forwardRef, useEffect, useImperativeHandle, type RefObject } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import type { ThreadCenterPanelState } from "~/centerPanelStore";

import {
  CenterPanelSurfaceHosts,
  type CenterPanelBodyTargetRegistry,
  type CenterPanelSurfaceHostsHandle,
  useCenterPanelBodyTargets,
} from "./CenterPanelSurfaceHosts";

interface RectInput {
  readonly left: number;
  readonly top: number;
  readonly width: number;
  readonly height: number;
}

const host = { id: "chat:host", kind: "chat-host" } as const;
const terminal = {
  id: "terminal:term-1",
  kind: "terminal",
  terminalId: "term-1",
} as const;
const stateWithTerminalInLeft: ThreadCenterPanelState = {
  surfaces: [host, terminal],
  groups: [
    { id: "group-left", surfaceIds: [host.id, terminal.id], activeSurfaceId: terminal.id },
    { id: "group-right", surfaceIds: [], activeSurfaceId: null },
  ],
  layout: {
    type: "split",
    direction: "horizontal",
    ratio: 0.5,
    first: { type: "leaf", groupId: "group-left" },
    second: { type: "leaf", groupId: "group-right" },
  },
  focusedGroupId: "group-left",
};

const stateWithTerminalInRight: ThreadCenterPanelState = {
  ...stateWithTerminalInLeft,
  groups: [
    { id: "group-left", surfaceIds: [host.id], activeSurfaceId: host.id },
    { id: "group-right", surfaceIds: [terminal.id], activeSurfaceId: terminal.id },
  ],
  focusedGroupId: "group-right",
};

const stateWithInactiveHost: ThreadCenterPanelState = {
  ...stateWithTerminalInLeft,
  groups: [{ id: "group-left", surfaceIds: [host.id, terminal.id], activeSurfaceId: terminal.id }],
};

let container: HTMLDivElement;
let root: Root;
let frameCallbacks: FrameRequestCallback[];
let resizeObservers: FakeResizeObserver[];

class FakeResizeObserver {
  readonly observed = new Set<Element>();

  constructor(readonly callback: ResizeObserverCallback) {
    resizeObservers.push(this);
  }

  observe(target: Element): void {
    this.observed.add(target);
  }

  unobserve(target: Element): void {
    this.observed.delete(target);
  }

  disconnect(): void {
    this.observed.clear();
  }
}

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  frameCallbacks = [];
  resizeObservers = [];
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
    frameCallbacks.push(callback);
    return frameCallbacks.length;
  });
  vi.stubGlobal("cancelAnimationFrame", vi.fn());
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  document.body.replaceChildren();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function rect(input: RectInput): DOMRect {
  return {
    ...input,
    right: input.left + input.width,
    bottom: input.top + input.height,
    x: input.left,
    y: input.top,
    toJSON: () => ({}),
  } as DOMRect;
}

function targets(rects: ReadonlyMap<string, RectInput>) {
  return new Map(rects);
}

async function renderHosts(
  state: ThreadCenterPanelState,
  targetRects: ReadonlyMap<string, RectInput>,
  options: {
    readonly mounted?: (id: string) => void;
    readonly onFocusGroup?: (groupId: string) => void;
  } = {},
): Promise<RefObject<CenterPanelSurfaceHostsHandle | null>> {
  const hostsRef = { current: null } as RefObject<CenterPanelSurfaceHostsHandle | null>;
  const surfaceMounts = options.mounted ?? vi.fn<(id: string) => void>();
  const onFocusGroup = options.onFocusGroup ?? vi.fn<(groupId: string) => void>();
  const renderSurface = (surface: (typeof state.surfaces)[number]) => (
    <Surface id={surface.id} mounted={surfaceMounts} />
  );
  await act(async () => {
    root.render(
      <CenterPanelSurfaceHosts
        ref={hostsRef}
        state={state}
        rects={targets(targetRects)}
        readBodyRect={(groupId) => targetRects.get(groupId) ?? null}
        onFocusGroup={onFocusGroup}
        renderSurface={renderSurface}
      />,
    );
  });
  return hostsRef;
}

function Surface({ id, mounted }: { readonly id: string; readonly mounted: (id: string) => void }) {
  useEffect(() => {
    mounted(id);
    return () => mounted(`unmount:${id}`);
  }, [id, mounted]);
  return <div data-surface-instance={id} />;
}

const BodyTargetHarness = forwardRef<
  CenterPanelBodyTargetRegistry,
  { readonly groups?: ThreadCenterPanelState["groups"]; readonly targetKey?: string }
>(function BodyTargetHarness({ groups = stateWithTerminalInLeft.groups, targetKey = "body" }, ref) {
  const registry = useCenterPanelBodyTargets(groups);
  const { registerBodyTarget, rootRef } = registry;
  useImperativeHandle(ref, () => registry, [registry]);
  return (
    <div ref={rootRef} data-target="root">
      <div key={targetKey} ref={registerBodyTarget("group-left")} data-target="group-left" />
    </div>
  );
});

describe("CenterPanelSurfaceHosts", () => {
  it("keeps a visible terminal host mounted while its group target changes", async () => {
    const mounted = vi.fn<(id: string) => void>();
    const targetRects = new Map<string, RectInput>([
      ["group-left", { left: 10, top: 20, width: 320, height: 360 }],
      ["group-right", { left: 340, top: 20, width: 320, height: 360 }],
    ]);

    await renderHosts(stateWithTerminalInLeft, targetRects, { mounted });
    const before = container.querySelector('[data-center-surface-host="terminal:term-1"]');
    const beforeChild = before?.querySelector('[data-surface-instance="terminal:term-1"]');
    await renderHosts(stateWithTerminalInRight, targetRects, { mounted });
    const after = container.querySelector('[data-center-surface-host="terminal:term-1"]');
    const afterChild = after?.querySelector('[data-surface-instance="terminal:term-1"]');

    expect(after).toBe(before);
    expect(afterChild).toBe(beforeChild);
    expect(mounted).toHaveBeenCalledWith("terminal:term-1");
    expect(mounted).not.toHaveBeenCalledWith("unmount:terminal:term-1");
  });

  it("keeps the inactive host mounted but hidden", async () => {
    await renderHosts(
      stateWithInactiveHost,
      new Map([["group-left", { left: 10, top: 20, width: 320, height: 360 }]]),
    );

    expect(
      container
        .querySelector('[data-center-surface-host="chat:host"]')
        ?.getAttribute("data-visible"),
    ).toBe("false");
  });

  it("updates wrapper geometry imperatively without rerendering surface content", async () => {
    const mounted = vi.fn<(id: string) => void>();
    const targetRects = new Map<string, RectInput>([
      ["group-left", { left: 10, top: 20, width: 320, height: 360 }],
      ["group-right", { left: 340, top: 20, width: 320, height: 360 }],
    ]);
    const hostsRef = await renderHosts(stateWithTerminalInLeft, targetRects, { mounted });
    targetRects.set("group-left", { left: 20, top: 40, width: 640, height: 360 });

    act(() => hostsRef.current?.syncRects());
    const surfaceHost = container.querySelector<HTMLElement>(
      '[data-center-surface-host="terminal:term-1"]',
    );
    expect(surfaceHost?.style.left).toBe("20px");
    expect(surfaceHost?.style.width).toBe("640px");
    expect(mounted.mock.calls.filter(([id]) => id === "terminal:term-1")).toHaveLength(1);
  });

  it("hides a host with missing geometry and restores the same mounted host when geometry returns", async () => {
    const mounted = vi.fn<(id: string) => void>();
    const targetRects = new Map<string, RectInput>([
      ["group-left", { left: 10, top: 20, width: 320, height: 360 }],
    ]);
    const hostsRef = await renderHosts(stateWithTerminalInLeft, targetRects, { mounted });
    const before = container.querySelector<HTMLElement>(
      '[data-center-surface-host="terminal:term-1"]',
    );
    const beforeChild = before?.querySelector('[data-surface-instance="terminal:term-1"]');
    targetRects.delete("group-left");

    act(() => hostsRef.current?.syncRects());
    expect(before?.dataset.centerSurfaceGeometry).toBe("invalid");
    expect(before?.dataset.visible).toBe("true");
    expect(before?.style.visibility).toBe("hidden");
    expect(before?.style.pointerEvents).toBe("none");
    expect(before?.style.left).toBe("");

    targetRects.set("group-left", { left: 40, top: 60, width: 640, height: 360 });
    act(() => hostsRef.current?.syncRects());
    const after = container.querySelector<HTMLElement>(
      '[data-center-surface-host="terminal:term-1"]',
    );
    expect(after).toBe(before);
    expect(after?.querySelector('[data-surface-instance="terminal:term-1"]')).toBe(beforeChild);
    expect(after?.dataset.centerSurfaceGeometry).toBe("valid");
    expect(after?.dataset.visible).toBe("true");
    expect(after?.style.left).toBe("40px");
    expect(after?.style.visibility).toBe("");
    expect(after?.style.pointerEvents).toBe("");
    expect(mounted).not.toHaveBeenCalledWith("unmount:terminal:term-1");
  });

  it("does not render orphan surface descriptors", async () => {
    await expect(
      renderHosts(
        {
          ...stateWithTerminalInLeft,
          groups: [{ id: "group-left", surfaceIds: [host.id], activeSurfaceId: host.id }],
          layout: { type: "leaf", groupId: "group-left" },
        },
        new Map([["group-left", { left: 10, top: 20, width: 320, height: 360 }]]),
      ),
    ).resolves.toBeDefined();

    expect(container.querySelector('[data-center-surface-host="terminal:term-1"]')).toBeNull();
  });

  it("focuses an unfocused owning group from a surface pointer", async () => {
    const onFocusGroup = vi.fn<(groupId: string) => void>();
    await renderHosts(
      { ...stateWithTerminalInLeft, focusedGroupId: "group-right" },
      new Map([["group-left", { left: 10, top: 20, width: 320, height: 360 }]]),
      { onFocusGroup },
    );
    const surfaceHost = container.querySelector<HTMLElement>(
      '[data-center-surface-host="terminal:term-1"]',
    );
    if (!surfaceHost) throw new Error("Terminal host not found");

    surfaceHost.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    expect(onFocusGroup).toHaveBeenCalledWith("group-left");
  });

  it("focuses an unfocused owning group from surface keyboard focus", async () => {
    const onFocusGroup = vi.fn<(groupId: string) => void>();
    await renderHosts(
      { ...stateWithTerminalInLeft, focusedGroupId: "group-right" },
      new Map([["group-left", { left: 10, top: 20, width: 320, height: 360 }]]),
      { onFocusGroup },
    );
    const surfaceHost = container.querySelector<HTMLElement>(
      '[data-center-surface-host="terminal:term-1"]',
    );
    if (!surfaceHost) throw new Error("Terminal host not found");

    surfaceHost.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    expect(onFocusGroup).toHaveBeenCalledWith("group-left");
  });

  it("does not write focus ownership again for the already focused group", async () => {
    const onFocusGroup = vi.fn<(groupId: string) => void>();
    await renderHosts(
      stateWithTerminalInLeft,
      new Map([["group-left", { left: 10, top: 20, width: 320, height: 360 }]]),
      { onFocusGroup },
    );
    const surfaceHost = container.querySelector<HTMLElement>(
      '[data-center-surface-host="terminal:term-1"]',
    );
    if (!surfaceHost) throw new Error("Terminal host not found");

    surfaceHost.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    surfaceHost.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    expect(onFocusGroup).not.toHaveBeenCalled();
  });

  it("publishes initial targets before a deferred animation frame", async () => {
    const registryRef = { current: null } as RefObject<CenterPanelBodyTargetRegistry | null>;
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
      function (this: HTMLElement) {
        return this.dataset.target === "root"
          ? rect({ left: 100, top: 200, width: 800, height: 600 })
          : rect({ left: 140, top: 260, width: 320, height: 360 });
      },
    );
    await act(async () => root.render(<BodyTargetHarness ref={registryRef} />));
    expect(registryRef.current?.rects.get("group-left")).toEqual({
      left: 40,
      top: 60,
      width: 320,
      height: 360,
    });
  });

  it("refreshes geometry on panel activation without waiting for paint", async () => {
    const registryRef = { current: null } as RefObject<CenterPanelBodyTargetRegistry | null>;
    let left = 140;
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
      function (this: HTMLElement) {
        return this.dataset.target === "root"
          ? rect({ left: 100, top: 200, width: 800, height: 600 })
          : rect({ left, top: 260, width: 320, height: 360 });
      },
    );
    await act(async () => root.render(<BodyTargetHarness ref={registryRef} />));
    await act(async () => frameCallbacks.shift()?.(0));
    left = 180;
    await act(async () =>
      root.render(<BodyTargetHarness ref={registryRef} groups={stateWithTerminalInRight.groups} />),
    );
    expect(registryRef.current?.rects.get("group-left")?.left).toBe(80);
  });

  it("refreshes a replaced body target with unchanged activation state", async () => {
    const registryRef = { current: null } as RefObject<CenterPanelBodyTargetRegistry | null>;
    let left = 140;
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
      function (this: HTMLElement) {
        return this.dataset.target === "root"
          ? rect({ left: 100, top: 200, width: 800, height: 600 })
          : rect({ left, top: 260, width: 320, height: 360 });
      },
    );
    await act(async () => root.render(<BodyTargetHarness ref={registryRef} />));
    left = 200;
    await act(async () =>
      root.render(<BodyTargetHarness ref={registryRef} targetKey="replacement" />),
    );
    expect(registryRef.current?.rects.get("group-left")?.left).toBe(100);
  });

  it("does not measure again for equivalent group activation state", async () => {
    const measure = vi
      .spyOn(HTMLElement.prototype, "getBoundingClientRect")
      .mockReturnValue(rect({ left: 0, top: 0, width: 800, height: 600 }));
    await act(async () => root.render(<BodyTargetHarness />));
    measure.mockClear();
    await act(async () =>
      root.render(
        <BodyTargetHarness
          groups={stateWithTerminalInLeft.groups.map((group) => ({ ...group }))}
        />,
      ),
    );
    expect(measure).not.toHaveBeenCalled();
  });

  it("publishes root-relative body rectangles and reads the current rectangle synchronously", async () => {
    const registryRef = { current: null } as RefObject<CenterPanelBodyTargetRegistry | null>;
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
      function (this: HTMLElement) {
        return this.dataset.target === "root"
          ? rect({ left: 100, top: 200, width: 800, height: 600 })
          : rect({ left: 140, top: 260, width: 320, height: 360 });
      },
    );

    await act(async () => root.render(<BodyTargetHarness ref={registryRef} />));
    await act(async () => frameCallbacks.shift()?.(0));

    expect(registryRef.current?.rects.get("group-left")).toEqual({
      left: 40,
      top: 60,
      width: 320,
      height: 360,
    });
    expect(registryRef.current?.readBodyRect("group-left")).toEqual({
      left: 40,
      top: 60,
      width: 320,
      height: 360,
    });
  });

  it("uses one observer and batches repeated resize notifications into one frame", async () => {
    const registryRef = { current: null } as RefObject<CenterPanelBodyTargetRegistry | null>;
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
      function (this: HTMLElement) {
        return this.dataset.target === "root"
          ? rect({ left: 100, top: 200, width: 800, height: 600 })
          : rect({ left: 140, top: 260, width: 320, height: 360 });
      },
    );

    await act(async () => root.render(<BodyTargetHarness ref={registryRef} />));
    expect(resizeObservers).toHaveLength(1);
    expect(resizeObservers[0]?.observed).toHaveLength(2);
    await act(async () => frameCallbacks.shift()?.(0));
    resizeObservers[0]?.callback([], resizeObservers[0] as unknown as ResizeObserver);
    resizeObservers[0]?.callback([], resizeObservers[0] as unknown as ResizeObserver);

    expect(frameCallbacks).toHaveLength(1);
  });
});
