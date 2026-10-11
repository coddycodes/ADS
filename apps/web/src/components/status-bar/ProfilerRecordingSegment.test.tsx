// @vitest-environment happy-dom

import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

type AnyProps = Record<string, unknown>;

interface FakeEnvironment {
  readonly environmentId: string;
  readonly label: string;
  readonly connection: { readonly phase: string };
  readonly serverConfig: {
    readonly environment: { readonly capabilities: { readonly profiler: boolean } };
    readonly settings: { readonly enableProfiler: boolean };
  } | null;
}

const h = vi.hoisted(() => ({
  environments: [] as unknown[],
  navigate: vi.fn(),
}));

vi.mock("../../state/environments", () => ({
  useEnvironments: () => ({ environments: h.environments }),
}));

vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => h.navigate,
}));

vi.mock("../ui/tooltip", () => ({
  Tooltip: (props: AnyProps) => <>{props.children as ReactNode}</>,
  TooltipTrigger: (props: AnyProps) => {
    const render = props.render as { props: AnyProps } | undefined;
    return (
      <button type="button" {...(render?.props ?? {})}>
        {props.children as ReactNode}
      </button>
    );
  },
  TooltipPopup: (props: AnyProps) => <div data-tooltip-popup>{props.children as ReactNode}</div>,
}));

import { ProfilerRecordingSegment } from "./ProfilerRecordingSegment";

const mounted: Array<{ root: Root; container: HTMLDivElement }> = [];

function environment(
  label: string,
  options: { enableProfiler?: boolean; phase?: string; config?: boolean; profiler?: boolean } = {},
): FakeEnvironment {
  const { enableProfiler = false, phase = "connected", config = true, profiler = true } = options;
  return {
    environmentId: `env-${label}`,
    label,
    connection: { phase },
    serverConfig: config
      ? { environment: { capabilities: { profiler } }, settings: { enableProfiler } }
      : null,
  };
}

async function render(): Promise<HTMLDivElement> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  mounted.push({ root, container });
  await act(async () => {
    root.render(<ProfilerRecordingSegment />);
  });
  return container;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.environments = [];
  h.navigate.mockReset();
});

afterEach(async () => {
  for (const entry of mounted.splice(0)) {
    await act(async () => entry.root.unmount());
    entry.container.remove();
  }
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("ProfilerRecordingSegment", () => {
  it("renders nothing when no environment records", async () => {
    h.environments = [environment("Local"), environment("Luna")];

    const container = await render();

    expect(container.textContent).toBe("");
  });

  it("ignores environments that are not connected or have no settings yet", async () => {
    h.environments = [
      environment("Offline", { enableProfiler: true, phase: "offline" }),
      environment("Loading", { enableProfiler: true, config: false }),
    ];

    const container = await render();

    expect(container.textContent).toBe("");
  });

  it("ignores an older server that echoes back a stored enableProfiler", async () => {
    h.environments = [environment("Old", { enableProfiler: true, profiler: false })];

    const container = await render();

    expect(container.textContent).toBe("");
  });

  it("shows the segment and lists every recording environment", async () => {
    h.environments = [
      environment("Local", { enableProfiler: true }),
      environment("Luna"),
      environment("Build box", { enableProfiler: true }),
    ];

    const container = await render();

    const trigger = container.querySelector("button")!;
    expect(trigger.textContent).toBe("Profiler");
    expect(trigger.getAttribute("aria-label")).toBe(
      "Profiler recording, open diagnostics settings",
    );
    const popup = container.querySelector("[data-tooltip-popup]")!;
    expect(popup.textContent).toContain("Profiler on");
    expect([...popup.querySelectorAll("li")].map((item) => item.textContent)).toEqual([
      "Local",
      "Build box",
    ]);
  });

  it("opens Diagnostics with the first recording environment selected", async () => {
    h.environments = [
      environment("Local"),
      environment("Luna", { enableProfiler: true }),
      environment("Build box", { enableProfiler: true }),
    ];
    const container = await render();

    await act(async () => {
      container.querySelector("button")!.click();
    });

    expect(h.navigate).toHaveBeenCalledWith({
      to: "/settings/diagnostics",
      search: { environment: "env-Luna" },
    });
  });

  it("marks recording with a neutral dot, not an error color", async () => {
    h.environments = [environment("Local", { enableProfiler: true })];

    const container = await render();

    const dot = container.querySelector("button span")!;
    expect(dot.className).not.toContain("destructive");
  });
});
