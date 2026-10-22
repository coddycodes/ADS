import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { textSizesBelowTextXs } from "../../test/uiTypography";

const harness = vi.hoisted(() => ({
  popovers: 0,
  triggers: [] as Array<Record<string, unknown>>,
  tooltipTriggers: 0,
}));

vi.mock("../ui/popover", () => ({
  Popover: ({ children }: { children: React.ReactNode }) => {
    harness.popovers += 1;
    return <div>{children}</div>;
  },
  PopoverTrigger: (props: Record<string, unknown>) => {
    harness.triggers.push(props);
    return <>{props.render as React.ReactNode}</>;
  },
  PopoverPopup: ({ children }: { children: React.ReactNode }) => <section>{children}</section>,
}));

vi.mock("../ui/tooltip", () => ({
  Tooltip: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  TooltipTrigger: ({ render }: { render: React.ReactNode }) => {
    harness.tooltipTriggers += 1;
    return <>{render}</>;
  },
  TooltipPopup: ({ children }: { children: React.ReactNode }) => <section>{children}</section>,
}));

import { ContextWindowMeter } from "./ContextWindowMeter";

function usage(overrides: Record<string, unknown> = {}) {
  return {
    usedTokens: 500,
    maxTokens: 1_000,
    usedPercentage: 50,
    totalProcessedTokens: null,
    compactsAutomatically: false,
    ...overrides,
  } as never;
}

function render({
  supported = true,
  contextWindowUsage = usage(),
  providerDisplayName,
}: {
  supported?: boolean;
  contextWindowUsage?: ReturnType<typeof usage> | null;
  providerDisplayName?: string | null;
} = {}) {
  return renderToStaticMarkup(
    <ContextWindowMeter
      supported={supported}
      usage={contextWindowUsage}
      {...(providerDisplayName !== undefined ? { providerDisplayName } : {})}
    />,
  );
}

beforeEach(() => {
  harness.popovers = 0;
  harness.triggers.length = 0;
  harness.tooltipTriggers = 0;
});

describe("ContextWindowMeter", () => {
  it.each([
    { state: "awaiting data", contextWindowUsage: null },
    {
      state: "a known maximum, processed total, and automatic compaction",
      contextWindowUsage: usage({ totalProcessedTokens: 12_500, compactsAutomatically: true }),
    },
    {
      state: "an unknown maximum",
      contextWindowUsage: usage({ maxTokens: null, usedPercentage: null }),
    },
  ])("follows UI.md typography in the popover with $state", ({ contextWindowUsage }) => {
    const markup = render({ contextWindowUsage });

    expect.soft(textSizesBelowTextXs(markup)).toEqual([]);
    expect.soft(markup).not.toMatch(/text-muted-foreground\/\d+/);
  });

  it("renders a disabled tooltip-only control when context usage is unavailable", () => {
    const markup = render({ supported: false, contextWindowUsage: null });

    expect(markup).toContain('aria-disabled="true"');
    expect(markup).toContain("Context window usage unavailable");
    expect(markup).not.toContain("Awaiting context usage");
    expect(markup).toContain("Context usage is not available for this provider.");
    expect(harness.popovers).toBe(0);
    expect(harness.triggers).toEqual([]);
    expect(harness.tooltipTriggers).toBe(1);
  });

  it("renders a neutral popover when supported context usage is awaiting data", () => {
    const markup = render({ supported: true, contextWindowUsage: null });

    expect(markup).toContain("Context window usage awaiting data");
    expect(markup).toContain("Awaiting context usage");
    expect(markup).toContain("Usage will appear after the first provider response.");
    expect(markup).not.toContain('role="progressbar"');
    expect(harness.popovers).toBe(1);
    expect(harness.triggers[0]).toMatchObject({ openOnHover: true, delay: 150, closeDelay: 0 });
  });

  it("renders ordinary and low percentage formats", () => {
    expect(render()).toContain("Context window 50% used");
    expect(render({ contextWindowUsage: usage({ usedPercentage: 9 }) })).toContain(
      "Context window 9% used",
    );
    expect(render({ contextWindowUsage: usage({ usedPercentage: 9.25 }) })).toContain(
      "Context window 9.3% used",
    );
    expect(render({ contextWindowUsage: usage({ usedPercentage: 10.6 }) })).toContain(
      "Context window 11% used",
    );
    expect(harness.triggers[0]).toMatchObject({ openOnHover: true, delay: 150, closeDelay: 0 });
  });

  it("falls back to token labels for missing or invalid percentages", () => {
    expect(
      render({ contextWindowUsage: usage({ usedPercentage: null, maxTokens: null }) }),
    ).toContain("Context window 500 tokens used");
    expect(
      render({ contextWindowUsage: usage({ usedPercentage: Number.NaN, maxTokens: 1_000 }) }),
    ).toContain("Context window 500 tokens used");
  });

  it("clamps progress widths and changes color above ninety percent", () => {
    const negative = render({ contextWindowUsage: usage({ usedPercentage: -5 }) });
    expect(negative).toContain("width:0%");
    expect(negative).toContain("var(--color-blue-500)");

    const overloaded = render({ contextWindowUsage: usage({ usedPercentage: 105 }) });
    expect(overloaded).toContain("width:100%");
    expect(overloaded).toContain("var(--color-red-500)");
    expect(overloaded).toContain('aria-valuenow="100"');

    expect(
      render({ contextWindowUsage: usage({ usedPercentage: null, maxTokens: 1_000 }) }),
    ).toContain("width:0%");
  });

  it("shows processed totals only when positive", () => {
    expect(render({ contextWindowUsage: usage({ totalProcessedTokens: 12_500 }) })).toContain(
      "Total processed",
    );
    expect(render({ contextWindowUsage: usage({ totalProcessedTokens: 12_500 }) })).toContain(
      "13k",
    );
    expect(render({ contextWindowUsage: usage({ totalProcessedTokens: 0 }) })).not.toContain(
      "Total processed",
    );
    expect(render({ contextWindowUsage: usage({ totalProcessedTokens: null }) })).not.toContain(
      "Total processed",
    );
  });

  it("renders only measured context fields", () => {
    const markup = render({ contextWindowUsage: usage({ totalProcessedTokens: 12_500 }) });

    expect(markup).not.toContain("Free space");
    expect(markup).not.toContain("MCP tools");
    expect(markup).not.toContain("Memory files");
  });

  it("explains automatic compaction with provider and fallback names", () => {
    expect(
      render({
        contextWindowUsage: usage({ compactsAutomatically: true }),
        providerDisplayName: "Codex",
      }),
    ).toContain("Codex automatically compacts");
    expect(
      render({
        contextWindowUsage: usage({ compactsAutomatically: true }),
        providerDisplayName: null,
      }),
    ).toContain("It automatically compacts");
    expect(
      render({
        contextWindowUsage: usage({ compactsAutomatically: false }),
        providerDisplayName: "Codex",
      }),
    ).not.toContain("automatically compacts");
  });

  it("names the automatic compaction threshold when the provider reports one", () => {
    const markup = render({
      contextWindowUsage: usage({ compactsAutomatically: true, autoCompactTokens: 967_000 }),
      providerDisplayName: "Claude",
    });
    expect(markup).toContain("Claude automatically compacts its context at about 967k tokens.");
    expect(
      render({
        contextWindowUsage: usage({ compactsAutomatically: true }),
        providerDisplayName: "Claude",
      }),
    ).toContain("Claude automatically compacts its context when needed.");
  });

  it("omits progress details when no maximum is known", () => {
    const markup = render({
      contextWindowUsage: usage({ maxTokens: null, usedPercentage: null, usedTokens: 1_250 }),
    });
    expect(markup).toContain("1.3k");
    expect(markup).not.toContain('role="progressbar"');

    const changingUsage = usage();
    let reads = 0;
    Object.defineProperty(changingUsage, "maxTokens", {
      get: () => {
        reads += 1;
        return reads === 3 ? null : 1_000;
      },
    });
    renderToStaticMarkup(<ContextWindowMeter supported usage={changingUsage} />);
  });
});
