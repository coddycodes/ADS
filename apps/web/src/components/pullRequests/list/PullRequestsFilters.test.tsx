// @vitest-environment happy-dom
import type { PullRequestsVocabulary, PullRequestsVocabularyInput } from "@coddy/contracts";
import { act, useEffect, useMemo, useReducer, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import {
  DEFAULT_PULL_REQUESTS_FILTERS,
  type PullRequestsFilters as Filters,
} from "../../../pullRequestsStore";
import { context } from "../testFixtures";
const h = vi.hoisted(() => ({
  vocabulary: vi.fn((args: { input: PullRequestsVocabularyInput }) => args),
  filters: null as Filters | null,
  changes: vi.fn(),
  entries: [
    { id: "bug", label: "bug", color: "ff0000", description: null },
  ] as PullRequestsVocabulary["entries"],
}));
vi.mock("../../../state/pullRequests", () => ({
  pullRequestsEnvironment: { getVocabulary: h.vocabulary },
}));
vi.mock("../../../state/query", () => ({
  useEnvironmentQuery: (atom: unknown) => {
    const [revision, publish] = useReducer((value: number) => value + 1, 0);
    const data = useMemo(
      () => (atom ? { entries: h.entries, truncated: false } : null),
      [atom, revision],
    );
    return {
      data,
      emission: { _tag: "Initial" },
      error: null,
      isPending: false,
      refresh: publish,
      revalidate: publish,
      requiresRetry: false,
    };
  },
}));
import { PullRequestsFilters } from "./PullRequestsFilters";
let container: HTMLDivElement;
let root: Root;
function Wrapper({ combined = true }: { combined?: boolean }) {
  const [filters, setFilters] = useState(DEFAULT_PULL_REQUESTS_FILTERS);
  const [resetKey, reset] = useState(0);
  useEffect(() => {
    h.filters = filters;
  }, [filters]);
  return (
    <PullRequestsFilters
      scope={{ environmentId: "env" as never, cwd: "/repo" }}
      context={{
        ...context,
        capabilities: { ...context.capabilities, closedTabIncludesMerged: combined },
      }}
      filters={filters}
      sort="newest"
      resetKey={resetKey}
      onFiltersChange={(patch) => {
        h.changes(patch);
        setFilters((current) => ({ ...current, ...patch }));
      }}
      onSortChange={() => undefined}
      onClear={() => {
        setFilters(DEFAULT_PULL_REQUESTS_FILTERS);
        reset((v) => v + 1);
      }}
    />
  );
}
function button(label: string) {
  const result = document.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`);
  if (!result) throw new Error(`Missing ${label}`);
  return result;
}
async function typeSearch(text: string) {
  const input = container.querySelector<HTMLInputElement>(
    'input[aria-label="Search pull requests"]',
  )!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, text);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  return input;
}
beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  h.vocabulary.mockClear();
  h.changes.mockClear();
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.useRealTimers();
});
describe("PullRequestsFilters", () => {
  it("debounces search for 300 ms and applies Enter immediately", async () => {
    vi.useFakeTimers();
    await act(async () => root.render(<Wrapper />));
    const input = await typeSearch("fix");
    await act(async () => vi.advanceTimersByTime(299));
    expect(h.changes).not.toHaveBeenCalled();
    await act(async () => vi.advanceTimersByTime(1));
    expect(h.filters?.search).toBe("fix");
    await typeSearch("now");
    await act(async () =>
      input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true })),
    );
    expect(h.filters?.search).toBe("now");
  });
  it("clears pending search without allowing the debounce to restore it", async () => {
    vi.useFakeTimers();
    await act(async () => root.render(<Wrapper />));
    await typeSearch("pending");
    await act(async () =>
      [...container.querySelectorAll("button")]
        .find((b) => b.textContent === "Clear filters")!
        .click(),
    );
    await act(async () => vi.advanceTimersByTime(1000));
    expect(h.filters?.search).toBeNull();
    expect(container.querySelector("input")?.value).toBe("");
  });
  it("loads labels only when opened and supports multiple selected labels", async () => {
    h.entries = [
      { id: "bug", label: "bug", color: null, description: null },
      { id: "docs", label: "docs", color: null, description: null },
    ];
    await act(async () => root.render(<Wrapper />));
    expect(h.vocabulary).not.toHaveBeenCalled();
    await act(async () => button("Labels").click());
    expect(h.vocabulary).toHaveBeenCalledWith({
      environmentId: "env",
      input: { cwd: "/repo", kind: "labels", query: null },
    });
    await act(async () =>
      document.querySelector<HTMLElement>('[role="menuitemcheckbox"]')!.click(),
    );
    expect(h.filters?.labels).toEqual(["bug"]);
    await act(async () =>
      [...document.querySelectorAll<HTMLElement>('[role="menuitemcheckbox"]')]
        .find((e) => e.textContent === "docs")!
        .click(),
    );
    expect(h.filters?.labels).toEqual(["bug", "docs"]);
  });
  it("selects Me and exposes only the host's review-status options", async () => {
    await act(async () => root.render(<Wrapper combined={false} />));
    await act(async () => button("Author").click());
    await act(async () =>
      [...document.querySelectorAll<HTMLElement>('[role="menuitemradio"]')]
        .find((e) => e.textContent === "Me")!
        .click(),
    );
    expect(h.filters?.author).toBe("@me");
    await act(async () => button("Review status").click());
    expect(document.body.textContent).toContain("Not approved");
    expect(document.body.textContent).not.toContain("Changes requested");
  });
});
