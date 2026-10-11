// @vitest-environment happy-dom

import {
  act,
  cloneElement,
  type ComponentPropsWithoutRef,
  type ReactElement,
  type ReactNode,
} from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

vi.mock("lucide-react", () => ({
  ArrowLeft: () => <span data-icon="back" />,
  ArrowRight: () => <span data-icon="forward" />,
  Camera: () => <span data-icon="camera" />,
  ExternalLink: () => <span data-icon="external" />,
  MousePointerClick: () => <span data-icon="pointer" />,
  RotateCw: (props: { className?: string }) => <span data-icon="refresh" {...props} />,
}));

vi.mock("~/components/ui/button", () => ({
  Button: ({
    variant: _variant,
    size: _size,
    ...props
  }: ComponentPropsWithoutRef<"button"> & { variant?: string; size?: string }) => (
    <button {...props} />
  ),
}));

vi.mock("~/components/ui/input-group", async () => {
  const React = await import("react");
  return {
    InputGroup: ({ children, ...props }: ComponentPropsWithoutRef<"div">) => (
      <div {...props}>{children}</div>
    ),
    InputGroupAddon: ({
      align: _align,
      children,
      ...props
    }: ComponentPropsWithoutRef<"div"> & { align?: string }) => <div {...props}>{children}</div>,
    InputGroupInput: React.forwardRef<
      HTMLInputElement,
      ComponentPropsWithoutRef<"input"> & { size?: string }
    >(function InputGroupInput({ size: _size, ...props }, ref) {
      return <input ref={ref} {...props} />;
    }),
  };
});

vi.mock("~/components/ui/tooltip", () => ({
  Tooltip: ({ children }: { children?: ReactNode }) => <>{children}</>,
  TooltipPopup: ({ children }: { children?: ReactNode }) => <span data-tooltip>{children}</span>,
  TooltipTrigger: ({
    render,
    children,
  }: {
    render: ReactElement<Record<string, unknown>>;
    children?: ReactNode;
  }) => cloneElement(render, {}, children),
}));

vi.mock("~/lib/utils", () => ({
  cn: (...values: ReadonlyArray<unknown>) => values.filter(Boolean).join(" "),
}));

import { PreviewChromeRow } from "./PreviewChromeRow";

interface MountedTree {
  readonly container: HTMLDivElement;
  readonly root: Root;
}

const mountedTrees: MountedTree[] = [];

const callbacks = {
  onBack: vi.fn(),
  onForward: vi.fn(),
  onRefresh: vi.fn(),
  onSubmit: vi.fn(),
  onOpenInBrowser: vi.fn(),
  onCapture: vi.fn(),
  onPickElement: vi.fn(),
};

function renderRow(overrides: Partial<React.ComponentProps<typeof PreviewChromeRow>> = {}) {
  return (
    <PreviewChromeRow
      url="https://example.test/full/path"
      loading={false}
      loadProgress={0}
      canGoBack={false}
      canGoForward={false}
      refreshDisabled={false}
      onBack={callbacks.onBack}
      onForward={callbacks.onForward}
      onRefresh={callbacks.onRefresh}
      onSubmit={callbacks.onSubmit}
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

function button(container: HTMLElement, label: string): HTMLButtonElement {
  const result = container.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`);
  if (result === null) {
    throw new Error(`Missing button ${label}.`);
  }
  return result;
}

function input(container: HTMLElement): HTMLInputElement {
  const result = container.querySelector<HTMLInputElement>("[data-preview-url-input]");
  if (result === null) {
    throw new Error("Missing preview URL input.");
  }
  return result;
}

async function click(element: HTMLElement, options: MouseEventInit = {}): Promise<void> {
  await act(async () => {
    element.dispatchEvent(new MouseEvent("click", { bubbles: true, ...options }));
  });
}

async function changeInput(element: HTMLInputElement, value: string): Promise<void> {
  const valueSetter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  await act(async () => {
    valueSetter?.call(element, value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  for (const callback of Object.values(callbacks)) {
    callback.mockReset();
  }
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

describe("PreviewChromeRow", () => {
  it("enables only available navigation actions", async () => {
    const mounted = await mount(renderRow({ canGoBack: true, canGoForward: false }));
    const back = button(mounted.container, "Back");
    const forward = button(mounted.container, "Forward");

    await click(back);
    await click(forward);

    expect(back.disabled).toBe(false);
    expect(forward.disabled).toBe(true);
    expect(callbacks.onBack).toHaveBeenCalledTimes(1);
    expect(callbacks.onForward).not.toHaveBeenCalled();
  });

  it("switches refresh presentation while loading and honors disablement", async () => {
    const mounted = await mount(
      renderRow({ loading: true, loadProgress: 42, refreshDisabled: true }),
    );
    const refresh = button(mounted.container, "Stop");

    await click(refresh);

    expect(refresh.disabled).toBe(true);
    expect(callbacks.onRefresh).not.toHaveBeenCalled();
    expect(mounted.container.querySelector("[data-icon=refresh]")?.className).toContain(
      "animate-spin",
    );
    expect(mounted.container.querySelector<HTMLElement>("[aria-hidden]")?.style.width).toBe("42%");
  });

  it("submits a trimmed edited URL on Enter and blurs the input", async () => {
    const mounted = await mount(
      renderRow({ displayUrl: "example.test", onOpenInBrowser: callbacks.onOpenInBrowser }),
    );
    const urlInput = input(mounted.container);
    const select = vi.spyOn(urlInput, "select");

    await act(async () => {
      urlInput.focus();
      await Promise.resolve();
    });
    expect(urlInput.value).toBe("https://example.test/full/path");
    expect(select).toHaveBeenCalledTimes(1);

    await changeInput(urlInput, "  https://new.test/page  ");
    await act(async () => {
      urlInput.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    });

    expect(callbacks.onSubmit).toHaveBeenCalledWith("https://new.test/page");
    expect(document.activeElement).not.toBe(urlInput);
    expect(urlInput.value).toBe("example.test");
  });

  it("ignores empty submission and restores the canonical URL on Escape", async () => {
    const mounted = await mount(renderRow());
    const urlInput = input(mounted.container);
    await act(async () => urlInput.focus());

    await changeInput(urlInput, "   ");
    await act(async () => {
      urlInput.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    });
    expect(callbacks.onSubmit).not.toHaveBeenCalled();

    await changeInput(urlInput, "https://discard.test");
    await act(async () => {
      urlInput.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    });
    expect(document.activeElement).not.toBe(urlInput);
    expect(urlInput.value).toBe("https://example.test/full/path");
  });

  it("focuses the URL input when the nonce changes", async () => {
    const mounted = await mount(renderRow({ focusUrlNonce: 1 }));
    const urlInput = input(mounted.container);
    expect(document.activeElement).toBe(urlInput);

    await act(async () => mounted.root.render(renderRow({ focusUrlNonce: 2 })));
    expect(document.activeElement).toBe(urlInput);
  });

  it("names the browser action after where it opens", async () => {
    // Browser mode (no desktop host): the system browser is this one.
    let mounted = await mount(
      renderRow({ displayUrl: "example.test", onOpenInBrowser: callbacks.onOpenInBrowser }),
    );
    expect(button(mounted.container, "Open in new tab")).toBeTruthy();
    mounted.root.unmount();

    window.desktopBridge = {} as never;
    mounted = await mount(
      renderRow({ displayUrl: "example.test", onOpenInBrowser: callbacks.onOpenInBrowser }),
    );
    delete window.desktopBridge;
    expect(button(mounted.container, "Open in system browser")).toBeTruthy();
  });

  it("shows and invokes optional browser and annotation actions", async () => {
    window.desktopBridge = {} as never;
    const mounted = await mount(
      renderRow({
        displayUrl: "example.test",
        onOpenInBrowser: callbacks.onOpenInBrowser,
        onPickElement: callbacks.onPickElement,
        pickActive: true,
      }),
    );

    delete window.desktopBridge;
    await click(button(mounted.container, "Open in system browser"));
    await click(button(mounted.container, "Cancel annotation"));

    expect(callbacks.onOpenInBrowser).toHaveBeenCalledTimes(1);
    expect(callbacks.onPickElement).toHaveBeenCalledTimes(1);
    expect(button(mounted.container, "Cancel annotation").getAttribute("aria-pressed")).toBe(
      "true",
    );
  });

  it("passes shift state to capture and renders trailing actions", async () => {
    const mounted = await mount(
      renderRow({
        onCapture: callbacks.onCapture,
        recording: false,
        trailingActions: <button data-trailing>More</button>,
      }),
    );

    await click(button(mounted.container, "Capture screenshot"), { shiftKey: true });

    expect(callbacks.onCapture).toHaveBeenCalledWith(true);
    expect(mounted.container.querySelector("[data-trailing]")?.textContent).toBe("More");
  });

  it("uses screenshot-only behavior when recording is unsupported", async () => {
    const mounted = await mount(
      renderRow({
        onCapture: callbacks.onCapture,
        recording: false,
        recordingSupported: false,
      }),
    );

    await click(button(mounted.container, "Capture screenshot"), { shiftKey: true });

    expect(callbacks.onCapture).toHaveBeenCalledWith(false);
    expect(mounted.container.textContent).toContain("Capture screenshot");
    expect(mounted.container.textContent).not.toContain("Shift-click to record");
  });

  it("disables optional actions and reports their active labels", async () => {
    const mounted = await mount(
      renderRow({
        onCapture: callbacks.onCapture,
        captureDisabled: true,
        recording: true,
        onPickElement: callbacks.onPickElement,
        pickDisabled: true,
        pickDisabledReason: "Connect the preview first",
      }),
    );

    const capture = button(mounted.container, "Stop recording");
    const pick = button(mounted.container, "Annotate preview");
    await click(capture);
    await click(pick);

    expect(capture.disabled).toBe(true);
    expect(pick.disabled).toBe(true);
    expect(callbacks.onCapture).not.toHaveBeenCalled();
    expect(callbacks.onPickElement).not.toHaveBeenCalled();
    expect(mounted.container.textContent).toContain("Connect the preview first");
  });
});
