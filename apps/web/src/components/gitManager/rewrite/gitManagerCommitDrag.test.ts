// @vitest-environment happy-dom

import type { GitManagerCommitEntry } from "@coddy/contracts";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vite-plus/test";

vi.mock("@legendapp/list/react", async () => {
  const React = await import("react");
  return {
    LegendList: (props: {
      data: ReadonlyArray<GitManagerCommitEntry>;
      keyExtractor: (commit: GitManagerCommitEntry) => string;
      renderItem: (input: { item: GitManagerCommitEntry; index: number }) => React.ReactNode;
    }) =>
      React.createElement(
        "div",
        null,
        props.data.map((item, index) =>
          React.createElement(
            React.Fragment,
            { key: props.keyExtractor(item) },
            props.renderItem({ item, index }),
          ),
        ),
      ),
  };
});

import { GitManagerCommitList } from "../history/GitManagerCommitList";
import { advanceCommitKeyboardReorder, resolveCommitDropTarget } from "./gitManagerCommitDrag";

const drag = { type: "commit" as const, shas: ["b", "c"] };

describe("resolveCommitDropTarget", () => {
  it("maps each supported target to its history operation", () => {
    expect(resolveCommitDropTarget(drag, { type: "branch", branch: "release" })).toEqual({
      _tag: "cherry-pick",
      shas: ["b", "c"],
      branch: "release",
      createBranch: false,
    });
    expect(resolveCommitDropTarget(drag, { type: "new-branch" })).toEqual({
      _tag: "cherry-pick",
      shas: ["b", "c"],
      branch: null,
      createBranch: true,
    });
    expect(resolveCommitDropTarget(drag, { type: "commit", sha: "a" })).toEqual({
      _tag: "squash",
      shas: ["b", "c"],
      targetSha: "a",
    });
    expect(resolveCommitDropTarget(drag, { type: "insertion", beforeSha: "d" })).toEqual({
      _tag: "reorder",
      shas: ["b", "c"],
      insertBeforeSha: "d",
    });
    expect(resolveCommitDropTarget(drag, { type: "other" })).toBeNull();
    expect(resolveCommitDropTarget(drag, { type: "commit", sha: "b" })).toBeNull();
  });

  it("refuses a server-blocked target with the verbatim reason", () => {
    const blocked = {
      operation: "reorder",
      code: "operation-in-flight",
      message: "Server says another history operation is running.",
    } as const;

    expect(
      resolveCommitDropTarget(drag, {
        type: "insertion",
        beforeSha: "d",
        blocked,
      }),
    ).toEqual({ _tag: "blocked", reason: blocked });
  });
});

describe("advanceCommitKeyboardReorder", () => {
  it("moves with arrow keys and drops with Enter", () => {
    const started = { activeSha: "b", overSha: "b", dropped: false };
    const moved = advanceCommitKeyboardReorder(started, "ArrowDown", ["a", "b", "c"]);
    const dropped = advanceCommitKeyboardReorder(moved, "Enter", ["a", "b", "c"]);

    expect(moved).toEqual({ activeSha: "b", overSha: "c", dropped: false });
    expect(dropped).toEqual({ activeSha: "b", overSha: "c", dropped: true });
  });
});

describe("GitManagerCommitList drag affordance", () => {
  it("keeps the memoized row inside a dnd-kit commit source", () => {
    const commit: GitManagerCommitEntry = {
      sha: "commit-a",
      shortSha: "commit-",
      parents: ["parent"],
      decorations: [],
      subject: "Subject",
      body: "",
      authorName: "Author",
      authorEmail: "author@example.com",
      authoredAtMs: 1,
      committerName: "Committer",
      committerEmail: "committer@example.com",
      committedAtMs: 1,
      changedFiles: [],
    };

    const markup = renderToStaticMarkup(
      createElement(GitManagerCommitList, {
        commits: [commit],
        selectedSha: commit.sha,
        multiCommitSelection: [commit.sha],
        isLoadingMore: false,
        onCommitDrop: vi.fn(),
        onReachEnd: () => undefined,
        onSelect: () => undefined,
      }),
    );

    expect(markup).toContain('data-commit-drag-source="commit-a"');
    expect(markup).toContain('aria-roledescription="sortable"');
  });
});
