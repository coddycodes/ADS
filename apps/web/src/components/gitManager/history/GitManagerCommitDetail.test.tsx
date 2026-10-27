// @vitest-environment happy-dom

import type { EnvironmentId, GitManagerCommitEntry, GitManagerDiff } from "@coddy/contracts";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  diff: null as GitManagerDiff | null,
  parser: vi.fn(() => ({ kind: "raw" as const, text: "parsed", reason: "test" })),
}));

vi.mock("@legendapp/list/react", () => ({
  LegendList: (props: {
    data: ReadonlyArray<string>;
    keyExtractor: (path: string) => string;
    renderItem: (input: { item: string; index: number }) => React.ReactNode;
  }) => (
    <div>
      {props.data.map((item, index) => (
        <div key={props.keyExtractor(item)}>{props.renderItem({ item, index })}</div>
      ))}
    </div>
  ),
}));

vi.mock("../../../state/gitManager", () => ({
  gitManagerEnvironment: { getDiff: vi.fn(() => ({ id: "diff-atom" })) },
}));

vi.mock("../../../state/query", () => ({
  useEnvironmentQuery: () => ({
    data: h.diff,
    emission: { _tag: "Success" },
    error: null,
    isPending: false,
    refresh: () => undefined,
  }),
}));

vi.mock("../../../hooks/useTheme", () => ({
  useTheme: () => ({ resolvedTheme: "dark" }),
}));

vi.mock("../../../lib/diffRendering", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../lib/diffRendering")>();
  return { ...actual, getRenderablePatch: h.parser };
});

vi.mock("../../DiffWorkerPoolProvider", () => ({
  DiffWorkerPoolProvider: ({ children }: { children?: React.ReactNode }) => children,
}));

vi.mock("@pierre/diffs/react", () => ({
  FileDiff: () => <div data-testid="file-diff" />,
}));

import { GitManagerCommitDetail } from "./GitManagerCommitDetail";

let container: HTMLDivElement;
let root: Root | null;

const commit: GitManagerCommitEntry = {
  sha: "a".repeat(40),
  shortSha: "aaaaaaa",
  parents: [],
  decorations: ["HEAD -> main"],
  subject: "Large change",
  body: "Details",
  authorName: "Local Author",
  authorEmail: "author@example.test",
  authoredAtMs: 1,
  committerName: "Local Committer",
  committerEmail: "committer@example.test",
  committedAtMs: 2,
  changedFiles: ["src/large.ts"],
};

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.diff = {
    _tag: "patch",
    generation: 1,
    source: { _tag: "commit", sha: commit.sha, path: "src/large.ts" },
    byteLength: 4_375_000,
    longestLineLength: 10,
    patch: "diff --git a/src/large.ts b/src/large.ts",
  };
  h.parser.mockClear();
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

describe("GitManagerCommitDetail", () => {
  it("does not parse a large-text diff until the user explicitly asks", async () => {
    await act(async () =>
      root?.render(
        <GitManagerCommitDetail
          environmentId={"env-a" as EnvironmentId}
          cwd="/repo"
          commit={commit}
          selectedFilePath="src/large.ts"
          onSelectFile={() => undefined}
        />,
      ),
    );

    expect(h.parser).not.toHaveBeenCalled();
    const showButton = Array.from(container.querySelectorAll("button")).find(
      (button) => button.textContent === "Show diff anyway",
    );
    expect(showButton).toBeDefined();

    await act(async () => showButton?.click());

    expect(h.parser).toHaveBeenCalledTimes(1);
    expect(h.parser).toHaveBeenCalledWith(
      "diff --git a/src/large.ts b/src/large.ts",
      "git-manager-history:dark",
    );
  });

  it("renders the selected image payload without issuing an external request", async () => {
    const fetchSpy = vi.fn();
    vi.stubGlobal("fetch", fetchSpy);
    h.diff = {
      _tag: "image",
      generation: 2,
      source: { _tag: "commit", sha: commit.sha, path: "src/large.ts" },
      byteLength: 128,
      longestLineLength: 0,
      before: { contentBase64: "YmVmb3Jl", mimeType: "image/png" },
      after: { contentBase64: "YWZ0ZXI=", mimeType: "image/png" },
    };

    await act(async () =>
      root?.render(
        <GitManagerCommitDetail
          environmentId={"env-a" as EnvironmentId}
          cwd="/repo"
          commit={commit}
          selectedFilePath="src/large.ts"
          onSelectFile={() => undefined}
        />,
      ),
    );

    expect(container.querySelector('[aria-label="Image diff"]')).not.toBeNull();
    expect(container.querySelectorAll("img")).toHaveLength(2);
    expect(fetchSpy).not.toHaveBeenCalled();
  });
});
