// @vitest-environment happy-dom

/**
 * `useEnvironmentQuery`'s `requiresRetry`, rendered through the React Compiler.
 *
 * The web package compiles happy-dom files as the client build does (docs/testing/README.md,
 * "Web unit tests and the React Compiler"), so the hook's memoization runs for real here.
 * A query latches together with a failure emission, and the rendered `requiresRetry` must
 * follow that emission. A hook that computed it from the atom alone would have its read
 * cached on the atom and keep rendering the value from mount; that was the T15-g bug, when
 * the file browser never offered Retry after the latch engaged.
 */
import { RegistryContext } from "@effect/atom-react";
import * as Cause from "effect/Cause";
import { AsyncResult, Atom, AtomRegistry } from "effect/reactivity";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

/** The query runtime's Retry latch, keyed by the query atom. */
const latches = vi.hoisted(() => new WeakSet<object>());

vi.mock("@bibcode/client-runtime/state/runtime", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@bibcode/client-runtime/state/runtime")>()),
  /**
   * The runtime's contract: Retry is awaited only while the rendered emission is a latched
   * failure. Called without an emission it reads the latch alone, as the hook did before
   * T15-g; that branch exists so the red can be shown by restoring the old one-argument call.
   */
  isEnvironmentQueryAwaitingRetry: (
    atom: object,
    emission?: AsyncResult.AsyncResult<unknown, unknown>,
  ) => (emission === undefined || emission._tag === "Failure") && latches.has(atom),
}));

import { useEnvironmentQuery } from "./query";

type QueryResult = AsyncResult.AsyncResult<string, Error>;

const cutOff = (): QueryResult =>
  AsyncResult.failure(Cause.fail(new Error("The session ended before the result arrived.")));

function QueryProbe({ atom }: { readonly atom: Atom.Writable<QueryResult> }) {
  const view = useEnvironmentQuery(atom);
  return <output>{`${view.emission._tag} requiresRetry=${view.requiresRetry}`}</output>;
}

let container: HTMLDivElement;
let root: Root;
let registry: AtomRegistry.AtomRegistry;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  registry = AtomRegistry.make();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  registry.dispose();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

async function renderProbe(atom: Atom.Writable<QueryResult>): Promise<void> {
  await act(async () =>
    root.render(
      <RegistryContext.Provider value={registry}>
        <QueryProbe atom={atom} />
      </RegistryContext.Provider>,
    ),
  );
}

describe("useEnvironmentQuery under the React Compiler", () => {
  it("offers Retry once the latch engages with a new emission of the same query", async () => {
    const atom = Atom.make<QueryResult>(AsyncResult.initial(true));
    await renderProbe(atom);
    expect(container.textContent).toBe("Initial requiresRetry=false");

    // The first cut-off waits for the automatic re-issue: no latch yet.
    await act(async () => registry.set(atom, cutOff()));
    expect(container.textContent).toBe("Failure requiresRetry=false");

    // The re-issue failed too: the runtime latches and emits that failure.
    await act(async () => {
      latches.add(atom);
      registry.set(atom, cutOff());
    });
    expect(container.textContent).toBe("Failure requiresRetry=true");
  });

  it("stops offering Retry when the next emission is not the latched failure", async () => {
    const atom = Atom.make<QueryResult>(cutOff());
    latches.add(atom);
    await renderProbe(atom);
    expect(container.textContent).toBe("Failure requiresRetry=true");

    // Retry cleared the latch and the request answered.
    await act(async () => {
      latches.delete(atom);
      registry.set(atom, AsyncResult.success("page"));
    });
    expect(container.textContent).toBe("Success requiresRetry=false");
  });
});
