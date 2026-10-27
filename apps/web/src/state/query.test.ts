import { describe, expect, it, vi } from "vite-plus/test";
import * as Cause from "effect/Cause";
import { RpcClientError } from "effect/rpc";
import * as Socket from "effect/socket/Socket";
import { AsyncResult, Atom } from "effect/reactivity";

import {
  formatEnvironmentQueryError,
  QUERY_CONNECTION_DROPPED_MESSAGE,
  useEnvironmentQuery,
} from "./query";

const actions = vi.hoisted(() => ({
  refresh: vi.fn(),
  retry: vi.fn((_atom: object, refresh: () => void) => refresh()),
  awaitingRetry: vi.fn((_atom: object, _emission: unknown) => true),
}));
vi.mock("@effect/atom-react", () => ({
  useAtomValue: () => AsyncResult.success("page"),
  useAtomRefresh: () => actions.refresh,
}));
vi.mock("react", () => ({ useCallback: (callback: () => void) => callback }));
vi.mock("@coddy/client-runtime/state/runtime", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@coddy/client-runtime/state/runtime")>()),
  retryEnvironmentQuery: actions.retry,
  isEnvironmentQueryAwaitingRetry: actions.awaitingRetry,
}));

it("only explicit Retry clears the cutoff latch", () => {
  const atom = Atom.make(AsyncResult.success("page"));
  const result = useEnvironmentQuery(atom);
  expect(result.requiresRetry).toBe(true);
  // The latch is read against the rendered emission, the React Compiler's memo key.
  expect(actions.awaitingRetry).toHaveBeenCalledExactlyOnceWith(atom, result.emission);
  result.revalidate();
  expect(actions.refresh).toHaveBeenCalledTimes(1);
  expect(actions.retry).not.toHaveBeenCalled();
  result.refresh();
  expect(actions.retry).toHaveBeenCalledExactlyOnceWith(atom, actions.refresh);
  expect(actions.refresh).toHaveBeenCalledTimes(2);
});

describe("formatEnvironmentQueryError", () => {
  it("names a transport cut-off instead of showing the raw socket error", () => {
    const cause = Cause.fail(
      new RpcClientError.RpcClientError({
        reason: new Socket.SocketCloseError({ code: 4408, closeReason: "liveness timeout" }),
      }),
    );
    expect(formatEnvironmentQueryError(cause)).toBe(QUERY_CONNECTION_DROPPED_MESSAGE);
    expect(QUERY_CONNECTION_DROPPED_MESSAGE).toBe(
      "The connection dropped before the result arrived.",
    );
  });

  it("keeps the message of any other error", () => {
    expect(formatEnvironmentQueryError(Cause.fail(new Error("Branch not found.")))).toBe(
      "Branch not found.",
    );
  });

  it("falls back when an error has no message", () => {
    expect(formatEnvironmentQueryError(Cause.fail("opaque"))).toBe(
      "The environment request failed.",
    );
  });

  it("treats a blank message as none and uses the view's fallback", () => {
    expect(formatEnvironmentQueryError(Cause.fail(new Error("  \n ")))).toBe(
      "The environment request failed.",
    );
    expect(formatEnvironmentQueryError(Cause.fail(new Error(" ")), "Failed to load refs.")).toBe(
      "Failed to load refs.",
    );
    expect(formatEnvironmentQueryError(Cause.fail("opaque"), "Workspace query failed.")).toBe(
      "Workspace query failed.",
    );
  });

  it("names an attempt interrupted by a closing session like a cut-off, not with Effect's text", () => {
    expect(formatEnvironmentQueryError(Cause.interrupt())).toBe(QUERY_CONNECTION_DROPPED_MESSAGE);
    expect(formatEnvironmentQueryError(Cause.interrupt(), "Workspace query failed.")).toBe(
      QUERY_CONNECTION_DROPPED_MESSAGE,
    );
  });

  it("keeps a typed failure's message when an interrupt came with it", () => {
    const cause = Cause.combine(Cause.fail(new Error("Branch not found.")), Cause.interrupt());
    expect(formatEnvironmentQueryError(cause)).toBe("Branch not found.");
  });

  it("names a cut-off the same way whatever the view's fallback", () => {
    const cause = Cause.fail(
      new RpcClientError.RpcClientError({
        reason: new Socket.SocketCloseError({ code: 1006, closeReason: "" }),
      }),
    );
    expect(formatEnvironmentQueryError(cause, "Failed to load refs.")).toBe(
      QUERY_CONNECTION_DROPPED_MESSAGE,
    );
  });
});
