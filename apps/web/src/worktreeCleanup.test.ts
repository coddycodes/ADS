import { describe, expect, it } from "vite-plus/test";

import { formatWorktreePathForDisplay, getBulkThreadDeletionConfirmation } from "./worktreeCleanup";

describe("getBulkThreadDeletionConfirmation", () => {
  it("states that worktree-backed rows are detach-only", () => {
    expect(getBulkThreadDeletionConfirmation(3, 2)).toBe(
      [
        "Delete 3 threads?",
        "This permanently clears conversation history for these threads.",
        "2 worktree-backed threads will be removed from Coddy Studio only. Git worktrees and files are left untouched.",
      ].join("\n"),
    );
  });

  it("omits worktree copy when the selection has no worktree-backed rows", () => {
    expect(getBulkThreadDeletionConfirmation(1, 0)).toBe(
      ["Delete 1 thread?", "This permanently clears conversation history for this thread."].join(
        "\n",
      ),
    );
  });
});

describe("formatWorktreePathForDisplay", () => {
  it("preserves empty and root-only paths", () => {
    expect(formatWorktreePathForDisplay("   ")).toBe("   ");
    expect(formatWorktreePathForDisplay("/")).toBe("/");
  });

  it("shows only the last path segment for unix-like paths", () => {
    const result = formatWorktreePathForDisplay(
      "/Users/julius/.coddy/studio/worktrees/coddy-studio-mvp/coddy-studio-4e609bb8",
    );
    expect(result).toBe("coddy-studio-4e609bb8");
  });

  it("normalizes windows separators before selecting the final segment", () => {
    const result = formatWorktreePathForDisplay(
      "C:\\Users\\julius\\.coddy/studio\\worktrees\\coddy-studio-mvp\\coddy-studio-4e609bb8",
    );
    expect(result).toBe("coddy-studio-4e609bb8");
  });

  it("uses the final segment even when outside ~/.coddy/studio/worktrees", () => {
    const result = formatWorktreePathForDisplay("/tmp/custom-worktrees/my-worktree");
    expect(result).toBe("my-worktree");
  });

  it("ignores trailing slashes", () => {
    const result = formatWorktreePathForDisplay("/tmp/custom-worktrees/my-worktree/");
    expect(result).toBe("my-worktree");
  });
});
