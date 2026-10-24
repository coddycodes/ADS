// @vitest-environment happy-dom
import {
  EnvironmentId,
  WS_METHODS,
  type GitManagerCommitEntry,
  type GitManagerCommitPage,
  type GitManagerGetCommitsInput,
} from "@bibcode/contracts";
import { RegistryContext } from "@effect/atom-react";
import { createGitManagerEnvironmentAtoms } from "@bibcode/client-runtime/state/git-manager";
import {
  EnvironmentRegistry,
  EnvironmentSupervisor,
  Wakeups,
} from "@bibcode/client-runtime/connection";
import type { RpcSession } from "@bibcode/client-runtime/rpc";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { Atom, AtomRegistry } from "effect/reactivity";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, vi } from "vite-plus/test";
import { it } from "@effect/vitest";

const h = vi.hoisted(() => ({
  page: null as GitManagerCommitPage | null,
  retained: null as ReadonlyArray<GitManagerCommitPage> | null,
  atoms: null as ReturnType<typeof createGitManagerEnvironmentAtoms> | null,
  requests: [] as GitManagerGetCommitsInput[],
  completed: 0,
  pending: 0,
}));
vi.mock("../../../state/gitManager", () => ({
  gitManagerEnvironment: {
    getHistoryFirstPage: (
      ...args: Parameters<NonNullable<typeof h.atoms>["getHistoryFirstPage"]>
    ) => h.atoms!.getHistoryFirstPage(...args),
    getCommits: (...args: Parameters<NonNullable<typeof h.atoms>["getCommits"]>) =>
      h.atoms!.getCommits(...args),
    getRetainedCommitPages: (
      ...args: Parameters<NonNullable<typeof h.atoms>["getRetainedCommitPages"]>
    ) => h.atoms!.getRetainedCommitPages(...args),
  },
}));
vi.mock("../../../localApi", () => ({ readLocalApi: () => ({ contextMenu: { show: vi.fn() } }) }));
vi.mock("./GitManagerCommitDetail", () => ({ GitManagerCommitDetail: () => null }));
vi.mock("../rewrite/gitManagerCommitDrag", () => ({
  GitManagerCommitDndContext: ({ children }: { children: React.ReactNode }) => children,
  GitManagerCommitInsertionTarget: () => null,
  useGitManagerCommitDragSource: () => ({
    attributes: {},
    listeners: {},
    setNodeRef: () => {},
    isDragging: false,
  }),
}));

import { GitManagerHistoryView } from "./GitManagerHistoryView";

function commit(index: number, decorations: string[] = []): GitManagerCommitEntry {
  const sha = index.toString(16).padStart(40, "0");
  return {
    sha,
    shortSha: sha.slice(-7),
    parents: [],
    decorations,
    subject: `Commit ${index}`,
    body: "",
    authorName: "Author",
    authorEmail: "author@example.test",
    authoredAtMs: index,
    committerName: "Author",
    committerEmail: "author@example.test",
    committedAtMs: index,
    changedFiles: [],
  };
}

afterEach(() => vi.restoreAllMocks());

it.effect.each([
  { height: 250, overlap: "all", staleRetained: false },
  { height: 700, overlap: "all", staleRetained: false },
  { height: 250, overlap: "none", staleRetained: false },
  { height: 250, overlap: "all", staleRetained: true },
  { height: 250, overlap: "head", staleRetained: true },
])(
  "updates old HEAD through real queries and virtualization (%j)",
  ({ height, overlap, staleRetained }) =>
    Effect.gen(function* () {
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
      vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue(
        new DOMRect(0, 0, 900, height),
      );
      vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(height);
      vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(900);
      const container = document.createElement("div");
      document.body.append(container);
      const root = createRoot(container);
      const original = Array.from({ length: 12 }, (_, index) =>
        commit(12 - index, index === 0 ? ["HEAD -> main"] : []),
      );
      const initialPage: GitManagerCommitPage = {
        generation: 1,
        pinnedTips: [original[0]!.sha],
        commits: original,
        exhausted: true,
        nextOffset: null,
        degradedToAllPaging: false,
      };
      h.page = initialPage;
      h.retained = null;
      h.requests = [];
      h.completed = 0;
      h.pending = 0;
      const registry = AtomRegistry.make();
      h.atoms = yield* Effect.gen(function* () {
        const session: RpcSession = {
          client: {
            [WS_METHODS.gitManagerGetCommits]: (input: GitManagerGetCommitsInput) =>
              Effect.promise(async () => {
                h.requests.push(input);
                h.pending += 1;
                const page = structuredClone(
                  input.pinnedTips === undefined ? h.page : h.retained?.[0],
                );
                await new Promise((resolve) =>
                  setTimeout(resolve, input.pinnedTips === undefined ? 10 : 25),
                );
                h.pending -= 1;
                h.completed += 1;
                return page;
              }),
          } as never,
          ready: Effect.void,
          probe: Effect.void,
          closed: Effect.never,
          initialConfig: Effect.never,
          e2eeAuthenticated: Effect.succeed(null),
        };
        const supervisor = EnvironmentSupervisor.of({
          target: { environmentId: EnvironmentId.make("external-refresh-test"), label: "test" },
          session: yield* SubscriptionRef.make(Option.some(session)),
          state: yield* SubscriptionRef.make({ phase: "connected", generation: 1 }),
        } as never);
        const environment = EnvironmentRegistry.of({
          run: <A, E>(_: unknown, effect: Effect.Effect<A, E, EnvironmentSupervisor>) =>
            Effect.provideService(effect, EnvironmentSupervisor, supervisor),
          followStream: (
            _: unknown,
            stream: Stream.Stream<unknown, unknown, EnvironmentSupervisor>,
          ) => Stream.provideService(stream, EnvironmentSupervisor, supervisor),
        } as never);
        return createGitManagerEnvironmentAtoms(
          Atom.runtime(
            Layer.mergeAll(
              Layer.succeed(EnvironmentRegistry, environment),
              Wakeups.layer({ changes: Stream.never, focusVisibility: Stream.never }),
            ),
          ),
        );
      });
      yield* Effect.promise(async () => {
        const render = (repositoryGeneration: number, signalGeneration: number) =>
          root.render(
            <RegistryContext.Provider value={registry}>
              <GitManagerHistoryView
                scope={{ environmentId: "external-refresh-test" as never, cwd: "/repo" }}
                projectRef={
                  { environmentId: "external-refresh-test", projectId: "project" } as never
                }
                blockedReasons={[]}
                branchSyncDisabledReason={null}
                rewriteDisabledReason={null}
                tagDisabledReason={null}
                repositoryGeneration={repositoryGeneration}
                signalGeneration={signalGeneration}
                signalPending={false}
                onAction={() => {}}
              />
            </RegistryContext.Provider>,
          );
        try {
          await act(async () => render(1, 10));
          await vi.waitFor(async () => {
            await act(async () => {
              await Promise.resolve();
            });
            expect(container.querySelectorAll("[data-commit-sha]").length).toBeGreaterThan(1);
          });
          await act(async () =>
            container
              .querySelector<HTMLButtonElement>(`button[data-commit-sha="${original[0]!.sha}"]`)
              ?.click(),
          );
          const refreshed = original.map((entry) => ({ ...entry, decorations: [] }));
          const currentPage: GitManagerCommitPage = {
            ...initialPage,
            generation: 2,
            pinnedTips: [commit(13).sha],
            commits: [
              commit(13, ["HEAD -> main"]),
              ...(overlap === "all" ? refreshed : overlap === "head" ? refreshed.slice(0, 1) : []),
            ],
          };
          h.page = currentPage;
          h.retained = [
            {
              ...currentPage,
              generation: staleRetained ? 1 : 2,
              pinnedTips: [original[0]!.sha],
              commits: staleRetained ? original : refreshed,
            },
          ];
          await act(async () => render(1, 11));
          await act(async () => render(2, 11));
          await vi.waitFor(async () => {
            await act(async () => {
              await Promise.resolve();
            });
            expect(h.completed).toBeGreaterThanOrEqual(2);
            expect(h.pending).toBe(0);
          });
          await vi.waitFor(async () => {
            await act(async () => {
              await Promise.resolve();
            });
            const rows = [
              ...container.querySelectorAll<HTMLButtonElement>("button[data-commit-sha]"),
            ];
            expect(rows.some((row) => row.dataset.commitSha === commit(13).sha)).toBe(true);
            expect(rows.filter((row) => row.textContent?.includes("HEAD -> main"))).toHaveLength(1);
            expect(
              rows.find((row) => row.dataset.commitSha === original[0]!.sha)?.textContent,
            ).not.toContain("HEAD -> main");
            expect(
              rows
                .find((row) => row.dataset.commitSha === original[0]!.sha)
                ?.getAttribute("aria-selected"),
            ).toBe("true");
          });
          if (overlap === "all")
            expect(h.requests.filter((request) => request.pinnedTips !== undefined)).toEqual([]);
        } finally {
          await act(async () => root.unmount());
          registry.dispose();
          container.remove();
        }
      });
    }),
);
