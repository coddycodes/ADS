// @vitest-environment happy-dom

import type { EnvironmentId } from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import * as AsyncResult from "effect/reactivity/AsyncResult";
import { act, StrictMode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({ refreshStatus: vi.fn() }));

vi.mock("../../state/vcs", () => ({
  vcsEnvironment: { refreshStatus: "cmd:refresh-status" },
}));
vi.mock("../../state/use-atom-command", () => ({
  useAtomCommand: () => h.refreshStatus,
}));

import { useCheckoutStatusRereads } from "./useCheckoutStatusRereads";

const CHECKOUT_A = { environmentId: "environment-1" as EnvironmentId, cwd: "/repo/main" };
const CHECKOUT_B = { environmentId: "environment-1" as EnvironmentId, cwd: "/repo/other" };
const OTHER_ENVIRONMENT = { environmentId: "environment-2" as EnvironmentId, cwd: "/repo/main" };
const SUCCESS = AsyncResult.success({});
const FAILURE = AsyncResult.failure(Cause.fail(new Error("Git could not read the status.")));
const INTERRUPTED = AsyncResult.failure(Cause.interrupt());

function Probe({ scope }: { scope: typeof CHECKOUT_A }) {
  const { retrying, onRetry } = useCheckoutStatusRereads(scope);
  // Keep the probe clickable while busy to exercise overlapping callers of this hook.
  return <button onClick={onRetry}>{retrying ? "Retrying…" : "Retry"}</button>;
}

let container: HTMLDivElement;
let root: Root | null;

function deferredRead() {
  let resolve!: (result: unknown) => void;
  const promise = new Promise<unknown>((settle) => {
    resolve = settle;
  });
  return { promise, resolve };
}

async function renderProbe(scope = CHECKOUT_A) {
  await act(async () =>
    root?.render(
      <StrictMode>
        <Probe scope={scope} />
      </StrictMode>,
    ),
  );
}

function retryButton(): HTMLButtonElement {
  const button = container.querySelector("button");
  if (button === null) throw new Error("Missing Retry button");
  return button;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.refreshStatus.mockReset();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root?.unmount());
  root = null;
  container.remove();
  vi.restoreAllMocks();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("useCheckoutStatusRereads", () => {
  it.each([
    ["cwd", CHECKOUT_B],
    ["environment", OTHER_ENVIRONMENT],
  ] as const)("tracks concurrent reads independently by %s", async (_dimension, checkoutB) => {
    const readA = deferredRead();
    const readB = deferredRead();
    h.refreshStatus.mockReturnValueOnce(readA.promise).mockReturnValueOnce(readB.promise);

    await renderProbe();
    expect(retryButton().textContent).toBe("Retry");
    await act(async () => retryButton().click());
    expect(retryButton().textContent).toBe("Retrying…");

    await renderProbe(checkoutB);
    expect(retryButton().textContent).toBe("Retry");
    await act(async () => retryButton().click());
    expect(retryButton().textContent).toBe("Retrying…");
    expect(h.refreshStatus).toHaveBeenNthCalledWith(1, {
      environmentId: CHECKOUT_A.environmentId,
      input: { cwd: CHECKOUT_A.cwd },
    });
    expect(h.refreshStatus).toHaveBeenNthCalledWith(2, {
      environmentId: checkoutB.environmentId,
      input: { cwd: checkoutB.cwd },
    });

    await renderProbe();
    expect(retryButton().textContent).toBe("Retrying…");
    await act(async () => readB.resolve(SUCCESS));
    expect(retryButton().textContent).toBe("Retrying…");

    await renderProbe(checkoutB);
    expect(retryButton().textContent).toBe("Retry");
    await renderProbe();
    expect(retryButton().textContent).toBe("Retrying…");
    await act(async () => readA.resolve(SUCCESS));
    expect(retryButton().textContent).toBe("Retry");
  });

  it.each([0, 1] as const)(
    "stays busy until both reads of one checkout settle (read %s settles first)",
    async (first) => {
      const reads = [deferredRead(), deferredRead()];
      h.refreshStatus.mockReturnValueOnce(reads[0]!.promise).mockReturnValueOnce(reads[1]!.promise);
      await renderProbe();

      await act(async () => {
        retryButton().click();
        retryButton().click();
      });
      expect(h.refreshStatus).toHaveBeenCalledTimes(2);
      expect(retryButton().textContent).toBe("Retrying…");

      await act(async () => reads[first]!.resolve(SUCCESS));
      expect(retryButton().textContent).toBe("Retrying…");
      await act(async () => reads[1 - first]!.resolve(SUCCESS));
      expect(retryButton().textContent).toBe("Retry");
    },
  );

  it.each([
    ["failed", FAILURE],
    ["interrupted", INTERRUPTED],
  ] as const)("settles a %s read and allows another retry", async (_outcome, result) => {
    const read = deferredRead();
    const nextRead = deferredRead();
    h.refreshStatus.mockReturnValueOnce(read.promise).mockReturnValueOnce(nextRead.promise);
    await renderProbe();

    await act(async () => retryButton().click());
    expect(retryButton().textContent).toBe("Retrying…");
    await act(async () => read.resolve(result));
    expect(retryButton().textContent).toBe("Retry");

    await act(async () => retryButton().click());
    expect(retryButton().textContent).toBe("Retrying…");
    await act(async () => nextRead.resolve(SUCCESS));
    expect(retryButton().textContent).toBe("Retry");
  });

  it.each([
    ["successful", SUCCESS],
    ["failed", FAILURE],
    ["interrupted", INTERRUPTED],
  ] as const)(
    "safely settles a %s read after unmount without affecting a new mount",
    async (_outcome, result) => {
      const oldRead = deferredRead();
      const newRead = deferredRead();
      h.refreshStatus.mockReturnValueOnce(oldRead.promise).mockReturnValueOnce(newRead.promise);
      await renderProbe();
      await act(async () => retryButton().click());
      expect(retryButton().textContent).toBe("Retrying…");
      await act(async () => root?.unmount());
      root = null;

      const error = vi.spyOn(console, "error");
      const warn = vi.spyOn(console, "warn");
      root = createRoot(container);
      await renderProbe();
      expect(retryButton().textContent).toBe("Retry");
      await act(async () => retryButton().click());
      expect(retryButton().textContent).toBe("Retrying…");

      await act(async () => oldRead.resolve(result));
      expect(retryButton().textContent).toBe("Retrying…");
      await act(async () => newRead.resolve(SUCCESS));
      expect(retryButton().textContent).toBe("Retry");
      expect(error).not.toHaveBeenCalled();
      expect(warn).not.toHaveBeenCalled();
    },
  );
});
