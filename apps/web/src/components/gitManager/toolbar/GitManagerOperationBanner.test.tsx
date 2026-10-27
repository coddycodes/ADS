// @vitest-environment happy-dom

import type { GitManagerOperationEvent } from "@coddy/contracts";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { GitManagerOperationBanner } from "./GitManagerOperationBanner";

let container: HTMLDivElement;
let root: Root | null;

async function emit(operation: GitManagerOperationEvent | null, onCancel = () => undefined) {
  await act(async () =>
    root?.render(<GitManagerOperationBanner operation={operation} onCancel={onCancel} />),
  );
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root?.unmount());
  root = null;
  container.remove();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("GitManagerOperationBanner", () => {
  it("tracks started, chunked output, finished and failed events", async () => {
    await emit({ _tag: "started", operation: "fetch" });
    expect(document.querySelector('[role="status"]')?.textContent).toContain("fetch");

    await emit({ _tag: "output", operation: "fetch", stream: "stdout", text: "chunk one\n" });
    const toggle = document.querySelector('button[aria-expanded="false"]');
    expect(toggle).toBeInstanceOf(HTMLButtonElement);
    expect(document.querySelector("[data-operation-output]")?.hasAttribute("hidden")).toBe(true);
    await act(async () => (toggle as HTMLButtonElement).click());
    expect(document.querySelector("[data-operation-output]")?.textContent).toContain("chunk one");

    await emit({ _tag: "finished", operation: "fetch", message: "Fetched." });
    expect(document.querySelector('[role="status"]')).toBeNull();

    await emit({
      _tag: "failed",
      operation: "push",
      code: "non-fast-forward",
      message: "The remote rejected this push.",
      blocked: null,
    });
    expect(document.querySelector('[role="status"]')?.textContent).toContain(
      "The remote rejected this push.",
    );
    expect(document.body.textContent).toContain("non-fast-forward");
  });

  it("invokes the abort path from Cancel", async () => {
    const onCancel = vi.fn();
    await emit({ _tag: "started", operation: "pull" }, onCancel);

    const cancel = [...document.querySelectorAll("button")].find(
      (button) => button.textContent === "Cancel",
    );
    expect(cancel).toBeInstanceOf(HTMLButtonElement);
    await act(async () => cancel?.click());
    expect(onCancel).toHaveBeenCalledOnce();
  });

  it("keeps output collapsed by default and expands it from the keyboard", async () => {
    await emit({ _tag: "started", operation: "fetch" });
    await emit({ _tag: "output", operation: "fetch", stream: "stderr", text: "chunk\n" });
    const toggle = document.querySelector('button[aria-expanded="false"]');
    expect(toggle).toBeInstanceOf(HTMLButtonElement);

    await act(async () => {
      toggle?.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
      toggle?.dispatchEvent(new KeyboardEvent("keyup", { key: "Enter", bubbles: true }));
      (toggle as HTMLButtonElement).click();
    });

    expect(toggle?.getAttribute("aria-expanded")).toBe("true");
  });
});
