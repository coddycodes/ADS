import { EnvironmentId } from "@coddy/contracts";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  stateValues: [] as unknown[],
  stateIndex: 0,
  setters: [] as Array<ReturnType<typeof vi.fn>>,
  menuItems: [] as Array<Record<string, unknown>>,
  markdownProps: [] as Array<Record<string, unknown>>,
  writeFile: vi.fn(),
  copy: vi.fn(),
  isCopied: false,
  download: vi.fn(),
  toastAdd: vi.fn(),
  stackedToast: vi.fn((value: unknown) => value),
  interrupted: false,
  squashFailure: vi.fn(() => new Error("save failed")),
}));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  return {
    ...actual,
    useState: (initial: unknown) => {
      const index = harness.stateIndex++;
      const value = harness.stateValues[index] ?? initial;
      const setter = vi.fn();
      harness.setters[index] = setter;
      return [value, setter];
    },
  };
});
vi.mock("@coddy/client-runtime/state/runtime", () => ({
  isAtomCommandInterrupted: () => harness.interrupted,
  squashAtomCommandFailure: harness.squashFailure,
}));
vi.mock("~/state/use-atom-command", () => ({ useAtomCommand: () => harness.writeFile }));
vi.mock("~/hooks/useCopyToClipboard", () => ({
  useCopyToClipboard: () => ({ copyToClipboard: harness.copy, isCopied: harness.isCopied }),
}));
vi.mock("../proposedPlan", () => ({
  proposedPlanTitle: () => "Proposed title",
  buildProposedPlanMarkdownFilename: () => "proposed-plan.md",
  normalizePlanMarkdownForExport: (value: string) => `normalized:${value}`,
  downloadPlanAsTextFile: harness.download,
  stripDisplayedPlanMarkdown: (value: string) => `displayed:${value}`,
}));
vi.mock("./ui/toast", () => ({
  toastManager: { add: harness.toastAdd },
  stackedThreadToast: harness.stackedToast,
}));
vi.mock("./ChatMarkdown", () => ({
  default: (props: Record<string, unknown>) => {
    harness.markdownProps.push(props);
    return <div data-chat-markdown>{props.text as string}</div>;
  },
}));
vi.mock("./ui/badge", () => ({
  Badge: ({ children }: { children: React.ReactNode }) => <span>{children}</span>,
}));
vi.mock("./ui/button", () => ({
  Button: (props: Record<string, unknown>) => (
    <button type="button">{props.children as React.ReactNode}</button>
  ),
}));
vi.mock("./ui/scroll-area", () => ({
  ScrollArea: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
}));
vi.mock("./ui/menu", () => ({
  Menu: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  MenuTrigger: ({ children, render }: { children: React.ReactNode; render: React.ReactNode }) => (
    <>
      {render}
      {children}
    </>
  ),
  MenuPopup: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  MenuItem: (props: Record<string, unknown>) => {
    harness.menuItems.push(props);
    return <button type="button">{props.children as React.ReactNode}</button>;
  },
}));
vi.mock("~/state/projects", () => ({
  projectEnvironment: { writeFile: { label: "write" } },
}));

import PlanSidebar from "./PlanSidebar";

const environmentId = EnvironmentId.make("env-1");

function renderSidebar(overrides: Partial<React.ComponentProps<typeof PlanSidebar>> = {}) {
  return renderToStaticMarkup(
    <PlanSidebar
      activePlan={null}
      activeProposedPlan={null}
      environmentId={environmentId}
      markdownCwd={undefined}
      workspaceRoot={undefined}
      timestampFormat="locale"
      {...overrides}
    />,
  );
}

function menuItem(label: string) {
  const item = harness.menuItems.find((props) => props.children === label);
  if (!item) throw new Error(`Missing menu item: ${label}`);
  return item;
}

beforeEach(() => {
  harness.stateValues = [];
  harness.stateIndex = 0;
  harness.setters.length = 0;
  harness.menuItems.length = 0;
  harness.markdownProps.length = 0;
  harness.writeFile.mockReset();
  harness.writeFile.mockResolvedValue({
    _tag: "Success",
    value: { relativePath: "proposed-plan.md" },
  });
  harness.copy.mockReset();
  harness.isCopied = false;
  harness.download.mockReset();
  harness.toastAdd.mockReset();
  harness.stackedToast.mockClear();
  harness.interrupted = false;
  harness.squashFailure.mockClear();
  harness.squashFailure.mockReturnValue(new Error("save failed"));
});

describe("PlanSidebar", () => {
  it("renders the empty sidebar and alternate layout", () => {
    const sidebar = renderSidebar();
    expect(sidebar).toContain("Plan");
    expect(sidebar).toContain("No active plan yet.");
    expect(sidebar).toContain("w-[340px]");

    const embedded = renderSidebar({ label: "Execution", mode: "embedded" });
    expect(embedded).toContain("Execution");
    expect(embedded).not.toContain("w-[340px]");
  });

  it("renders explanations and every step status", () => {
    const markup = renderSidebar({
      activePlan: {
        explanation: "Do the work carefully.",
        createdAt: "2026-07-16T00:00:00.000Z",
        turnId: null,
        steps: [
          { step: "Done", status: "completed" },
          { step: "Now", status: "inProgress" },
          { step: "Later", status: "pending" },
        ],
      },
    });
    expect(markup).toContain("Do the work carefully.");
    expect(markup).toContain("Done");
    expect(markup).toContain("Now");
    expect(markup).toContain("Later");
    expect(markup).toContain("line-through");
    expect(markup).toContain("animate-spin");
  });

  it("renders collapsed and expanded proposed plan markdown", () => {
    const proposedPlan = { planMarkdown: "# Plan\nBody" } as never;
    const collapsed = renderSidebar({ activeProposedPlan: proposedPlan });
    expect(collapsed).toContain("Proposed title");
    expect(collapsed).not.toContain("data-chat-markdown");

    harness.stateIndex = 0;
    harness.stateValues = [true, false];
    harness.menuItems.length = 0;
    const expanded = renderSidebar({
      activeProposedPlan: proposedPlan,
      markdownCwd: "/repo",
    });
    expect(expanded).toContain("data-chat-markdown");
    expect(harness.markdownProps[0]).toMatchObject({
      text: "displayed:# Plan\nBody",
      cwd: "/repo",
      isStreaming: false,
    });
  });

  it("copies, downloads, and toggles a proposed plan", () => {
    harness.isCopied = true;
    const proposedPlan = { planMarkdown: "# Plan" } as never;
    const markup = renderSidebar({ activeProposedPlan: proposedPlan, workspaceRoot: "/repo" });
    expect(markup).toContain("Copied!");

    (menuItem("Copied!").onClick as () => void)();
    (menuItem("Download as markdown").onClick as () => void)();
    expect(harness.copy).toHaveBeenCalledWith("# Plan");
    expect(harness.download).toHaveBeenCalledWith("proposed-plan.md", "normalized:# Plan");

    expect(markup).toContain("Proposed title");
  });

  it("saves a plan and reports success", async () => {
    renderSidebar({
      activeProposedPlan: { planMarkdown: "# Save" } as never,
      workspaceRoot: "/repo",
    });
    (menuItem("Save to workspace").onClick as () => void)();

    await vi.waitFor(() => expect(harness.writeFile).toHaveBeenCalledTimes(1));
    expect(harness.writeFile).toHaveBeenCalledWith({
      environmentId,
      input: {
        cwd: "/repo",
        relativePath: "proposed-plan.md",
        contents: "normalized:# Save",
      },
    });
    await vi.waitFor(() =>
      expect(harness.toastAdd).toHaveBeenCalledWith({
        type: "success",
        title: "Plan saved",
        description: "proposed-plan.md",
      }),
    );
    expect(harness.setters[1]).toHaveBeenNthCalledWith(1, true);
    expect(harness.setters[1]).toHaveBeenNthCalledWith(2, false);
  });

  it("reports ordinary save failures and suppresses interruptions", async () => {
    harness.writeFile.mockResolvedValue({ _tag: "Failure" });
    renderSidebar({
      activeProposedPlan: { planMarkdown: "# Fail" } as never,
      workspaceRoot: "/repo",
    });
    (menuItem("Save to workspace").onClick as () => void)();
    await vi.waitFor(() => expect(harness.stackedToast).toHaveBeenCalledTimes(1));
    expect(harness.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not save plan", description: "save failed" }),
    );

    harness.stateIndex = 0;
    harness.stateValues = [];
    harness.menuItems.length = 0;
    harness.toastAdd.mockReset();
    harness.interrupted = true;
    renderSidebar({
      activeProposedPlan: { planMarkdown: "# Interrupted" } as never,
      workspaceRoot: "/repo",
    });
    (menuItem("Save to workspace").onClick as () => void)();
    await vi.waitFor(() => expect(harness.writeFile).toHaveBeenCalledTimes(2));
    await Promise.resolve();
    expect(harness.toastAdd).not.toHaveBeenCalled();
  });

  it("disables workspace saving when no workspace is available", () => {
    renderSidebar({ activeProposedPlan: { planMarkdown: "# No workspace" } as never });
    const save = menuItem("Save to workspace");
    expect(save.disabled).toBe(true);
    (save.onClick as () => void)();
    expect(harness.writeFile).not.toHaveBeenCalled();
  });
});
