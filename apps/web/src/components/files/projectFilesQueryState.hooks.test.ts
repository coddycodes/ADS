import { EnvironmentId } from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import { AsyncResult } from "effect/unstable/reactivity";
import { RpcClientError } from "effect/unstable/rpc";
import * as Socket from "effect/unstable/socket/Socket";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  atomValues: new Map<string, unknown>(),
  refresh: vi.fn(),
  retries: [] as unknown[],
  /** Keys of the mocked query atoms the hooks read. */
  reads: [] as string[],
  key(atom: unknown): string {
    return (atom as { key: string }).key;
  },
}));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  return {
    ...actual,
    useCallback: <T>(callback: T): T => callback,
  };
});

vi.mock("@effect/atom-react", async () => {
  // Real atoms (the hook's own placeholder) read through a real registry.
  const { AtomRegistry } = await import("effect/unstable/reactivity");
  const registry = AtomRegistry.make();
  return {
    useAtomValue: (atom: never) => {
      const key = harness.key(atom);
      if (key === undefined) return registry.get(atom);
      harness.reads.push(key);
      return harness.atomValues.get(key);
    },
    useAtomRefresh: () => harness.refresh,
  };
});

vi.mock("~/state/projects", () => ({
  projectEnvironment: {
    fileTreeEntries: ({
      environmentId,
      input,
    }: {
      environmentId: string;
      input: { cwd: string };
    }) => ({
      key: `entries:${environmentId}:${input.cwd}`,
    }),
    readFile: ({
      environmentId,
      input,
    }: {
      environmentId: string;
      input: { cwd: string; relativePath: string };
    }) => ({ key: `file:${environmentId}:${input.cwd}:${input.relativePath}` }),
    optimisticFile: ({
      environmentId,
      cwd,
      relativePath,
    }: {
      environmentId: string;
      cwd: string;
      relativePath: string;
    }) => ({ key: `optimistic:${environmentId}:${cwd}:${relativePath}` }),
  },
}));

vi.mock("~/rpc/atomRegistry", () => ({
  appAtomRegistry: {
    get: vi.fn(),
    set: vi.fn(),
    refresh: vi.fn(),
  },
}));

vi.mock("@bibcode/client-runtime/state/runtime", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@bibcode/client-runtime/state/runtime")>()),
  executeAtomQuery: vi.fn(),
  retryEnvironmentQuery: (atom: unknown, refresh: () => void) => {
    harness.retries.push(atom);
    refresh();
  },
}));

import { useProjectEntriesQuery, useProjectFileQuery } from "./projectFilesQueryState";

const environmentId = EnvironmentId.make("environment-project-query-hooks");
const entriesKey = `entries:${environmentId}:/repo`;
const fileKey = `file:${environmentId}:/repo:README.md`;
const emptyFileKey = `file:${environmentId}:/repo:`;
const optimisticKey = `optimistic:${environmentId}:/repo:README.md`;
const emptyOptimisticKey = `optimistic:${environmentId}:/repo:`;

beforeEach(() => {
  harness.atomValues.clear();
  harness.refresh.mockReset();
  harness.retries.length = 0;
  harness.reads.length = 0;
});

describe("project file query hooks", () => {
  it("returns successful entry data, pending state, and a refresh callback", () => {
    const data = { entries: [], truncated: false };
    harness.atomValues.set(entriesKey, { ...AsyncResult.success(data), waiting: true });

    const result = useProjectEntriesQuery(environmentId, "/repo");

    expect(result).toMatchObject({ data, error: null, isPending: true });
    result.refresh();
    expect(harness.refresh).toHaveBeenCalledOnce();
    expect(harness.retries).toEqual([{ key: entriesKey }]);
  });

  it("reads nothing and shows the list as loading until enabled", () => {
    harness.atomValues.set(entriesKey, AsyncResult.success({ entries: [], truncated: false }));

    expect(useProjectEntriesQuery(environmentId, "/repo", false)).toMatchObject({
      data: null,
      error: null,
      isPending: true,
    });
    expect(harness.reads).toEqual([]);

    expect(useProjectEntriesQuery(environmentId, "/repo", true).data).not.toBeNull();
    expect(harness.reads).toEqual([entriesKey]);
  });

  it("names a transport cut-off with the connection-dropped copy", () => {
    harness.atomValues.set(
      entriesKey,
      AsyncResult.failure(
        Cause.fail(
          new RpcClientError.RpcClientError({
            reason: new Socket.SocketCloseError({ code: 4408, closeReason: "liveness timeout" }),
          }),
        ),
      ),
    );
    expect(useProjectEntriesQuery(environmentId, "/repo").error).toBe(
      "The connection dropped before the result arrived.",
    );
  });

  it("revalidates without clearing a transport cut-off; only refresh retries", () => {
    harness.atomValues.set(entriesKey, AsyncResult.success({ entries: [], truncated: false }));
    const result = useProjectEntriesQuery(environmentId, "/repo");
    result.revalidate();
    expect(harness.refresh).toHaveBeenCalledOnce();
    expect(harness.retries).toEqual([]);
  });

  it("maps error and opaque entry failures while leaving initial queries error-free", () => {
    harness.atomValues.set(
      entriesKey,
      AsyncResult.failure(Cause.fail(new Error("workspace unavailable"))),
    );
    expect(useProjectEntriesQuery(environmentId, "/repo").error).toBe("workspace unavailable");

    harness.atomValues.set(entriesKey, AsyncResult.failure(Cause.fail("offline")));
    expect(useProjectEntriesQuery(environmentId, "/repo").error).toBe("Workspace query failed.");

    // A blank message is no message: the view's fallback names what failed.
    harness.atomValues.set(entriesKey, AsyncResult.failure(Cause.fail(new Error("   "))));
    expect(useProjectEntriesQuery(environmentId, "/repo").error).toBe("Workspace query failed.");

    harness.atomValues.set(entriesKey, AsyncResult.initial(false));
    expect(useProjectEntriesQuery(environmentId, "/repo")).toMatchObject({
      data: null,
      error: null,
      isPending: false,
    });
  });

  it("prefers an optimistic file while preserving query status", () => {
    const serverData = {
      relativePath: "README.md",
      contents: "server",
      byteLength: 6,
      truncated: false,
    };
    const optimisticData = { ...serverData, contents: "draft", byteLength: 5 };
    harness.atomValues.set(fileKey, AsyncResult.success(serverData));
    harness.atomValues.set(optimisticKey, { data: optimisticData });

    expect(useProjectFileQuery(environmentId, "/repo", "README.md")).toMatchObject({
      data: optimisticData,
      error: null,
      isPending: false,
    });
  });

  it("falls back to server data and ignores optimistic state for a missing path", () => {
    const data = {
      relativePath: "",
      contents: "server",
      byteLength: 6,
      truncated: false,
    };
    harness.atomValues.set(emptyFileKey, AsyncResult.success(data));
    harness.atomValues.set(emptyOptimisticKey, { data: { ...data, contents: "ignored" } });

    const result = useProjectFileQuery(environmentId, "/repo", null);

    expect(result.data).toBe(data);
    result.refresh();
    expect(harness.refresh).toHaveBeenCalledOnce();

    harness.atomValues.set(fileKey, AsyncResult.success(data));
    harness.atomValues.set(optimisticKey, null);
    expect(useProjectFileQuery(environmentId, "/repo", "README.md").data).toBe(data);
  });
});
