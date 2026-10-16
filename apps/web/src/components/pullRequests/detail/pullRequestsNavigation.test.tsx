// @vitest-environment happy-dom
import { act, useEffect } from "react";
import { createRoot } from "react-dom/client";
import { describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({ navigate: vi.fn(), hash: "" }));
vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => h.navigate,
  useLocation: () => ({ hash: h.hash }),
}));

import {
  pathFromHash,
  type PullRequestsNavigation,
  PullRequestsRouteNavigation,
  usePullRequestsFileAnchor,
  usePullRequestsNavigation,
} from "./pullRequestsNavigation";

const projectRef = { environmentId: "env", projectId: "project" } as never;
const detailRoute = "/project/$environmentId/$projectId/pull-requests/$number";
const params = { environmentId: "env", projectId: "project", number: "14" };

async function mountRouteNavigation() {
  let navigation: PullRequestsNavigation | null = null;
  let anchor: string | null = null;
  function Probe() {
    const currentNavigation = usePullRequestsNavigation();
    const currentAnchor = usePullRequestsFileAnchor();
    useEffect(() => {
      navigation = currentNavigation;
      anchor = currentAnchor;
    }, [currentNavigation, currentAnchor]);
    return null;
  }
  const root = createRoot(document.createElement("div"));
  const render = () =>
    act(async () =>
      root.render(
        <PullRequestsRouteNavigation projectRef={projectRef} number={14}>
          <Probe />
        </PullRequestsRouteNavigation>,
      ),
    );
  await render();
  return { render, navigation: () => navigation!, anchor: () => anchor };
}

describe("PullRequestsRouteNavigation", () => {
  it("keeps the full page's URLs for every navigation", async () => {
    h.hash = "#file=src%2Fa%20b.ts";
    const view = await mountRouteNavigation();
    const navigation = view.navigation();
    expect(view.anchor()).toBe("src/a b.ts");
    navigation.selectTab("commits");
    navigation.showFile("docs/x.md");
    navigation.setFileAnchor("docs/y.md");
    navigation.openRequest(29);
    navigation.requestGone();
    navigation.back?.();
    expect(h.navigate.mock.calls.map(([call]) => call)).toEqual([
      { to: detailRoute, params, search: { tab: "commits" }, hash: "", replace: true },
      { to: detailRoute, params, search: { tab: "files" }, hash: "file=docs%2Fx.md" },
      {
        to: detailRoute,
        params,
        search: { tab: "files" },
        hash: "file=docs%2Fy.md",
        replace: true,
      },
      {
        to: detailRoute,
        params: { ...params, number: "29" },
        search: { tab: "conversation" },
        hash: "",
      },
      { to: "/project/$environmentId/$projectId/pull-requests", params: projectRef },
      { to: "/project/$environmentId/$projectId/pull-requests", params: projectRef },
    ]);
  });

  it("keeps the navigation object when only the file anchor changes", async () => {
    h.hash = "#file=a.ts";
    const view = await mountRouteNavigation();
    const before = view.navigation();
    expect(view.anchor()).toBe("a.ts");
    h.hash = "#file=b.ts";
    await view.render();
    expect(view.anchor()).toBe("b.ts");
    expect(view.navigation()).toBe(before);
  });

  it("reads no anchor from an unrelated or malformed hash", () => {
    expect(pathFromHash("")).toBeNull();
    expect(pathFromHash("#other=1")).toBeNull();
    expect(pathFromHash("#file=%E0%A4%A")).toBeNull();
  });

  it("throws when a consumer has no navigation host", async () => {
    function Orphan() {
      usePullRequestsNavigation();
      return null;
    }
    const root = createRoot(document.createElement("div"));
    const error = vi.spyOn(console, "error").mockImplementation(() => undefined);
    await expect(act(async () => root.render(<Orphan />))).rejects.toThrow(
      "Pull request views need a PullRequestsNavigationContext provider.",
    );
    error.mockRestore();
  });
});
