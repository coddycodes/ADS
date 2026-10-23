// @vitest-environment happy-dom

import { RegistryContext } from "@effect/atom-react";
import { EnvironmentId } from "@bibcode/contracts";
import type { RemoteUpdateRunState } from "@bibcode/client-runtime/state/remoteUpdateCoordinator";
import { AsyncResult, AtomRegistry } from "effect/reactivity";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

// The command boundary is mocked below; no connection services should start in this UI test.
vi.mock("../connection/runtime", async () => {
  const { Atom } = await import("effect/reactivity");
  const Layer = await import("effect/Layer");
  return { connectionAtomRuntime: Atom.runtime(Layer.empty) };
});

vi.mock("../components/ui/toast", async () => ({
  ...(await import("../components/ui/toastHelpers")),
  toastManager: { add: vi.fn() },
}));

import { toastManager } from "../components/ui/toast";
import {
  type RemoteUpdateConfirmationRequest,
  remoteUpdateConfirmationRequest,
  remoteUpdateEnvironment,
  requestRemoteUpdateConfirmation,
  startRemoteUpdate,
  useRequestRemoteUpdateConfirmation,
  useStartRemoteUpdate,
} from "./remoteUpdates";

const request: RemoteUpdateConfirmationRequest = {
  environmentId: EnvironmentId.make("ai-server"),
  name: "Ai-server",
  targetVersion: "0.6.4",
  progress: true,
};

let registry: AtomRegistry.AtomRegistry;
let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  registry = AtomRegistry.make();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  vi.mocked(toastManager.add).mockClear();
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  registry.dispose();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function UpdateControls({ value }: { readonly value: RemoteUpdateConfirmationRequest }) {
  const confirm = useRequestRemoteUpdateConfirmation();
  const start = useStartRemoteUpdate();
  return (
    <>
      <button onClick={() => confirm(value)}>Confirm</button>
      <button onClick={() => start(value)}>Start</button>
    </>
  );
}

describe("remote update confirmation requests", () => {
  it("sets and retains the original request without a mounted dialog", async () => {
    expect(registry.get(remoteUpdateConfirmationRequest)).toBeNull();
    requestRemoteUpdateConfirmation(registry, request);
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    expect(registry.get(remoteUpdateConfirmationRequest)).toBe(request);
  });

  it("binds the request hook to RegistryContext and uses the current request", () => {
    const render = (value: RemoteUpdateConfirmationRequest) =>
      act(() =>
        root.render(
          <RegistryContext.Provider value={registry}>
            <UpdateControls value={value} />
          </RegistryContext.Provider>,
        ),
      );
    render(request);
    act(() => container.querySelector("button")?.click());
    expect(registry.get(remoteUpdateConfirmationRequest)).toBe(request);
    const next = { ...request, name: "Other server", environmentId: EnvironmentId.make("other") };
    render(next);
    act(() => container.querySelector("button")?.click());
    expect(registry.get(remoteUpdateConfirmationRequest)).toBe(next);
  });
});

describe("startRemoteUpdate", () => {
  it.each<[RemoteUpdateRunState, { type: string; title: string }]>([
    [
      { phase: "succeeded", version: "0.6.5" },
      { type: "success", title: "Ai-server updated to v0.6.5" },
    ],
    [{ phase: "up-to-date" }, { type: "info", title: "Ai-server is already up to date" }],
  ])("adds one outcome toast for %j without a view abort signal", async (outcome, expected) => {
    const update = vi
      .spyOn(remoteUpdateEnvironment.update, "run")
      .mockResolvedValue(AsyncResult.success(outcome));
    startRemoteUpdate(registry, request);
    await vi.waitFor(() => expect(toastManager.add).toHaveBeenCalledExactlyOnceWith(expected));
    expect(update).toHaveBeenCalledExactlyOnceWith(
      registry,
      { environmentId: request.environmentId, input: {} },
      { signal: undefined },
    );
  });

  it("explains failure and reopens confirmation on Retry without starting again", async () => {
    const update = vi.spyOn(remoteUpdateEnvironment.update, "run").mockResolvedValue(
      AsyncResult.success<RemoteUpdateRunState>({
        phase: "failed",
        failure: { kind: "not-back" },
      }),
    );
    startRemoteUpdate(registry, request);
    await vi.waitFor(() => expect(toastManager.add).toHaveBeenCalledOnce());
    const toast = vi.mocked(toastManager.add).mock.calls[0]?.[0];
    expect(toast).toEqual({
      type: "error",
      title: "Update failed",
      description:
        "Ai-server hasn't come back after the update. Check BiBCode on Ai-server; it may be on a different port.",
      data: { actionLayout: "stacked-end", actionVariant: "outline" },
      actionProps: { children: "Retry", onClick: expect.any(Function) },
    });
    expect(registry.get(remoteUpdateConfirmationRequest)).toBeNull();
    act(() => root.render(<button {...toast?.actionProps} />));
    act(() => container.querySelector("button")?.click());
    expect(registry.get(remoteUpdateConfirmationRequest)).toBe(request);
    expect(update).toHaveBeenCalledOnce();
    expect(toastManager.add).toHaveBeenCalledOnce();
  });

  it("keeps the start hook's outcome toast alive after its view unmounts", async () => {
    type UpdateResult = Awaited<ReturnType<typeof remoteUpdateEnvironment.update.run>>;
    let resolveUpdate: ((result: UpdateResult) => void) | undefined;
    const pending = new Promise<UpdateResult>((resolve) => {
      resolveUpdate = resolve;
    });
    const update = vi.spyOn(remoteUpdateEnvironment.update, "run").mockReturnValue(pending);
    act(() =>
      root.render(
        <RegistryContext.Provider value={registry}>
          <UpdateControls value={request} />
        </RegistryContext.Provider>,
      ),
    );
    act(() => container.querySelectorAll("button")[1]?.click());
    expect(update).toHaveBeenCalledExactlyOnceWith(
      registry,
      { environmentId: request.environmentId, input: {} },
      { signal: undefined },
    );
    act(() => root.render(null));
    expect(toastManager.add).not.toHaveBeenCalled();
    resolveUpdate?.(AsyncResult.success({ phase: "succeeded", version: "0.6.4" }));
    await vi.waitFor(() =>
      expect(toastManager.add).toHaveBeenCalledExactlyOnceWith({
        type: "success",
        title: "Ai-server updated to v0.6.4",
      }),
    );
  });
});
