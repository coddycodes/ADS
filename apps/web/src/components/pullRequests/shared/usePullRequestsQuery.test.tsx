// @vitest-environment happy-dom
import { AsyncResult, Atom } from "effect/reactivity";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { EnvironmentQueryView } from "../../../state/query";

const h = vi.hoisted(() => ({
  view: null as EnvironmentQueryView<{ rows: number[] }, unknown> | null,
}));

vi.mock("../../../state/query", () => ({
  useEnvironmentQuery: () => h.view!,
}));

import { usePullRequestsQuery } from "./usePullRequestsQuery";

const atomA = Atom.make(AsyncResult.success({ rows: [1] }));
const atomB = Atom.make(AsyncResult.success({ rows: [2] }));

function Probe({ atom }: { atom: typeof atomA | typeof atomB }) {
  const query = usePullRequestsQuery(atom, { freshOnOpen: true });
  return (
    <div>
      <span data-testid="data">{query.data === null ? "null" : JSON.stringify(query.data)}</span>
      <span data-testid="error">{query.error ?? "null"}</span>
    </div>
  );
}

let root: Root;
let container: HTMLDivElement;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  h.view = {
    data: { rows: [1] },
    error: null,
    isPending: false,
    emission: AsyncResult.success({ rows: [1] }),
    requiresRetry: false,
    refresh: () => {},
    revalidate: () => {},
  };
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

async function renderProbe(atom: typeof atomA | typeof atomB = atomA) {
  await act(async () => root.render(<Probe atom={atom} />));
}

describe("usePullRequestsQuery", () => {
  it("keeps the last displayed data when a refresh fails", async () => {
    await renderProbe();
    expect(container.querySelector('[data-testid="data"]')?.textContent).toBe('{"rows":[1]}');

    h.view = {
      data: null,
      error: "GitLab timed out",
      isPending: false,
      emission: AsyncResult.fail("GitLab timed out"),
      requiresRetry: false,
      refresh: () => {},
      revalidate: () => {},
    };
    await renderProbe();

    expect(container.querySelector('[data-testid="data"]')?.textContent).toBe('{"rows":[1]}');
    expect(container.querySelector('[data-testid="error"]')?.textContent).toBe("GitLab timed out");
  });

  it("does not keep the previous atom's data after switching atoms", async () => {
    await renderProbe(atomA);
    expect(container.querySelector('[data-testid="data"]')?.textContent).toBe('{"rows":[1]}');

    h.view = {
      data: null,
      error: "GitLab timed out",
      isPending: false,
      emission: AsyncResult.fail("GitLab timed out"),
      requiresRetry: false,
      refresh: () => {},
      revalidate: () => {},
    };
    await renderProbe(atomB);

    expect(container.querySelector('[data-testid="data"]')?.textContent).toBe("null");
  });
});
