// @vitest-environment happy-dom
import type { EnvironmentThreadShell } from "@bibcode/client-runtime/state/models";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { selectThreadRightPanelState, useRightPanelStore } from "../../rightPanelStore";
import { OpenBesideThreadButton } from "./OpenBesideThreadButton";
import { click, mount, projectRef } from "./review/testHelpers";

const h = vi.hoisted(() => ({
  threads: [] as unknown[],
  navigate: vi.fn(),
}));
vi.mock("@tanstack/react-router", () => ({ useNavigate: () => h.navigate }));
vi.mock("../../state/entities", () => ({ useThreadShellsForProjectRefs: () => h.threads }));

const thread = (id: string, branch: string | null, updatedAt: string) =>
  ({
    id,
    environmentId: "env",
    title: `Thread ${id}`,
    branch,
    updatedAt,
    archivedAt: null,
  }) as unknown as EnvironmentThreadShell;

const triggerLabel = "Open beside a thread…";
const noThreadReason = "This project has no thread to open it beside.";
const element = (
  <OpenBesideThreadButton
    projectRef={projectRef}
    number={89}
    provider="gitlab"
    headBranch="feature/x"
  />
);

let unmount: (() => Promise<void>) | undefined;

beforeEach(() => {
  h.threads = [];
  h.navigate.mockReset();
  useRightPanelStore.setState({ byThreadKey: {} });
});
afterEach(async () => {
  vi.useRealTimers();
  await unmount?.();
  unmount = undefined;
});

describe("OpenBesideThreadButton", () => {
  it("opens the request in the chosen thread's panel and goes to that thread", async () => {
    h.threads = [
      thread("other", null, "2026-10-14T00:00:00.000Z"),
      thread("t1", "feature/x", "2026-10-10T00:00:00.000Z"),
    ];
    ({ unmount } = await mount(element));
    await click(triggerLabel);
    expect(document.body.textContent).toContain("On this branch");
    await click("Thread t1, on this branch");
    expect(
      selectThreadRightPanelState(useRightPanelStore.getState().byThreadKey, {
        environmentId: "env",
        threadId: "t1",
      } as never).activeSurfaceId,
    ).toBe("pullRequest:89");
    expect(h.navigate).toHaveBeenCalledWith({
      to: "/$environmentId/$threadId",
      params: { environmentId: "env", threadId: "t1" },
    });
  });

  it("labels only the branch's threads as on this branch", async () => {
    h.threads = [
      thread("other", null, "2026-10-14T00:00:00.000Z"),
      thread("t1", "feature/x", "2026-10-10T00:00:00.000Z"),
    ];
    ({ unmount } = await mount(element));
    await click(triggerLabel);
    const items = [...document.querySelectorAll("[role=menuitem]")];
    expect(items.map((item) => item.getAttribute("aria-label"))).toEqual([
      "Thread t1, on this branch",
      "Thread other",
    ]);
  });

  it("stays unavailable, and shows why on hover, when the project has no thread", async () => {
    ({ unmount } = await mount(element));
    const trigger = [...document.querySelectorAll("button")].find(
      (button) => button.textContent === triggerLabel,
    )!;
    const reason = () => document.querySelector("[data-slot=popover-popup]");
    expect(trigger.getAttribute("aria-disabled")).toBe("true");
    expect(reason()?.hasAttribute("data-closed")).toBe(true);
    vi.useFakeTimers();
    await act(async () => {
      trigger.dispatchEvent(new MouseEvent("mouseenter", { bubbles: true }));
      trigger.dispatchEvent(new MouseEvent("mousemove", { bubbles: true }));
      vi.advanceTimersByTime(1000);
    });
    expect(reason()?.hasAttribute("data-closed")).toBe(false);
    expect(reason()?.textContent).toBe(noThreadReason);
    await click(triggerLabel);
    expect(document.querySelector("[role=menuitem]")).toBeNull();
    expect(h.navigate).not.toHaveBeenCalled();
  });
});
