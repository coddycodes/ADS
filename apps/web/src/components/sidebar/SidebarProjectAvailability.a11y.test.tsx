// @vitest-environment happy-dom

import { EnvironmentId } from "@coddy/contracts";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";

import { SidebarProjectAvailability } from "./SidebarProjectAvailability";

const REASON =
  "devbox rejected a new pairing credential. Connect again; if it keeps failing, remove the environment and add it again.";

/** The accessible description from `aria-describedby`, as assistive technology reads it. */
function accessibleDescription(element: Element): string {
  return (element.getAttribute("aria-describedby") ?? "")
    .split(/\s+/u)
    .filter((id) => id.length > 0)
    .map((id) => element.ownerDocument.getElementById(id)?.textContent ?? "")
    .join(" ")
    .trim();
}

let root: Root | null = null;
let container: HTMLDivElement | null = null;

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
  root = null;
  container = null;
});

describe("SidebarProjectAvailability accessibility", () => {
  it("announces the connection's reason when the notice is focused", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    await act(async () => {
      root!.render(
        <SidebarProjectAvailability
          view={{
            kind: "configuration-error",
            environmentId: EnvironmentId.make("devbox-env"),
            error: null,
            hasCachedProjects: true,
          }}
          environment={{
            label: "devbox",
            connection: { phase: "error", error: REASON, traceId: null },
          }}
          showRetry={false}
          showConnectionSettings={false}
          showOpenRemoteServers
          onRetry={vi.fn()}
          onOpenSettings={vi.fn()}
          onViewDiagnostics={vi.fn()}
          onAdoptStorage={vi.fn()}
        />,
      );
    });

    const notice = [...container.querySelectorAll("[tabindex='0']")].find(
      (element) => element.textContent === "devbox is not connected.",
    );
    expect(notice, "the notice line is focusable").toBeDefined();
    await act(async () => {
      (notice as HTMLElement).focus();
    });

    expect(document.activeElement).toBe(notice);
    expect(accessibleDescription(notice!)).toContain(REASON);
  });
});
