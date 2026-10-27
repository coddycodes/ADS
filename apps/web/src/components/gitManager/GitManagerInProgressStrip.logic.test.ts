// @vitest-environment happy-dom

import type { GitManagerBlockedReason, GitManagerInProgressOperation } from "@coddy/contracts";
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

vi.mock("~/components/ui/dialog", () => ({
  Dialog: ({ children }: { children: React.ReactNode }) => createElement("div", null, children),
  DialogDescription: ({ children }: { children: React.ReactNode }) =>
    createElement("p", null, children),
  DialogFooter: ({ children }: { children: React.ReactNode }) =>
    createElement("footer", null, children),
  DialogHeader: ({ children }: { children: React.ReactNode }) =>
    createElement("header", null, children),
  DialogPopup: ({ children }: { children: React.ReactNode }) =>
    createElement("section", null, children),
  DialogTitle: ({ children }: { children: React.ReactNode }) => createElement("h2", null, children),
}));

import { GitManagerInProgressStrip } from "./GitManagerInProgressStrip";
import {
  describeInProgressOperation,
  resolveInProgressBlockedReason,
} from "./GitManagerInProgressStrip.logic";

const kinds = ["merge", "rebase", "cherry-pick", "revert"] as const;
const blocked: GitManagerBlockedReason = {
  operation: "stash-apply",
  code: "merge-in-progress",
  message: "Server says another mutation must wait for conflict resolution.",
};

function operation(kind: GitManagerInProgressOperation["kind"]): GitManagerInProgressOperation {
  return { kind, current: kind === "rebase" ? 2 : null, total: kind === "rebase" ? 5 : null };
}

let container: HTMLDivElement;
let root: Root | null;

async function renderStrip(
  value: GitManagerInProgressOperation,
  onContinue = vi.fn(),
  onAbort = vi.fn(),
) {
  await act(async () =>
    root?.render(
      createElement(GitManagerInProgressStrip, {
        operation: value,
        blocked,
        onContinue,
        onAbort,
      }),
    ),
  );
  return { onContinue, onAbort };
}

function buttonWithText(text: string): HTMLButtonElement {
  const button = [...container.querySelectorAll("button")].find(
    (candidate) => candidate.textContent === text,
  );
  if (!(button instanceof HTMLButtonElement)) throw new Error(`Missing button: ${text}`);
  return button;
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

describe("describeInProgressOperation", () => {
  it("covers every externally resumable operation kind", () => {
    expect(kinds.map((kind) => describeInProgressOperation(operation(kind)).label)).toEqual([
      "Merge underway",
      "Rebase underway",
      "Cherry-pick underway",
      "Revert underway",
    ]);
    expect(describeInProgressOperation(operation("rebase")).progress).toBe("Step 2 of 5");
  });

  it("passes the server's blocking reason through verbatim", () => {
    expect(resolveInProgressBlockedReason(blocked)).toBe(blocked.message);
    expect(resolveInProgressBlockedReason(null)).toBeNull();
  });
});

describe("GitManagerInProgressStrip", () => {
  it("renders a non-dismissable alert with Continue and Abort for every kind", async () => {
    for (const kind of kinds) {
      await renderStrip(operation(kind));
      expect(container.querySelector('[role="alert"]')).not.toBeNull();
      expect(buttonWithText("Continue")).toBeInstanceOf(HTMLButtonElement);
      expect(buttonWithText("Abort")).toBeInstanceOf(HTMLButtonElement);
      expect(container.querySelector('button[aria-label="Dismiss"]')).toBeNull();
      expect(container.textContent).toContain(blocked.message);
    }
  });

  it("places Abort behind confirmation while Continue invokes directly", async () => {
    const { onContinue, onAbort } = await renderStrip(operation("merge"));

    await act(async () => buttonWithText("Continue").click());
    expect(onContinue).toHaveBeenCalledOnce();
    await act(async () => buttonWithText("Abort").click());
    expect(onAbort).not.toHaveBeenCalled();
    expect(container.textContent).toContain("Abort merge?");
    await act(async () => buttonWithText("Abort Merge").click());
    expect(onAbort).toHaveBeenCalledOnce();
  });

  it("stays visible when reconnect publishes a fresh object for the same operation", async () => {
    await renderStrip(operation("cherry-pick"));
    expect(container.querySelector('[role="alert"]')?.textContent).toContain(
      "Cherry-pick underway",
    );

    await renderStrip({ ...operation("cherry-pick") });
    expect(container.querySelector('[role="alert"]')?.textContent).toContain(
      "Cherry-pick underway",
    );
  });
});
