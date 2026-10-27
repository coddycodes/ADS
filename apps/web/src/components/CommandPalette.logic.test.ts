import { describe, expect, it, vi } from "vite-plus/test";
import { EnvironmentId, ProjectId, ProviderInstanceId, ThreadId } from "@coddy/contracts";
import type { Thread } from "../types";
import {
  buildRootGroups,
  buildThreadActionItems,
  filterCommandPaletteGroups,
  filterBrowseEntries,
  type CommandPaletteGroup,
} from "./CommandPalette.logic";

const LOCAL_ENVIRONMENT_ID = EnvironmentId.make("environment-local");
const PROJECT_ID = ProjectId.make("project-1");

function makeThread(overrides: Partial<Thread> = {}): Thread {
  return {
    id: ThreadId.make("thread-1"),
    environmentId: LOCAL_ENVIRONMENT_ID,
    projectId: PROJECT_ID,
    title: "Thread",
    modelSelection: { instanceId: ProviderInstanceId.make("codex"), model: "gpt-5" },
    runtimeMode: "full-access",
    interactionMode: "default",
    session: null,
    messages: [],
    proposedPlans: [],
    createdAt: "2026-03-01T00:00:00.000Z",
    archivedAt: null,
    deletedAt: null,
    updatedAt: "2026-03-01T00:00:00.000Z",
    latestTurn: null,
    branch: null,
    worktreePath: null,
    checkpoints: [],
    activities: [],
    ...overrides,
  };
}

describe("buildThreadActionItems", () => {
  it("orders threads by most recent activity and formats timestamps from updatedAt", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-03-25T12:00:00.000Z"));

    try {
      const items = buildThreadActionItems({
        threads: [
          makeThread({
            id: ThreadId.make("thread-older"),
            title: "Older thread",
            updatedAt: "2026-03-24T12:00:00.000Z",
          }),
          makeThread({
            id: ThreadId.make("thread-newer"),
            title: "Newer thread",
            createdAt: "2026-03-20T00:00:00.000Z",
            updatedAt: "2026-03-20T00:00:00.000Z",
          }),
        ],
        projectTitleById: new Map([[PROJECT_ID, "Project"]]),
        sortOrder: "updated_at",
        icon: null,
        runThread: async (_thread) => undefined,
      });

      expect(items.map((item) => item.value)).toEqual([
        "thread:thread-older",
        "thread:thread-newer",
      ]);
      expect(items[0]?.timestamp).toBe("1d ago");
      expect(items[1]?.timestamp).toBe("5d ago");
    } finally {
      vi.useRealTimers();
    }
  });

  it("ranks thread title matches ahead of contextual project-name matches", () => {
    const threadItems = buildThreadActionItems({
      threads: [
        makeThread({
          id: ThreadId.make("thread-context-match"),
          title: "Fix navbar spacing",
          updatedAt: "2026-03-20T00:00:00.000Z",
        }),
        makeThread({
          id: ThreadId.make("thread-title-match"),
          title: "Project kickoff notes",
          createdAt: "2026-03-02T00:00:00.000Z",
          updatedAt: "2026-03-19T00:00:00.000Z",
        }),
      ],
      projectTitleById: new Map([[PROJECT_ID, "Project"]]),
      sortOrder: "updated_at",
      icon: null,
      runThread: async (_thread) => undefined,
    });

    const groups = filterCommandPaletteGroups({
      activeGroups: [],
      query: "project",
      isInSubmenu: false,
      projectSearchItems: [],
      threadSearchItems: threadItems,
    });

    expect(groups).toHaveLength(1);
    expect(groups[0]?.value).toBe("threads-search");
    expect(groups[0]?.items.map((item) => item.value)).toEqual([
      "thread:thread-title-match",
      "thread:thread-context-match",
    ]);
  });

  it("preserves thread project-name matches when there is no stronger title match", () => {
    const group: CommandPaletteGroup = {
      value: "threads-search",
      label: "Threads",
      items: [
        {
          kind: "action",
          value: "thread:project-context-only",
          searchTerms: ["Fix navbar spacing", "Project"],
          title: "Fix navbar spacing",
          description: "Project",
          icon: null,
          run: async () => undefined,
        },
      ],
    };

    const groups = filterCommandPaletteGroups({
      activeGroups: [group],
      query: "project",
      isInSubmenu: false,
      projectSearchItems: [],
      threadSearchItems: [],
    });

    expect(groups).toHaveLength(1);
    expect(groups[0]?.items.map((item) => item.value)).toEqual(["thread:project-context-only"]);
  });

  it("filters archived threads out of thread search items", () => {
    const items = buildThreadActionItems({
      threads: [
        makeThread({
          id: ThreadId.make("thread-active"),
          title: "Active thread",
          createdAt: "2026-03-02T00:00:00.000Z",
          updatedAt: "2026-03-19T00:00:00.000Z",
        }),
        makeThread({
          id: ThreadId.make("thread-archived"),
          title: "Archived thread",
          archivedAt: "2026-03-20T00:00:00.000Z",
          updatedAt: "2026-03-20T00:00:00.000Z",
        }),
      ],
      projectTitleById: new Map([[PROJECT_ID, "Project"]]),
      sortOrder: "updated_at",
      icon: null,
      runThread: async (_thread) => undefined,
    });

    expect(items.map((item) => item.value)).toEqual(["thread:thread-active"]);
  });

  it("limits contextual items and falls back through optional thread metadata", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-03-25T12:00:00.000Z"));
    const runThread = vi.fn(async () => undefined);

    try {
      const items = buildThreadActionItems({
        threads: [
          makeThread({
            id: ThreadId.make("thread-context"),
            title: "Context thread",
            branch: "feature/context",
            updatedAt: undefined as never,
            createdAt: "2026-03-25T11:00:00.000Z",
          }),
          makeThread({ id: ThreadId.make("thread-hidden"), title: "Hidden by limit" }),
        ],
        activeThreadId: ThreadId.make("thread-context"),
        projectTitleById: new Map(),
        sortOrder: "created_at",
        icon: null,
        runThread,
        limit: 1,
      });

      expect(items).toHaveLength(1);
      expect(items[0]).toMatchObject({
        searchTerms: ["Context thread", "", "feature/context"],
        description: "#feature/context · Current thread",
        timestamp: "1h ago",
      });
      await items[0]!.run();
      expect(runThread).toHaveBeenCalledWith(expect.objectContaining({ id: "thread-context" }));
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("command palette defensive branches", () => {
  const action = (value: string, searchTerms: string[]) => ({
    kind: "action" as const,
    value,
    searchTerms,
    title: value,
    icon: null,
    run: async () => undefined,
  });

  it("returns null when a highlighted browse path is no longer filtered", () => {
    expect(
      filterBrowseEntries({
        browseEntries: [
          { name: "src", fullPath: "/repo/src" },
          { name: ".git", fullPath: "/repo/.git" },
        ],
        browseFilterQuery: "s",
        highlightedItemValue: "browse:/repo/missing",
      }),
    ).toEqual({
      filteredEntries: [{ name: "src", fullPath: "/repo/src" }],
      highlightedEntry: null,
      exactEntry: null,
    });
  });

  it("ranks prefix and substring matches and filters nonempty action queries", () => {
    const searchableGroup: CommandPaletteGroup = {
      value: "actions",
      label: "Actions",
      items: [action("prefix", ["Project setup"]), action("substring", ["My project notes"])],
    };

    expect(
      filterCommandPaletteGroups({
        activeGroups: [searchableGroup],
        query: "pro",
        isInSubmenu: true,
        projectSearchItems: [],
        threadSearchItems: [],
      })[0]?.items.map((item) => item.value),
    ).toEqual(["prefix", "substring"]);
    expect(
      filterCommandPaletteGroups({
        activeGroups: [searchableGroup],
        query: "oje",
        isInSubmenu: true,
        projectSearchItems: [],
        threadSearchItems: [],
      })[0]?.items.map((item) => item.value),
    ).toEqual(["prefix", "substring"]);
    expect(
      filterCommandPaletteGroups({
        activeGroups: [searchableGroup, { value: "threads", label: "Threads", items: [] }],
        query: ">project",
        isInSubmenu: false,
        projectSearchItems: [],
        threadSearchItems: [],
      }).map((group) => group.value),
    ).toEqual(["actions"]);
  });

  it("omits an empty actions group while retaining recent threads", () => {
    const groups = buildRootGroups({
      actionItems: [],
      recentThreadItems: [action("recent", ["recent"])],
    });
    expect(groups).toHaveLength(1);
    expect(groups[0]).toMatchObject({
      value: "recent-threads",
      label: "Recent Threads",
    });
    expect(groups[0]?.items.map((item) => item.value)).toEqual(["recent"]);
  });
});
