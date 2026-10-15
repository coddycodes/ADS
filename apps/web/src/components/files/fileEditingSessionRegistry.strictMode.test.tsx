// @vitest-environment happy-dom

import { StrictMode, useEffect, useMemo } from "react";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { FileEditingSessionRegistry } from "./fileEditingSessionRegistry";

function fakeSession(relativePath: string) {
  return {
    relativePath,
    flush: vi.fn(async () => "saved" as const),
    settle: vi.fn(async () => "saved" as const),
    setAutosaveEnabled: vi.fn(),
    pauseSaving: vi.fn(),
    resumeSaving: vi.fn(),
    discardPendingSave: vi.fn(),
    rename: vi.fn(function rename(this: { relativePath: string }, next: string) {
      this.relativePath = next;
    }),
    dispose: vi.fn(),
  };
}

type TestSession = ReturnType<typeof fakeSession>;
type TestRegistry = FileEditingSessionRegistry<TestSession> & {
  acquireOwnership?: () => () => void;
};

function RegistryOwner({
  openRelativePaths,
  activeRelativePath,
  onRegistry,
  onSession,
}: {
  openRelativePaths: readonly string[];
  activeRelativePath: string | null;
  onRegistry: (registry: TestRegistry) => void;
  onSession: (session: TestSession) => void;
}) {
  // Keyed by lifetime, so a new lifetime remounts the owner with a fresh registry.
  const registry = useMemo(() => new FileEditingSessionRegistry<TestSession>() as TestRegistry, []);

  useEffect(() => {
    const releaseOwnership = registry.acquireOwnership?.();
    return releaseOwnership ?? (() => void registry.dispose());
  }, [registry]);

  useEffect(() => {
    void registry.reconcile(openRelativePaths);
  }, [openRelativePaths, registry]);

  useEffect(() => {
    onRegistry(registry);
    if (activeRelativePath !== null) {
      onSession(registry.getOrCreate(activeRelativePath, () => fakeSession(activeRelativePath)));
    }
    registry.setActivePath(activeRelativePath);
  }, [activeRelativePath, onRegistry, onSession, registry]);

  return null;
}

describe("FileEditingSessionRegistry StrictMode ownership", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(async () => {
    await act(async () => root.unmount());
    container.remove();
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
  });

  it("survives effect rehearsal, tool transitions, mutation release, lifetime replacement, and unmount", async () => {
    const registries: TestRegistry[] = [];
    const sessions: TestSession[] = [];
    const onRegistry = (registry: TestRegistry) => {
      if (!registries.includes(registry)) registries.push(registry);
    };
    const onSession = (session: TestSession) => {
      if (!sessions.includes(session)) sessions.push(session);
    };
    const renderOwner = async (
      lifetimeKey: string,
      openRelativePaths: readonly string[],
      activeRelativePath: string | null,
    ) => {
      await act(async () => {
        root.render(
          <StrictMode>
            <RegistryOwner
              key={lifetimeKey}
              openRelativePaths={openRelativePaths}
              activeRelativePath={activeRelativePath}
              onRegistry={onRegistry}
              onSession={onSession}
            />
          </StrictMode>,
        );
      });
    };

    await renderOwner("project-a:thread-a", ["src/a.ts"], "src/a.ts");
    expect(registries).toHaveLength(1);
    expect(sessions).toHaveLength(1);
    expect(sessions[0]!.flush).not.toHaveBeenCalled();
    expect(sessions[0]!.dispose).not.toHaveBeenCalled();

    await renderOwner("project-a:thread-a", ["src/a.ts"], null);
    expect(registries).toHaveLength(1);
    expect(sessions).toHaveLength(1);
    expect(sessions[0]!.flush).toHaveBeenCalledOnce();
    expect(sessions[0]!.dispose).not.toHaveBeenCalled();

    const lease = await registries[0]!.beginPathMutation({
      kind: "delete",
      relativePath: "src/a.ts",
    });
    expect(lease).not.toBeNull();
    await renderOwner("project-b:thread-b", ["src/b.ts"], "src/b.ts");
    await vi.waitFor(() => {
      expect(registries).toHaveLength(2);
      expect(sessions).toHaveLength(2);
    });
    expect(sessions[0]!.dispose).not.toHaveBeenCalled();

    lease!.release();
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(sessions[0]!.settle).toHaveBeenCalledTimes(2);
    expect(sessions[0]!.dispose).toHaveBeenCalledOnce();
    expect(sessions[1]!.dispose).not.toHaveBeenCalled();

    await act(async () => root.unmount());
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(sessions[1]!.settle).toHaveBeenCalledOnce();
    expect(sessions[1]!.dispose).toHaveBeenCalledOnce();
  });
});
