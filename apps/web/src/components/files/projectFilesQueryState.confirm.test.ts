import type { EnvironmentSupervisor as SupervisorService } from "@bibcode/client-runtime/connection";
import { EnvironmentId, type ProjectReadFileResult } from "@bibcode/contracts";
import type { Effect as EffectOf } from "effect/Effect";
import * as Option from "effect/Option";
import type { Stream as StreamOf } from "effect/Stream";
import { AsyncResult } from "effect/reactivity";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";

/**
 * The file server behind the real query family: `readFile` returns what is on
 * disk when the read starts, and a test can hold reads to observe what the panel
 * resolves to while one is in flight.
 */
const server = vi.hoisted(() => ({
  disk: "",
  reads: 0,
  hold: null as Promise<void> | null,
}));

vi.mock("~/state/projects", async () => {
  const Effect = await import("effect/Effect");
  const Layer = await import("effect/Layer");
  const { none } = await import("effect/Option");
  const Stream = await import("effect/Stream");
  const SubscriptionRef = await import("effect/SubscriptionRef");
  const { Atom } = await import("effect/reactivity");
  const { EnvironmentRegistry, EnvironmentSupervisor } =
    await import("@bibcode/client-runtime/connection");
  const { createEnvironmentQueryAtomFamily } =
    await import("@bibcode/client-runtime/state/runtime");

  const supervisor = EnvironmentSupervisor.of({
    target: { environmentId: "files-confirm", label: "Files confirm test" },
    session: Effect.runSync(SubscriptionRef.make(none())),
    state: Effect.runSync(SubscriptionRef.make({ phase: "connected", generation: 1 })),
  } as never);
  const environment = EnvironmentRegistry.of({
    run: <A, E>(_: unknown, effect: EffectOf<A, E, SupervisorService>) =>
      Effect.provideService(effect, EnvironmentSupervisor, supervisor),
    followStream: <A, E>(_: unknown, stream: StreamOf<A, E, SupervisorService>) =>
      Stream.provideService(stream, EnvironmentSupervisor, supervisor),
  } as never);
  const optimisticFile = Atom.family((_key: string) => Atom.make<unknown>(null));
  return {
    projectEnvironment: {
      // The same family and cache policy as the app's `readFile`; only the transport is faked.
      readFile: createEnvironmentQueryAtomFamily(
        Atom.runtime(Layer.succeed(EnvironmentRegistry, environment)),
        {
          label: "test:files-confirm:read-file",
          staleTimeMs: 30_000,
          idleTtlMs: 5 * 60_000,
          execute: (input: { cwd: string; relativePath: string }) =>
            Effect.promise(async () => {
              server.reads += 1;
              const contents = server.disk;
              await server.hold;
              return {
                relativePath: input.relativePath,
                contents,
                byteLength: contents.length,
                truncated: false,
              };
            }),
        },
      ),
      optimisticFile: (target: { environmentId: string; cwd: string; relativePath: string }) =>
        optimisticFile(JSON.stringify([target.environmentId, target.cwd, target.relativePath])),
    },
  };
});

import { appAtomRegistry } from "~/rpc/atomRegistry";

import {
  clearProjectFileQueryData,
  confirmProjectFileQueryData,
  getOptimisticProjectFileQueryData,
  getProjectFileQueryAtom,
  resolveProjectFileQueryData,
  setProjectFileQueryData,
} from "./projectFilesQueryState";

const environmentId = EnvironmentId.make("environment-files-confirm");

function queryContents(): string | null {
  const result = appAtomRegistry.get(getProjectFileQueryAtom(environmentId, "/repo", "README.md"));
  return Option.getOrNull(AsyncResult.value(result))?.contents ?? null;
}

/** What `useProjectFileQuery` hands the panel: the overlay, else the query. */
function panelContents(): string | undefined {
  const atom = getProjectFileQueryAtom(environmentId, "/repo", "README.md");
  const data = Option.getOrNull(AsyncResult.value(appAtomRegistry.get(atom)));
  return resolveProjectFileQueryData(
    environmentId,
    "/repo",
    "README.md",
    data as ProjectReadFileResult | null,
  )?.contents;
}

afterEach(() => {
  clearProjectFileQueryData(environmentId, "/repo", "README.md");
  server.hold = null;
});

describe("confirmProjectFileQueryData", () => {
  it("keeps the saved text until an unmounted, fresh file query has read it again", async () => {
    server.disk = "- [ ] first\n";
    const queryAtom = getProjectFileQueryAtom(environmentId, "/repo", "README.md");
    const unmount = appAtomRegistry.mount(queryAtom);
    await vi.waitFor(() => expect(queryContents()).toBe("- [ ] first\n"));
    // The panel lets go of the query; its idle TTL keeps the pre-save read cached,
    // and that read is still within its stale time.
    unmount();
    expect(server.reads).toBe(1);

    // The save lands on disk and is confirmed while the re-read is held.
    let releaseRead!: () => void;
    server.hold = new Promise((resolve) => {
      releaseRead = resolve;
    });
    server.disk = "- [ ] first\n- [ ] typed\n";
    setProjectFileQueryData(environmentId, "/repo", "README.md", server.disk);
    expect(confirmProjectFileQueryData(environmentId, "/repo", "README.md", server.disk)).toBe(
      true,
    );

    await vi.waitFor(() => expect(server.reads).toBe(2));
    expect(getOptimisticProjectFileQueryData(environmentId, "/repo", "README.md")?.contents).toBe(
      "- [ ] first\n- [ ] typed\n",
    );
    expect(panelContents()).toBe("- [ ] first\n- [ ] typed\n");

    releaseRead();
    await vi.waitFor(() =>
      expect(getOptimisticProjectFileQueryData(environmentId, "/repo", "README.md")).toBeNull(),
    );
    expect(queryContents()).toBe("- [ ] first\n- [ ] typed\n");
    expect(panelContents()).toBe("- [ ] first\n- [ ] typed\n");
  });
});
