import type { ScopedProjectRef } from "@coddy/contracts";
import { act, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createRoot } from "react-dom/client";
import { vi } from "vite-plus/test";
import { PullRequestsActionsContext, type PullRequestsActions } from "../usePullRequestsAction";
export const projectRef = { environmentId: "env", projectId: "project" } as ScopedProjectRef;
export const allowed = { allowed: true, reason: null };
export const mockRun = () =>
  vi.fn<PullRequestsActions["run"]>().mockResolvedValue({ kind: "done" });
export async function mount(ui: ReactNode, run = mockRun(), requestKind = "pull request") {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const render = async (node: ReactNode) => {
    await act(async () =>
      root.render(
        <PullRequestsActionsContext value={{ run, pending: false, error: null, requestKind }}>
          {node}
        </PullRequestsActionsContext>,
      ),
    );
  };
  await render(ui);
  return {
    container,
    run,
    render,
    unmount: async () => {
      await act(async () => root.unmount());
      container.remove();
    },
  };
}
export function button(text: string, container: ParentNode = document) {
  const found = [...container.querySelectorAll<HTMLButtonElement>("button, [role=menuitem]")].find(
    (b) => b.textContent?.trim() === text || b.getAttribute("aria-label") === text,
  );
  if (!found) throw new Error(`Missing button: ${text}`);
  return found;
}
export async function click(text: string, container: ParentNode = document) {
  await act(async () => button(text, container).click());
}
export async function input(element: HTMLTextAreaElement | HTMLInputElement, value: string) {
  await act(async () => {
    const proto =
      element instanceof HTMLTextAreaElement
        ? HTMLTextAreaElement.prototype
        : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, "value")!.set!.call(element, value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

export function renderReviewMarkup(ui: ReactNode) {
  return renderToStaticMarkup(
    <PullRequestsActionsContext
      value={{ run: mockRun(), pending: false, error: null, requestKind: "pull request" }}
    >
      {ui}
    </PullRequestsActionsContext>,
  );
}
