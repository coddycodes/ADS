// @vitest-environment happy-dom
import { EnvironmentId } from "@coddy/contracts";
import { act, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const state = vi.hoisted(() => ({
  data: null as unknown,
  pending: false,
  error: null as string | null,
  queries: [] as unknown[],
}));
vi.mock("../ui/dialog", () => {
  const Box = ({ children }: { children?: ReactNode }) => <div>{children}</div>;
  return {
    Dialog: Box,
    DialogPopup: Box,
    DialogHeader: Box,
    DialogTitle: Box,
    DialogDescription: Box,
    DialogPanel: Box,
    DialogFooter: Box,
  };
});
vi.mock("~/state/remoteUpdates", () => ({
  remoteUpdateEnvironment: { activeWork: (target: unknown) => target },
}));
vi.mock("~/state/query", () => ({
  useEnvironmentQuery: (query: unknown) => {
    state.queries.push(query);
    return { data: state.data, isPending: state.pending, error: state.error };
  },
}));
import { UpdateServerDialog } from "./UpdateServerDialog";

const request = {
  environmentId: EnvironmentId.make("host"),
  name: "Build server",
  targetVersion: "0.7.3",
  progress: true,
};
const roots: ReturnType<typeof createRoot>[] = [];
beforeEach(() => {
  Object.assign(state, { data: null, pending: false, error: null, queries: [] });
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});
afterEach(async () => {
  for (const root of roots.splice(0)) await act(async () => root.unmount());
  document.body.replaceChildren();
});
async function mount(override = {}, onConfirm = vi.fn(), onClose = vi.fn()) {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  roots.push(root);
  await act(async () =>
    root.render(
      <UpdateServerDialog
        request={{ ...request, ...override }}
        onConfirm={onConfirm}
        onClose={onClose}
      />,
    ),
  );
  return { container, onConfirm, onClose };
}
describe("UpdateServerDialog", () => {
  it("counts running work and updates only after the named confirmation", async () => {
    state.data = { runningTurns: 2, liveTerminals: 3, queuedMessages: 1 };
    const view = await mount();
    expect(view.container.textContent).toContain("2 running agents and 3 terminals will stop.");
    const readingSurface = view.container.querySelector('[data-text-surface="background"]');
    expect(readingSurface?.classList.contains("bg-background")).toBe(true);
    expect(readingSurface?.textContent).toContain("2 running agents and 3 terminals will stop.");
    expect(view.onConfirm).not.toHaveBeenCalled();
    const button = [...view.container.querySelectorAll("button")].find(
      (candidate) => candidate.textContent === "Update Build server",
    );
    expect(button).toBeDefined();
    await act(async () => button!.click());
    expect(view.onConfirm).toHaveBeenCalledExactlyOnceWith(request);
    expect(view.onClose).toHaveBeenCalledOnce();
  });
  it("keeps confirmation usable when counts are pending or unavailable", async () => {
    state.pending = true;
    const view = await mount();
    expect(view.container.textContent).toContain("Counting running work…");
    expect([...view.container.querySelectorAll("button")].every((button) => !button.disabled)).toBe(
      true,
    );
  });
  it("does not read counts from a legacy server and discards irrelevant cached counts", async () => {
    state.data = { runningTurns: 9, liveTerminals: 9, queuedMessages: 0 };
    const view = await mount({ progress: false });
    expect(state.queries).toEqual([null]);
    expect(view.container.textContent).toContain("Running agents and terminals on it will stop.");
    expect(view.container.textContent).not.toContain("9 running agents");
  });
  it("cancels without an update", async () => {
    const view = await mount();
    await act(async () =>
      [...view.container.querySelectorAll("button")]
        .find((button) => button.textContent === "Cancel")!
        .click(),
    );
    expect(view.onClose).toHaveBeenCalledOnce();
    expect(view.onConfirm).not.toHaveBeenCalled();
  });
});
