// @vitest-environment happy-dom

import type {
  ConsumeCodexRateLimitResetResult,
  ServerProviderUsageResetError,
} from "@coddy/contracts";
import type { AtomCommandResult } from "@coddy/client-runtime/state/runtime";
import * as Cause from "effect/Cause";
import * as DateTime from "effect/DateTime";
import { AsyncResult } from "effect/reactivity";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import type { ProviderUsageViewModel, UsageWindowViewModel } from "./providerUsagePresentation";

vi.mock("../ui/alert-dialog", async () => {
  const React = await vi.importActual<typeof import("react")>("react");
  const OpenContext = React.createContext<((open: boolean) => void) | undefined>(undefined);
  const passthrough = ({ children }: { children?: React.ReactNode }) => <>{children}</>;

  return {
    AlertDialog: ({
      children,
      open,
      onOpenChange,
    }: {
      children: React.ReactNode;
      open: boolean;
      onOpenChange: (open: boolean) => void;
    }) => (
      <OpenContext.Provider value={onOpenChange}>{open ? children : null}</OpenContext.Provider>
    ),
    AlertDialogPopup: ({ children }: { children: React.ReactNode }) => (
      <div role="alertdialog">{children}</div>
    ),
    AlertDialogHeader: passthrough,
    AlertDialogFooter: passthrough,
    AlertDialogTitle: ({ children }: { children: React.ReactNode }) => <h2>{children}</h2>,
    AlertDialogDescription: ({ children }: { children: React.ReactNode }) => <p>{children}</p>,
    AlertDialogClose: ({
      children,
      render,
      ...props
    }: {
      children: React.ReactNode;
      render?: React.ReactElement<React.ComponentProps<"button">>;
    } & React.ComponentProps<"button">) => {
      const onOpenChange = React.useContext(OpenContext);
      return React.cloneElement(render ?? <button type="button" />, {
        ...props,
        children,
        onClick: (event: React.MouseEvent<HTMLButtonElement>) => {
          render?.props.onClick?.(event);
          props.onClick?.(event);
          onOpenChange?.(false);
        },
      });
    },
  };
});

import { ProviderUsageDetail } from "./ProviderUsageDetail";

const mounted: Array<{ root: Root; container: HTMLDivElement }> = [];
const updatedAt = DateTime.makeUnsafe("2026-07-22T20:00:00.000Z");

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

afterEach(async () => {
  vi.restoreAllMocks();
  for (const entry of mounted.splice(0)) {
    await act(async () => entry.root.unmount());
    entry.container.remove();
  }
});

function usageWindow(overrides: Partial<UsageWindowViewModel> = {}): UsageWindowViewModel {
  return {
    key: "session",
    label: "Session",
    windowLabel: "5h",
    consumedPercent: 40,
    displayedPercent: 60,
    fillPercent: 60,
    percentageLabel: "60% remaining",
    resetLabel: "Resets in 5m",
    resetsAt: null,
    resetDescription: null,
    barColorClass: "bg-emerald-500",
    ...overrides,
  };
}

function provider(overrides: Partial<ProviderUsageViewModel> = {}): ProviderUsageViewModel {
  const session = usageWindow();
  return {
    provider: "codex",
    status: "ok",
    windows: [session],
    detailedWindows: [session],
    compactWindows: [session],
    plan: null,
    credits: null,
    updatedAt,
    error: null,
    ...overrides,
  };
}

function success(
  outcome: ConsumeCodexRateLimitResetResult["outcome"],
): AtomCommandResult<ConsumeCodexRateLimitResetResult, ServerProviderUsageResetError> {
  return AsyncResult.success({
    outcome,
    usage: { readAt: updatedAt, isFetching: false, providers: [] },
  });
}

function findButton(container: HTMLElement, name: string): HTMLButtonElement | undefined {
  return Array.from(container.querySelectorAll<HTMLButtonElement>("button")).find(
    (button) => button.textContent === name,
  );
}

async function mount(
  props: React.ComponentProps<typeof ProviderUsageDetail>,
): Promise<HTMLDivElement> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  mounted.push({ root, container });
  await act(async () => root.render(<ProviderUsageDetail {...props} />));
  return container;
}

function detailProps(overrides: Partial<React.ComponentProps<typeof ProviderUsageDetail>> = {}) {
  return {
    viewModel: provider(),
    onOpenProviderSettings: vi.fn(),
    onConsumeCodexRateLimitReset: vi.fn(async () => success("reset")),
    ...overrides,
  };
}

describe("ProviderUsageDetail", () => {
  it("renders every Claude window, updated time, stale error, and Provider settings action", async () => {
    const onOpenProviderSettings = vi.fn();
    const session = usageWindow();
    const weekly = usageWindow({ key: "weekly", label: "Weekly", windowLabel: "7d" });
    const fable = usageWindow({ key: "fable", label: "Fable", windowLabel: "7d" });
    const container = await mount(
      detailProps({
        viewModel: provider({
          provider: "claude",
          status: "error",
          windows: [session, weekly, fable],
          detailedWindows: [session, weekly, fable],
          error: "Could not refresh Claude usage.",
        }),
        onOpenProviderSettings,
      }),
    );

    expect(container.textContent).toContain("Claude usage");
    expect(container.textContent).toContain("Session");
    expect(container.textContent).toContain("Weekly");
    expect(container.textContent).toContain("Fable");
    expect(
      container.querySelector(`time[datetime="${DateTime.formatIso(updatedAt)}"]`),
    ).toBeTruthy();
    expect(container.querySelector('[role="status"]')?.textContent).toContain(
      "Showing last available usage. Could not refresh Claude usage.",
    );

    await act(async () => findButton(container, "Provider settings")?.click());
    expect(onOpenProviderSettings).toHaveBeenCalledOnce();
  });

  it("renders Codex plan, windows, plural credit count, and expiry", async () => {
    const session = usageWindow();
    const weekly = usageWindow({ key: "weekly", label: "Weekly", windowLabel: "7d" });
    const container = await mount(
      detailProps({
        viewModel: provider({
          windows: [session, weekly],
          detailedWindows: [session, weekly],
          plan: { value: "team", label: "Team" },
          credits: {
            availableCount: 2,
            totalEarnedCount: 5,
            nextExpiresAt: DateTime.makeUnsafe("2026-07-24T20:00:00.000Z"),
            nextExpiresLabel: "Expires in 2d",
          },
        }),
      }),
    );

    expect(container.textContent).toContain("Codex usage");
    expect(container.textContent).toContain("Team plan");
    expect(container.textContent).toContain("Session");
    expect(container.textContent).toContain("Weekly");
    expect(container.textContent).toContain("2 reset credits");
    expect(container.textContent).toContain("Expires in 2d");
    expect(findButton(container, "Reset now")).toBeTruthy();
  });

  it("uses singular credit grammar and hides Reset now when credits are empty or unsupported", async () => {
    const singular = await mount(
      detailProps({
        viewModel: provider({
          credits: {
            availableCount: 1,
            totalEarnedCount: null,
            nextExpiresAt: null,
            nextExpiresLabel: null,
          },
        }),
      }),
    );
    expect(singular.textContent).toContain("1 reset credit");

    const empty = await mount(
      detailProps({
        viewModel: provider({
          credits: {
            availableCount: 0,
            totalEarnedCount: null,
            nextExpiresAt: null,
            nextExpiresLabel: null,
          },
        }),
      }),
    );
    expect(empty.textContent).toContain("0 reset credits");
    expect(findButton(empty, "Reset now")).toBeUndefined();

    const unsupported = await mount(detailProps());
    expect(unsupported.textContent).not.toContain("reset credit");
    expect(findButton(unsupported, "Reset now")).toBeUndefined();
  });

  it("confirms before consuming one credit and holds one UUID for the pending attempt", async () => {
    let resolveReset!: (
      result: AtomCommandResult<ConsumeCodexRateLimitResetResult, ServerProviderUsageResetError>,
    ) => void;
    const onConsumeCodexRateLimitReset = vi.fn(
      () =>
        new Promise<
          AtomCommandResult<ConsumeCodexRateLimitResetResult, ServerProviderUsageResetError>
        >((resolve) => {
          resolveReset = resolve;
        }),
    );
    const randomUUID = vi
      .spyOn(globalThis.crypto, "randomUUID")
      .mockReturnValue("019f8aad-253c-77b0-9f6f-77b45f149efb");
    const container = await mount(
      detailProps({
        viewModel: provider({
          credits: {
            availableCount: 2,
            totalEarnedCount: 2,
            nextExpiresAt: null,
            nextExpiresLabel: null,
          },
        }),
        onConsumeCodexRateLimitReset,
      }),
    );

    await act(async () => findButton(container, "Reset now")?.click());
    expect(container.querySelector('[role="alertdialog"]')?.textContent).toContain(
      "One reset credit will be consumed, and eligible windows will reset immediately.",
    );
    await act(async () => findButton(container, "Cancel")?.click());
    expect(randomUUID).not.toHaveBeenCalled();
    expect(onConsumeCodexRateLimitReset).not.toHaveBeenCalled();

    await act(async () => findButton(container, "Reset now")?.click());
    await act(async () => findButton(container, "Confirm reset")?.click());

    expect(randomUUID).toHaveBeenCalledOnce();
    expect(onConsumeCodexRateLimitReset).toHaveBeenCalledOnce();
    expect(onConsumeCodexRateLimitReset).toHaveBeenCalledWith(
      "019f8aad-253c-77b0-9f6f-77b45f149efb",
    );
    expect(findButton(container, "Resetting…")?.disabled).toBe(true);
    expect(findButton(container, "Resetting…")?.getAttribute("aria-busy")).toBe("true");
    expect(container.textContent).toContain("2 reset credits");

    await act(async () => resolveReset(success("reset")));
    expect(container.querySelector('[role="status"]')?.textContent).toContain(
      "Rate-limit reset completed.",
    );
    expect(findButton(container, "Reset now")?.disabled).toBe(false);
  });

  it.each([
    ["reset", "Rate-limit reset completed."],
    ["nothingToReset", "No eligible windows currently need a reset."],
    ["noCredit", "No reset credit is available."],
    ["alreadyRedeemed", "This reset request was already redeemed."],
  ] as const)("surfaces the %s outcome", async (outcome, expectedCopy) => {
    vi.spyOn(globalThis.crypto, "randomUUID").mockReturnValue(
      "019f8aad-253c-77b0-9f6f-77b45f149efb",
    );
    const container = await mount(
      detailProps({
        viewModel: provider({
          credits: {
            availableCount: 1,
            totalEarnedCount: null,
            nextExpiresAt: null,
            nextExpiresLabel: null,
          },
        }),
        onConsumeCodexRateLimitReset: vi.fn(async () => success(outcome)),
      }),
    );

    await act(async () => findButton(container, "Reset now")?.click());
    await act(async () => findButton(container, "Confirm reset")?.click());

    expect(container.querySelector('[role="status"]')?.textContent).toContain(expectedCopy);
  });

  it("surfaces a typed failure without changing credits and leaves Reset now retryable", async () => {
    vi.spyOn(globalThis.crypto, "randomUUID")
      .mockReturnValueOnce("019f8aad-253c-77b0-9f6f-77b45f149efb")
      .mockReturnValueOnce("019f8aad-253c-77b0-9f6f-77b45f149efc");
    const resetError = new (await import("@coddy/contracts")).ServerProviderUsageResetError({
      message: "Codex reset service is unavailable.",
    });
    const onConsumeCodexRateLimitReset = vi
      .fn<
        (
          requestId: string,
        ) => Promise<
          AtomCommandResult<ConsumeCodexRateLimitResetResult, ServerProviderUsageResetError>
        >
      >()
      .mockResolvedValueOnce(AsyncResult.failure(Cause.fail(resetError)))
      .mockResolvedValueOnce(success("reset"));
    const container = await mount(
      detailProps({
        viewModel: provider({
          credits: {
            availableCount: 1,
            totalEarnedCount: null,
            nextExpiresAt: null,
            nextExpiresLabel: null,
          },
        }),
        onConsumeCodexRateLimitReset,
      }),
    );

    await act(async () => findButton(container, "Reset now")?.click());
    await act(async () => findButton(container, "Confirm reset")?.click());

    expect(container.querySelector('[role="status"]')?.textContent).toContain(
      "Codex reset service is unavailable. Try again.",
    );
    expect(container.textContent).toContain("1 reset credit");
    expect(findButton(container, "Reset now")?.disabled).toBe(false);

    await act(async () => findButton(container, "Reset now")?.click());
    await act(async () => findButton(container, "Confirm reset")?.click());
    expect(onConsumeCodexRateLimitReset).toHaveBeenCalledTimes(2);
  });

  it("disables Reset now while externally pending", async () => {
    const container = await mount(
      detailProps({
        isResetting: true,
        viewModel: provider({
          credits: {
            availableCount: 1,
            totalEarnedCount: null,
            nextExpiresAt: null,
            nextExpiresLabel: null,
          },
        }),
      }),
    );

    expect(findButton(container, "Resetting…")?.disabled).toBe(true);
  });
});
