// @vitest-environment happy-dom
import type { PullRequestsFile } from "@coddy/contracts";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import patches from "../../../../../server/tests/fixtures/pull_requests/file_patches.json";
// These wire patches are the exact file shapes produced by the Phase 03
// github/detail_tests.rs split-patch and gitlab/detail_tests.rs rename fixtures.
import { PullRequestsFileDiff } from "./PullRequestsFileDiff";
import { file } from "./testFixtures";
let root: Root;
let container: HTMLDivElement;
let intersect: IntersectionObserverCallback;
const disconnect = vi.fn();
const observe = vi.fn();
const toggle = vi.fn();
beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.stubGlobal(
    "IntersectionObserver",
    class {
      constructor(callback: IntersectionObserverCallback, options: IntersectionObserverInit) {
        intersect = callback;
        expect(options.rootMargin).toBe("600px");
      }
      observe = observe;
      disconnect = disconnect;
    },
  );
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  vi.clearAllMocks();
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});
async function render(value: PullRequestsFile = file, ignoreWhitespace = false) {
  await act(async () =>
    root.render(
      <PullRequestsFileDiff
        file={value}
        hostUrl="https://example.com/pr/14"
        viewed={false}
        onToggleViewed={toggle}
        ignoreWhitespace={ignoreWhitespace}
      />,
    ),
  );
}
async function show() {
  await act(async () =>
    intersect([{ isIntersecting: true } as IntersectionObserverEntry], {} as IntersectionObserver),
  );
}
function diffText() {
  return [...container.querySelectorAll("diffs-container")]
    .map((element) => element.shadowRoot?.textContent ?? "")
    .join("\n");
}
describe("PullRequestsFileDiff", () => {
  it("renders a real annotation slot below the selected line", async () => {
    await act(async () =>
      root.render(
        <PullRequestsFileDiff
          file={file}
          hostUrl="https://example.com/pr/14"
          viewed={false}
          onToggleViewed={toggle}
          ignoreWhitespace={false}
          annotations={[{ side: "additions", lineNumber: 1, metadata: <p>Anchored comment</p> }]}
        />,
      ),
    );
    await show();
    await vi.waitFor(() =>
      expect(
        container
          .querySelector("diffs-container")
          ?.shadowRoot?.querySelector('slot[name="annotation-additions-1"]'),
      ).not.toBeNull(),
    );
    expect(container.querySelector('[slot="annotation-additions-1"]')?.textContent).toBe(
      "Anchored comment",
    );
  });
  it("keeps unanchorable annotations visible when a patch becomes unavailable", async () => {
    await act(async () =>
      root.render(
        <PullRequestsFileDiff
          file={{ ...file, patch: null }}
          hostUrl="https://example.com/pr/14"
          viewed={false}
          onToggleViewed={toggle}
          ignoreWhitespace={false}
          annotations={[
            { side: "additions", lineNumber: 1, metadata: <p>Keep my pending comment</p> },
          ]}
        />,
      ),
    );
    expect(container.textContent).toContain("Keep my pending comment");
  });
  it.each(["github", "gitlab"] as const)(
    "renders the actual %s server patch with real FileDiff only after intersection",
    async (host) => {
      await render(patches[host] as PullRequestsFile);
      expect(observe).toHaveBeenCalledOnce();
      expect(container.querySelector("diffs-container")).toBeNull();
      await show();
      await vi.waitFor(() => {
        expect(container.querySelector("diffs-container")).not.toBeNull();
        expect(diffText()).toContain("old");
        expect(diffText()).toContain("new");
      });
      expect(container.textContent).not.toContain("Failed to parse");
      expect(disconnect).toHaveBeenCalled();
    },
  );
  it("shows tooLarge and binary metadata without attempting a renderer", async () => {
    await render({ ...file, tooLarge: true, patch: null });
    expect(container.textContent).toContain("Diff too large to render — open on host");
    expect(container.querySelector("a")?.href).toBe("https://example.com/pr/14");
    await render({ ...file, changeType: "binary", patch: null });
    expect(container.textContent).toContain("Binary file");
    expect(container.querySelector("diffs-container")).toBeNull();
  });
  it("degrades long lines to raw text without mounting FileDiff", async () => {
    await render({
      ...file,
      patch: `--- a/src/a.ts\n+++ b/src/a.ts\n@@ -1 +1 @@\n-old\n+${"x".repeat(5001)}\n`,
    });
    await show();
    expect(container.querySelector("pre")?.textContent).toContain("x".repeat(5001));
    expect(container.querySelector("diffs-container")).toBeNull();
  });
  it("exposes Viewed and cleans up an unused observer", async () => {
    await render();
    await act(async () =>
      container.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click(),
    );
    expect(toggle).toHaveBeenCalledWith(file.path);
    await act(async () => root.render(null));
    expect(disconnect).toHaveBeenCalled();
  });
  it("rejects the parse cap before mounting a diff", async () => {
    await render({ ...file, patch: "x".repeat(70_000_000) });
    await show();
    expect(container.textContent).toContain("Diff too large to render — open on host");
    expect(container.querySelector("diffs-container")).toBeNull();
  });
  it("gates large text with Show diff anyway before parsing", async () => {
    await render({
      ...file,
      patch: `--- a/src/a.ts\n+++ b/src/a.ts\n@@ -0,0 +1,22000 @@\n${("+" + "x".repeat(200) + "\n").repeat(22000)}`,
    });
    await show();
    expect(container.textContent).toContain("Show diff anyway");
    expect(container.querySelector("diffs-container")).toBeNull();
  });
  it("supports a manual mount if IntersectionObserver is unavailable", async () => {
    vi.stubGlobal("IntersectionObserver", undefined);
    await render();
    await act(async () =>
      [...container.querySelectorAll("button")]
        .find((button) => button.textContent === "Show diff")!
        .click(),
    );
    expect(container.querySelector("diffs-container")).not.toBeNull();
  });
  it("shows malformed patches as plain text", async () => {
    await render({ ...file, patch: "not a unified diff" });
    await show();
    expect(container.querySelector("pre")?.textContent).toBe("not a unified diff");
    expect(container.querySelector("diffs-container")).toBeNull();
  });
});
