// @vitest-environment happy-dom

import {
  AVAILABLE_CONNECTION_STATE,
  type SupervisorConnectionState,
} from "@coddy/client-runtime/connection";
import type { GitManagerCommitEntry, ServerConfig } from "@coddy/contracts";
import { makeTestExecutionEnvironmentCapabilities } from "@coddy/shared/testSupport";
import { setupServer } from "msw/node";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vite-plus/test";

import { useGitManagerStore } from "../../gitManagerStore";

const h = vi.hoisted(() => ({
  connectionState: null as SupervisorConnectionState | null,
  serverConfig: null as ServerConfig | null,
  project: {
    id: "project-1",
    environmentId: "environment-1",
    title: "Repository",
    workspaceRoot: "/opaque/main",
  } as Record<string, unknown> | null,
  catalog: {
    worktrees: [{ path: "/opaque/main", branch: "main", isPrimary: true }],
  } as Record<string, unknown> | null,
  catalogAtom: vi.fn(() => ({ kind: "catalog" })),
  signalAtom: vi.fn(() => ({ kind: "signal" })),
  refsAtom: vi.fn(() => ({ kind: "refs" })),
  stashesAtom: vi.fn(() => ({ kind: "stashes" })),
  diffAtom: vi.fn(() => ({ kind: "diff" })),
  listPullRequests: vi.fn(() => ({ kind: "provider" })),
  refresh: vi.fn(),
}));

vi.mock("@legendapp/list/react", () => ({
  LegendList: (props: {
    data: ReadonlyArray<GitManagerCommitEntry>;
    keyExtractor: (commit: GitManagerCommitEntry) => string;
    renderItem: (input: { item: GitManagerCommitEntry; index: number }) => React.ReactNode;
  }) => (
    <div>
      {props.data.map((item, index) => (
        <div key={props.keyExtractor(item)}>{props.renderItem({ item, index })}</div>
      ))}
    </div>
  ),
}));

vi.mock("../../hooks/useTheme", () => ({
  useTheme: () => ({ resolvedTheme: "light" }),
}));

vi.mock("../../state/entities", () => ({
  useProject: () => h.project,
  useServerConfigs: () =>
    new Map(h.serverConfig === null ? [] : [["environment-1", h.serverConfig]]),
}));

vi.mock("../../state/environments", () => ({
  useEnvironmentConnectionState: () => ({ data: h.connectionState }),
  useEnvironment: () => ({ label: "Local" }),
}));

vi.mock("../../state/query", () => ({
  useEnvironmentQuery: (atom: { kind?: string } | null) => ({
    data: atom?.kind === "catalog" ? h.catalog : null,
    error: null,
    isPending: false,
    refresh: h.refresh,
    revalidate: h.refresh,
    requiresRetry: false,
  }),
}));

vi.mock("../../state/worktrees", () => ({
  worktreeEnvironment: { catalog: h.catalogAtom },
}));

vi.mock("../../state/gitManager", () => ({
  gitManagerEnvironment: {
    signalWithDegradedFocusRefresh: h.signalAtom,
    getRefs: h.refsAtom,
    getStashes: h.stashesAtom,
    getDiff: h.diffAtom,
    listPullRequests: h.listPullRequests,
    commit: { label: "test:commit" },
    undoCommit: { label: "test:undo-commit" },
    discard: { label: "test:discard" },
  },
}));

vi.mock("../../state/sourceControlActions", () => ({
  useGitStackedAction: () => ({ run: vi.fn(), isPending: false, error: null }),
}));

vi.mock("../ui/tabs", () => ({
  Tabs: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  TabsList: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  TabsPanel: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  TabsTab: ({ children }: { children: React.ReactNode }) => <button>{children}</button>,
}));

vi.mock("./GitManagerToolbar", () => ({
  GitManagerToolbar: () => <div data-testid="git-manager-toolbar" />,
}));

vi.mock("./changes/GitManagerChangesView", () => ({
  GitManagerChangesView: () => <div data-testid="git-manager-changes" />,
}));

vi.mock("./history/GitManagerHistoryView", () => ({
  GitManagerHistoryView: () => <div data-testid="git-manager-history" />,
}));

vi.mock("./rewrite/gitManagerCommitDrag", () => ({
  GitManagerCommitDndContext: ({ children }: { children: React.ReactNode }) => <>{children}</>,
  GitManagerCommitInsertionTarget: () => null,
  useGitManagerCommitDragSource: () => ({
    attributes: {},
    listeners: {},
    setNodeRef: () => undefined,
    isDragging: false,
    transform: undefined,
  }),
}));

import { GitManagerPanel } from "./GitManagerPanel";
import { GitManagerCommitDetail } from "./history/GitManagerCommitDetail";
import { GitManagerCommitList } from "./history/GitManagerCommitList";
import { GitManagerPullRequestPanel } from "./provider/GitManagerPullRequestPanel";

const server = setupServer();
const deniedFetch = vi.fn(() => {
  throw new Error("Unexpected fetch from Git Manager");
});
const deniedImage = vi.fn(function DeniedImage() {
  throw new Error("Unexpected Image construction from Git Manager");
});
const deniedWebSocket = vi.fn(function DeniedWebSocket() {
  throw new Error("Unexpected WebSocket construction from Git Manager");
});
const deniedXmlHttpRequest = vi.fn(function DeniedXmlHttpRequest() {
  throw new Error("Unexpected XMLHttpRequest construction from Git Manager");
});
const projectRef = {
  environmentId: "environment-1",
  projectId: "project-1",
} as never;

let container: HTMLDivElement;
let root: Root;

function config(): ServerConfig {
  return {
    environment: {
      capabilities: makeTestExecutionEnvironmentCapabilities({
        gitManagerReads: true,
        gitManagerBranchSyncOperations: true,
        gitManagerStashMergeOperations: true,
        gitManagerRewriteOperations: true,
        gitManagerTagOperations: true,
        gitManagerLiveSignal: true,
        gitManagerPullRequests: true,
      }),
    },
  } as ServerConfig;
}

async function render(node: React.ReactNode): Promise<void> {
  await act(async () => root.render(node));
}

function button(text: string): HTMLButtonElement {
  const result = [...container.querySelectorAll<HTMLButtonElement>("button")].find((candidate) =>
    candidate.textContent?.includes(text),
  );
  if (!(result instanceof HTMLButtonElement)) throw new Error(`Missing button: ${text}`);
  return result;
}

function expectNoDirectNetwork(): void {
  expect(deniedFetch).not.toHaveBeenCalled();
  expect(deniedImage).not.toHaveBeenCalled();
  expect(deniedWebSocket).not.toHaveBeenCalled();
  expect(deniedXmlHttpRequest).not.toHaveBeenCalled();
}

function localCommit(): GitManagerCommitEntry {
  return {
    sha: "0123456789abcdef0123456789abcdef01234567",
    shortSha: "0123456",
    parents: [],
    decorations: [],
    subject: "Keep author identity local",
    body: "",
    authorName: "",
    authorEmail: "grace.hopper@example.test",
    authoredAtMs: 1_700_000_000_000,
    committerName: "Alan Turing",
    committerEmail: "alan.turing@example.test",
    committedAtMs: 1_700_000_000_000,
    changedFiles: [],
  };
}

beforeAll(() => {
  server.listen({ onUnhandledRequest: "error" });
});

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.connectionState = {
    ...AVAILABLE_CONNECTION_STATE,
    desired: true,
    phase: "connected",
    network: "online",
  };
  h.serverConfig = config();
  vi.clearAllMocks();
  vi.stubGlobal("fetch", deniedFetch);
  vi.stubGlobal("Image", deniedImage);
  vi.stubGlobal("WebSocket", deniedWebSocket);
  vi.stubGlobal("XMLHttpRequest", deniedXmlHttpRequest);
  useGitManagerStore.setState({ byProjectKey: {} });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  server.resetHandlers();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

afterAll(() => {
  server.close();
});

describe("Git Manager zero-telemetry runtime", () => {
  it("renders the panel without issuing a request or constructing an Image", async () => {
    await render(<GitManagerPanel projectRef={projectRef} />);

    expect(container.textContent).toContain("Changes");
    expectNoDirectNetwork();
  });

  it("does not request provider or third-party data after an idle hour", async () => {
    vi.useFakeTimers();
    useGitManagerStore.getState().setProviderPaneOpen(projectRef, true);
    await render(<GitManagerPanel projectRef={projectRef} />);

    await act(async () => vi.advanceTimersByTimeAsync(60 * 60 * 1_000));

    expect(h.listPullRequests).not.toHaveBeenCalled();
    expect(container.textContent).toContain("load only when you choose Refresh");
    expectNoDirectNetwork();
  });

  it("dispatches one provider command on Refresh without direct network access", async () => {
    await render(
      <GitManagerPullRequestPanel
        scope={{ environmentId: "environment-1" as never, cwd: "/opaque/main" }}
        statusLoaded={false}
        onRefresh={vi.fn()}
      />,
    );

    await act(async () => button("Refresh").click());

    expect(h.listPullRequests).toHaveBeenCalledOnce();
    expect(h.listPullRequests).toHaveBeenCalledWith({
      environmentId: "environment-1",
      input: { cwd: "/opaque/main" },
    });
    expectNoDirectNetwork();
  });

  it("renders every author identity from local commit data without a remote image", async () => {
    const commit = localCommit();
    await render(
      <>
        <GitManagerCommitList
          commits={[commit]}
          isLoadingMore={false}
          selectedSha={commit.sha}
          onReachEnd={() => undefined}
          onSelect={() => undefined}
        />
        <GitManagerCommitDetail
          commit={commit}
          cwd="/opaque/main"
          environmentId={"environment-1" as never}
          selectedFilePath={null}
          onSelectFile={() => undefined}
        />
      </>,
    );

    expect(container.textContent).toContain("GH");
    expect(container.textContent).toContain("AT");
    expect(container.textContent).toContain("grace.hopper@example.test");
    expect(container.querySelectorAll('img[src^="http://"], img[src^="https://"]')).toHaveLength(0);
    expectNoDirectNetwork();
  });
});
