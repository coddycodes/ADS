import type { Dispatch, ReactElement, SetStateAction } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import {
  type EnvironmentId,
  ProviderDriverKind,
  ProviderInstanceId,
  type ServerProvider,
} from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import { AsyncResult } from "effect/reactivity";

import type {
  LocalEnvironmentUpdateGroup,
  ProviderUpdateCandidate,
  ProviderUpdateRowStatus,
} from "./ProviderUpdateLaunchNotification.logic";

const testState = vi.hoisted(() => ({
  groups: [] as LocalEnvironmentUpdateGroup[],
  updateProvider: vi.fn(),
}));

const hooks = vi.hoisted(() => {
  let cursor = 0;
  let slots: unknown[] = [];

  const nextIndex = () => cursor++;

  return {
    beginRender() {
      cursor = 0;
    },
    reset() {
      cursor = 0;
      slots = [];
    },
    useCallback<T>(callback: T): T {
      nextIndex();
      return callback;
    },
    useMemo<T>(factory: () => T): T {
      nextIndex();
      return factory();
    },
    useMemoCache(size: number): unknown[] {
      const index = nextIndex();
      if (!slots[index]) {
        slots[index] = Array.from({ length: size }, () => Symbol.for("react.memo_cache_sentinel"));
      }
      return slots[index] as unknown[];
    },
    useRef<T>(initialValue: T): { current: T } {
      const index = nextIndex();
      if (!slots[index]) {
        slots[index] = { current: initialValue };
      }
      return slots[index] as { current: T };
    },
    useState<T>(initialValue: T | (() => T)): [T, Dispatch<SetStateAction<T>>] {
      const index = nextIndex();
      if (index >= slots.length) {
        slots[index] =
          typeof initialValue === "function" ? (initialValue as () => T)() : initialValue;
      }
      const setValue: Dispatch<SetStateAction<T>> = (nextValue) => {
        const previous = slots[index] as T;
        slots[index] =
          typeof nextValue === "function" ? (nextValue as (value: T) => T)(previous) : nextValue;
      };
      return [slots[index] as T, setValue];
    },
  };
});

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  return {
    ...actual,
    useCallback: hooks.useCallback,
    useMemo: hooks.useMemo,
    useRef: hooks.useRef,
    useState: hooks.useState,
  };
});

vi.mock("react/compiler-runtime", () => ({
  c: hooks.useMemoCache,
}));

vi.mock("~/state/server", () => ({
  serverEnvironment: { updateProvider: Symbol("updateProvider") },
}));

vi.mock("~/state/use-atom-command", () => ({
  useAtomCommand: () => testState.updateProvider,
}));

vi.mock("./ProviderUpdateLaunchNotification.environments", () => ({
  useLocalEnvironmentUpdateGroups: () => ({
    groups: testState.groups,
    isAnySettling: false,
  }),
}));

import { ProviderUpdateEnvironmentRows } from "./ProviderUpdateEnvironmentRows";

const environmentId = "env-wsl" as EnvironmentId;
const pendingExpiryMs = 6 * 60_000;

function provider(updateStatus?: "succeeded"): ServerProvider {
  const result: ServerProvider = {
    instanceId: ProviderInstanceId.make("codex-wsl"),
    driver: ProviderDriverKind.make("codex"),
    enabled: true,
    installed: true,
    version: updateStatus ? "1.1.0" : "1.0.0",
    status: "ready",
    auth: { status: "authenticated" },
    checkedAt: "2026-06-26T12:00:00.000Z",
    models: [],
    slashCommands: [],
    skills: [],
    agents: [],
    versionAdvisory: {
      status: updateStatus ? "current" : "behind_latest",
      currentVersion: updateStatus ? "1.1.0" : "1.0.0",
      latestVersion: "1.1.0",
      updateCommand: "npm install -g @openai/codex@latest",
      canUpdate: true,
      checkedAt: "2026-06-26T12:00:00.000Z",
      message: updateStatus ? "Up to date." : "Update available.",
    },
  };

  return updateStatus
    ? {
        ...result,
        updateState: {
          status: updateStatus,
          startedAt: "2026-06-26T12:00:00.000Z",
          finishedAt: "2026-06-26T12:00:01.000Z",
          message: "Provider updated.",
          output: null,
        },
      }
    : result;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((complete) => {
    resolve = complete;
  });
  return { promise, resolve };
}

type RowElement = ReactElement<{
  readonly status: ProviderUpdateRowStatus;
  readonly onUpdate: () => void;
}>;

function renderRow(onInteract?: () => void): RowElement {
  hooks.beginRender();
  const output = ProviderUpdateEnvironmentRows(onInteract ? { onInteract } : {}) as ReactElement<{
    readonly children: RowElement | RowElement[];
  }>;
  const children = output.props.children;
  return Array.isArray(children) ? children[0]! : children;
}

async function flushPromises(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

describe("ProviderUpdateEnvironmentRows", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    hooks.reset();
    testState.updateProvider.mockReset();
    const candidate = provider() as ProviderUpdateCandidate;
    testState.groups = [
      {
        environmentId,
        label: "WSL",
        isPrimary: false,
        isSettling: false,
        candidates: [candidate],
        providers: [candidate],
      },
    ];
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("keeps a successor pending when an expired request resolves late, then shows its success", async () => {
    const firstRequest =
      deferred<ReturnType<typeof AsyncResult.success<{ providers: ServerProvider[] }>>>();
    const successorRequest =
      deferred<ReturnType<typeof AsyncResult.success<{ providers: ServerProvider[] }>>>();
    testState.updateProvider
      .mockReturnValueOnce(firstRequest.promise)
      .mockReturnValueOnce(successorRequest.promise);

    renderRow().props.onUpdate();
    expect(renderRow().props.status.kind).toBe("loading");

    await vi.advanceTimersByTimeAsync(pendingExpiryMs);
    expect(renderRow().props.status.kind).toBe("failed");

    renderRow().props.onUpdate();
    expect(testState.updateProvider).toHaveBeenCalledTimes(2);
    expect(renderRow().props.status.kind).toBe("loading");

    firstRequest.resolve(AsyncResult.success({ providers: [provider("succeeded")] }));
    await flushPromises();

    expect(renderRow().props.status.kind).toBe("loading");

    successorRequest.resolve(AsyncResult.success({ providers: [provider("succeeded")] }));
    await flushPromises();

    expect(renderRow().props.status.kind).toBe("success");
  });

  it("renders no rows when no environment has update work", () => {
    testState.groups = [];
    hooks.beginRender();
    expect(ProviderUpdateEnvironmentRows({})).toBeNull();

    hooks.reset();
    testState.groups = [
      {
        environmentId,
        label: "WSL",
        isPrimary: false,
        isSettling: false,
        candidates: [],
        providers: [],
      },
    ];
    hooks.beginRender();
    expect(ProviderUpdateEnvironmentRows({})).toBeNull();
  });

  it("ignores rapid duplicate updates and invokes the interaction callback once", async () => {
    const request =
      deferred<ReturnType<typeof AsyncResult.success<{ providers: ServerProvider[] }>>>();
    testState.updateProvider.mockReturnValue(request.promise);
    const onInteract = vi.fn();
    const update = renderRow(onInteract).props.onUpdate;

    update();
    update();
    expect(testState.updateProvider).toHaveBeenCalledOnce();
    expect(onInteract).toHaveBeenCalledOnce();

    request.resolve(AsyncResult.success({ providers: [provider("succeeded")] }));
    await flushPromises();
    expect(renderRow(onInteract).props.status.kind).toBe("success");
  });

  it("shows rejected command errors and clears them before a successful retry", async () => {
    testState.updateProvider
      .mockResolvedValueOnce(AsyncResult.failure(Cause.fail(new Error("update denied"))))
      .mockResolvedValueOnce(AsyncResult.success({ providers: [provider("succeeded")] }));

    renderRow().props.onUpdate();
    await flushPromises();
    expect(renderRow().props.status).toMatchObject({ kind: "failed", text: "update denied" });

    renderRow().props.onUpdate();
    expect(renderRow().props.status.kind).toBe("loading");
    await flushPromises();
    expect(renderRow().props.status.kind).toBe("success");
  });

  it("uses generic errors for non-error command failures and thrown values", async () => {
    testState.updateProvider
      .mockResolvedValueOnce(AsyncResult.failure(Cause.fail("unknown")))
      .mockRejectedValueOnce("transport closed");

    renderRow().props.onUpdate();
    await flushPromises();
    expect(renderRow().props.status).toMatchObject({
      kind: "failed",
      text: "Provider update failed.",
    });

    renderRow().props.onUpdate();
    await flushPromises();
    expect(renderRow().props.status).toMatchObject({
      kind: "failed",
      text: "Provider update failed.",
    });
  });

  it("does not pin interrupted or missing-provider snapshots as running results", async () => {
    testState.updateProvider
      .mockResolvedValueOnce(AsyncResult.failure(Cause.interrupt()))
      .mockResolvedValueOnce(AsyncResult.success({ providers: [] }));

    renderRow().props.onUpdate();
    await flushPromises();
    expect(renderRow().props.status.kind).not.toBe("loading");

    renderRow().props.onUpdate();
    await flushPromises();
    expect(renderRow().props.status.kind).not.toBe("loading");
  });

  it("updates every candidate in one environment and reports unchanged outcomes", async () => {
    const unchangedState = {
      status: "unchanged" as const,
      startedAt: "2026-06-26T12:00:00.000Z",
      finishedAt: "2026-06-26T12:00:01.000Z",
      message: "Still outdated.",
      output: null,
    };
    const firstCandidate = {
      ...testState.groups[0]!.candidates[0]!,
      updateState: unchangedState,
    } as ProviderUpdateCandidate;
    const secondCandidate = {
      ...provider(),
      instanceId: ProviderInstanceId.make("claude-wsl"),
      driver: ProviderDriverKind.make("claudeAgent"),
      updateState: unchangedState,
    } as ProviderUpdateCandidate;
    testState.groups[0] = {
      ...testState.groups[0]!,
      candidates: [firstCandidate, secondCandidate],
      providers: [firstCandidate, secondCandidate],
    };
    testState.updateProvider.mockImplementation(({ input }) =>
      Promise.resolve(
        AsyncResult.success({
          providers: [input.instanceId === "codex-wsl" ? firstCandidate : secondCandidate],
        }),
      ),
    );

    renderRow().props.onUpdate();
    await flushPromises();

    expect(testState.updateProvider).toHaveBeenCalledTimes(2);
    expect(renderRow().props.status.kind).toBe("unchanged");
  });

  it("executes every row trailing-control presentation", () => {
    const row = renderRow();
    const RowComponent = row.type as (props: RowElement["props"]) => ReactElement;
    for (const status of [
      { kind: "idle", text: "Update available" },
      { kind: "loading", text: "Updating" },
      { kind: "success", text: "Updated" },
      { kind: "failed", text: "Failed" },
      { kind: "unchanged", text: "Unchanged" },
    ] as ProviderUpdateRowStatus[]) {
      expect(RowComponent({ ...row.props, status })).toBeTruthy();
    }
  });
});
