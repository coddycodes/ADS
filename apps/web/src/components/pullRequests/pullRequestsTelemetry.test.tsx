// @vitest-environment happy-dom
import { AVAILABLE_CONNECTION_STATE } from "@bibcode/client-runtime/connection";
import type { ScopedProjectRef, ServerConfig } from "@bibcode/contracts";
import { makeTestExecutionEnvironmentCapabilities } from "@bibcode/shared/testSupport";
import { act, StrictMode, Suspense, useReducer, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { usePullRequestsStore, type PullRequestsDetailTab } from "../../pullRequestsStore";
import { comment, context, detail, event, review, thread } from "./detail/testFixtures";
import { row } from "./testFixtures";

const h = vi.hoisted(() => ({
  data: {} as Record<string, unknown>,
  dispatch: vi.fn(),
  runAction: vi.fn(),
  checkout: vi.fn(),
  navigate: vi.fn(),
  atom: vi.fn((kind: string, args: unknown) => ({ kind, args })),
  listSuspension: null as Promise<void> | null,
}));

// Keep the real panels, renderers, draft store, query revalidation and action owner.
// Only the environment/transport boundary and viewport layout are substituted.
vi.mock("../../state/pullRequests", () => ({
  pullRequestsEnvironment: {
    ...Object.fromEntries(
      [
        "getContext",
        "list",
        "get",
        "getTimeline",
        "getCommits",
        "getChecks",
        "getFiles",
        "getVocabulary",
        // A display-copy read from the BiBCode server, never the host.
        "readSnapshot",
      ]
        .map((kind) => [kind, (args: unknown) => h.atom(kind, args)])
        .concat([
          ["runAction", "runAction"],
          ["checkout", "checkout"],
        ]),
    ),
    // Marking the next read to bypass server caches is not a request by itself.
    requestContextRescan: () => undefined,
    requestListTotalsRefresh: () => undefined,
  },
}));
vi.mock("../../state/worktrees", () => ({
  worktreeEnvironment: { catalog: (args: unknown) => h.atom("catalog", args) },
}));
vi.mock("../../state/entities", () => ({
  useProject: () => ({ id: "project", workspaceRoot: "/opaque/repo" }),
  useServerConfigs: () => new Map([["env", h.data.config]]),
}));
vi.mock("../../state/environments", () => ({
  useEnvironmentConnectionState: () => ({ data: h.data.connection }),
}));
vi.mock("../../hooks/useSettings", () => ({
  usePrimarySettings: (select: (settings: { pullRequestsEnabled: boolean }) => unknown) =>
    select({ pullRequestsEnabled: true }),
  useClientSettings: (select: (settings: { diffIgnoreWhitespace: boolean }) => unknown) =>
    select({ diffIgnoreWhitespace: false }),
}));
vi.mock("../../hooks/useTheme", () => ({ useTheme: () => ({ resolvedTheme: "light" }) }));
vi.mock("../../state/query", () => ({
  useEnvironmentQuery: (atom: { kind: string; args: unknown } | null) => {
    const [, publish] = useReducer((n: number) => n + 1, 0);
    const refresh = () => {
      if (!atom) return;
      h.dispatch(atom.kind, atom.args);
      // A fresh receipt lets the real usePullRequestsQuery release cached data.
      if (h.data[atom.kind]) h.data[atom.kind] = { ...(h.data[atom.kind] as object) };
      publish();
    };
    return {
      data: atom ? (h.data[atom.kind] ?? null) : null,
      emission: { _tag: "Initial" },
      error: null,
      isPending: false,
      refresh,
      revalidate: refresh,
      requiresRetry: false,
    };
  },
}));
vi.mock("@effect/atom-react", async (original) => ({
  ...(await original<object>()),
  useAtomRefresh: (atom: { kind: string; args: unknown }) => () => h.dispatch(atom.kind, atom.args),
}));
vi.mock("../../state/use-atom-command", () => ({
  useAtomCommand: (command: string) => (command === "runAction" ? h.runAction : h.checkout),
}));
vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => h.navigate,
  useLocation: () => ({ hash: "" }),
  Link: ({ children, params }: { children: ReactNode; params?: { number?: string } }) => (
    <a href={`/pull-requests/${params?.number ?? ""}`}>{children}</a>
  ),
}));
vi.mock("@legendapp/list/react", () => ({
  LegendList: ({
    data,
    keyExtractor,
    renderItem,
    ListHeaderComponent,
    ListFooterComponent,
  }: {
    data: readonly unknown[];
    keyExtractor: (item: unknown) => string;
    renderItem: (input: { item: unknown; index: number }) => ReactNode;
    ListHeaderComponent?: ReactNode;
    ListFooterComponent?: ReactNode;
  }) => {
    if (h.listSuspension) throw h.listSuspension;
    return (
      <div>
        {ListHeaderComponent}
        {data.map((item, index) => (
          <div key={keyExtractor(item)}>{renderItem({ item, index })}</div>
        ))}
        {ListFooterComponent}
      </div>
    );
  },
}));

import { PullRequestsPanel } from "./PullRequestsPanel";

const projectRef = { environmentId: "env", projectId: "project" } as ScopedProjectRef;
const request = { environmentId: "env", input: { cwd: "/opaque/repo", number: 14 } };
const deniedFetch = vi.fn(() => {
  throw new Error("Unexpected Pull Requests fetch");
});
const deniedImage = vi.fn(function DeniedImage() {
  throw new Error("Unexpected Pull Requests Image");
});
const deniedWebSocket = vi.fn(function DeniedWebSocket() {
  throw new Error("Unexpected Pull Requests WebSocket");
});
const deniedXhr = vi.fn(function DeniedXhr() {
  throw new Error("Unexpected Pull Requests XMLHttpRequest");
});
const deniedBeacon = vi.fn(() => {
  throw new Error("Unexpected Pull Requests beacon");
});
const imageBody =
  '![remote diagram](https://images.example.test/tracker.png)\n\n<img src="https://images.example.test/raw.png" alt="raw image">';

// Deliberately retain provider avatar fields that the normalized contract omits.
function actor(login: string, name: string) {
  return { login, name, isBot: false, avatarUrl: `https://avatars.example.test/${login}.png` };
}
let container: HTMLDivElement;
let root: Root;

async function render(tab?: PullRequestsDetailTab) {
  await act(async () =>
    root.render(
      <PullRequestsPanel projectRef={projectRef} {...(tab ? { number: 14, tab } : {})} />,
    ),
  );
  if (tab === "files") expect(container.textContent).toContain("No files changed");
}
async function click(label: string) {
  const button = [...container.querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === label,
  );
  expect(button, `Missing enabled button: ${label}`).toBeDefined();
  expect(button!.disabled).toBe(false);
  await act(async () => button!.click());
}
function expectLocalIdentity(login: string, initials: string) {
  const names = [...container.querySelectorAll("span")].filter(
    (span) => span.childElementCount === 0 && span.textContent === login,
  );
  expect(names.length, `Missing identity: ${login}`).toBeGreaterThan(0);
  expect(
    names.some(
      (name) => name.parentElement?.querySelector('[aria-hidden="true"]')?.textContent === initials,
    ),
    `Missing initials for ${login}`,
  ).toBe(true);
}
function expectNoDirectNetwork() {
  for (const denied of [deniedFetch, deniedImage, deniedWebSocket, deniedXhr, deniedBeacon])
    expect(denied).not.toHaveBeenCalled();
  expect(container.querySelectorAll("img")).toHaveLength(0);
}

beforeAll(async () => {
  // Resolve the code-split tab before fake timers; the idle checks must observe
  // its mounted content rather than only the Suspense loading fallback.
  await import("./detail/PullRequestsFiles");
});
beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.clearAllMocks();
  h.listSuspension = null;
  vi.stubGlobal("fetch", deniedFetch);
  vi.stubGlobal("Image", deniedImage);
  vi.stubGlobal("WebSocket", deniedWebSocket);
  vi.stubGlobal("XMLHttpRequest", deniedXhr);
  vi.spyOn(navigator, "sendBeacon").mockImplementation(deniedBeacon);
  usePullRequestsStore.setState({ byProjectKey: {} });
  h.runAction.mockResolvedValue({ _tag: "Success", value: { kind: "done" } });
  h.data = {
    connection: {
      ...AVAILABLE_CONNECTION_STATE,
      desired: true,
      phase: "connected",
      network: "online",
    },
    config: {
      environment: {
        capabilities: makeTestExecutionEnvironmentCapabilities({
          pullRequestsReads: true,
          pullRequestsMutations: true,
        }),
      },
    } as ServerConfig,
    catalog: { worktrees: [] },
    getContext: context,
    list: {
      rows: [{ ...row, author: actor("ada", "Ada Lovelace") }],
      nextCursor: null,
      totalCount: 1,
      counts: { open: 1, closed: 0, merged: 0 },
    },
    get: {
      ...detail,
      author: actor("ada", "Ada Lovelace"),
      body: imageBody,
      permissions: { ...detail.permissions, comment: { allowed: true, reason: null } },
      assignees: [actor("alan", "Alan Turing")],
      reviewers: [
        { actor: actor("katherine", "Katherine Johnson"), state: "approved", canRerequest: false },
      ],
    },
    getTimeline: {
      items: [
        { ...comment, author: actor("grace", "Grace Hopper"), body: imageBody },
        { ...review, author: actor("barbara", "Barbara Liskov"), body: imageBody },
        {
          ...thread,
          comments: [
            {
              ...thread.comments[0],
              author: actor("margaret", "Margaret Hamilton"),
              body: imageBody,
            },
          ],
        },
        { ...event, actor: actor("donald", "Donald Knuth") },
      ],
      truncated: false,
    },
    getCommits: {
      commits: [
        {
          sha: "abc123",
          shortSha: "abc123",
          subject: "Local identity",
          body: "",
          author: actor("edsger", "Edsger Dijkstra"),
          authoredAt: "2026-09-20T12:00:00Z",
          url: "https://example.test/commit/abc123",
          inCheckout: false,
        },
      ],
    },
    getChecks: { groups: [], summary: "none", pipelineUrl: null },
    getFiles: {
      files: [],
      diffRefs: { baseSha: "base", startSha: "start", headSha: "head" },
      truncated: false,
    },
  };
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("Pull Requests zero-telemetry runtime", () => {
  it("reuses the prefetched page through StrictMode double rendering and a suspended first open", async () => {
    h.data.getContext = null;
    const ui = (
      <StrictMode>
        <Suspense fallback={<span>Opening list…</span>}>
          <PullRequestsPanel projectRef={projectRef} />
        </Suspense>
      </StrictMode>
    );
    await act(async () => root.render(ui));
    expect(h.atom.mock.calls.some(([kind]) => kind === "list")).toBe(true);

    let release!: () => void;
    h.listSuspension = new Promise<void>((resolve) => {
      release = resolve;
    });
    h.data.getContext = context;
    await act(async () =>
      root.render(
        <StrictMode>
          <Suspense fallback={<span>Opening list…</span>}>
            <PullRequestsPanel projectRef={projectRef} />
          </Suspense>
        </StrictMode>,
      ),
    );
    await act(async () => {
      h.listSuspension = null;
      release();
    });
    expect(container.textContent).toContain(row.title);
    expect(h.dispatch.mock.calls.filter(([kind]) => kind === "list")).toHaveLength(0);
  });

  it("renders list actors as local initials even when payloads contain avatar URLs", async () => {
    await render();
    expect(container.textContent).toContain(row.title);
    expectLocalIdentity("ada", "AL");
    expectNoDirectNetwork();
  });

  it("renders every conversation actor as initials and remote markdown images as links", async () => {
    await render("conversation");
    for (const [login, initials] of [
      ["ada", "AL"],
      ["grace", "GH"],
      ["barbara", "BL"],
      ["margaret", "MH"],
      ["donald", "DK"],
      ["alan", "AT"],
      ["katherine", "KJ"],
    ])
      expectLocalIdentity(login!, initials!);
    expect(
      container.querySelectorAll('a[href="https://images.example.test/tracker.png"]').length,
    ).toBeGreaterThanOrEqual(4);
    expect(container.textContent).toContain("image: remote diagram — open in browser");
    expectNoDirectNetwork();
  });

  it("renders commit authors with local initials", async () => {
    await render("commits");
    expectLocalIdentity("edsger", "ED");
    expectNoDirectNetwork();
  });

  it.each([undefined, "conversation", "commits", "checks", "files"] as const)(
    "does not dispatch or contact a host during an idle hour on %s",
    async (tab) => {
      vi.useFakeTimers();
      await render(tab);
      h.dispatch.mockClear();
      h.atom.mockClear();
      await act(async () => vi.advanceTimersByTimeAsync(60 * 60 * 1_000));
      await act(async () => window.dispatchEvent(new Event("focus")));
      expect(h.dispatch).not.toHaveBeenCalled();
      expect(h.atom).not.toHaveBeenCalled();
      expect(h.runAction).not.toHaveBeenCalled();
      expect(h.checkout).not.toHaveBeenCalled();
      expectNoDirectNetwork();
    },
  );

  it.each([
    [undefined, ["list"]],
    ["conversation", ["get", "getTimeline"]],
    ["commits", ["get", "getCommits"]],
    ["checks", ["get", "getChecks"]],
    ["files", ["get", "getFiles", "getTimeline"]],
  ] as const)("refreshes each active query exactly once on %s", async (tab, queries) => {
    await render(tab);
    h.dispatch.mockClear();
    await click("Refresh");
    expect(h.dispatch.mock.calls.map(([kind]) => kind)).toEqual(queries);
    if (tab) for (const kind of queries) expect(h.dispatch).toHaveBeenCalledWith(kind, request);
    expect(h.runAction).not.toHaveBeenCalled();
    expectNoDirectNetwork();
  });

  it("dispatches one action for an explicit comment and refreshes affected queries once", async () => {
    usePullRequestsStore.getState().setCommentDraft(projectRef, 14, "Keep this explicit");
    await render("conversation");
    expect(h.runAction).not.toHaveBeenCalled();
    h.dispatch.mockClear();
    await click("Comment");
    expect(h.runAction).toHaveBeenCalledExactlyOnceWith({
      environmentId: "env",
      input: { cwd: "/opaque/repo", number: 14, action: "comment", body: "Keep this explicit" },
    });
    expect(h.dispatch.mock.calls.map(([kind]) => kind)).toEqual(["get", "getTimeline"]);
    expect(usePullRequestsStore.getState().selectDraft(projectRef, 14).comment).toBe("");
    expectNoDirectNetwork();
  });
});
