import { EnvironmentId } from "@coddy/contracts";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  stateValues: [] as unknown[],
  stateIndex: 0,
  setters: [] as Array<ReturnType<typeof vi.fn>>,
  buttons: [] as Array<Record<string, unknown>>,
  menuItems: [] as Array<Record<string, unknown>>,
  dialogs: [] as Array<Record<string, unknown>>,
  inputs: [] as Array<Record<string, unknown>>,
  markdown: [] as Array<Record<string, unknown>>,
  copyOptions: null as Record<string, unknown> | null,
  copy: vi.fn(),
  isCopied: false,
  download: vi.fn(),
  writeFile: vi.fn(),
  toastAdd: vi.fn(),
  stackedToast: vi.fn((value: unknown) => value),
  interrupted: false,
  squashFailure: vi.fn((): unknown => new Error("save failed")),
  title: "Plan title" as string | null,
  collapsedPreview: "collapsed preview" as string | null,
}));

vi.mock("react", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react")>()),
  useId: () => "save-path-id",
  useState: (initial: unknown) => {
    const index = harness.stateIndex++;
    const value = index < harness.stateValues.length ? harness.stateValues[index] : initial;
    const setter = vi.fn();
    harness.setters[index] = setter;
    return [value, setter];
  },
}));
vi.mock("@coddy/client-runtime/state/runtime", () => ({
  isAtomCommandInterrupted: () => harness.interrupted,
  squashAtomCommandFailure: harness.squashFailure,
}));
vi.mock("../../proposedPlan", () => ({
  buildCollapsedProposedPlanPreviewMarkdown: () => harness.collapsedPreview,
  buildProposedPlanMarkdownFilename: () => "plan.md",
  downloadPlanAsTextFile: harness.download,
  normalizePlanMarkdownForExport: (value: string) => `normalized:${value}`,
  proposedPlanTitle: () => harness.title,
  stripDisplayedPlanMarkdown: (value: string) => `displayed:${value}`,
}));
vi.mock("~/hooks/useCopyToClipboard", () => ({
  useCopyToClipboard: (options: Record<string, unknown>) => {
    harness.copyOptions = options;
    return { copyToClipboard: harness.copy, isCopied: harness.isCopied };
  },
}));
vi.mock("~/state/use-atom-command", () => ({ useAtomCommand: () => harness.writeFile }));
vi.mock("../ChatMarkdown", () => ({
  default: (props: Record<string, unknown>) => {
    harness.markdown.push(props);
    return <div data-markdown>{props.text as string}</div>;
  },
}));
vi.mock("../ui/badge", () => ({
  Badge: ({ children }: { children: React.ReactNode }) => <span>{children}</span>,
}));
vi.mock("../ui/button", () => ({
  Button: (props: Record<string, unknown>) => {
    harness.buttons.push(props);
    return <button type="button">{props.children as React.ReactNode}</button>;
  },
}));
vi.mock("../ui/input", () => ({
  Input: (props: Record<string, unknown>) => {
    harness.inputs.push(props);
    return <input />;
  },
}));
vi.mock("../ui/menu", () => ({
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
vi.mock("../ui/dialog", () => ({
  Dialog: (props: Record<string, unknown>) => {
    harness.dialogs.push(props);
    return <div>{props.children as React.ReactNode}</div>;
  },
  DialogDescription: ({ children }: { children: React.ReactNode }) => <p>{children}</p>,
  DialogFooter: ({ children }: { children: React.ReactNode }) => <footer>{children}</footer>,
  DialogHeader: ({ children }: { children: React.ReactNode }) => <header>{children}</header>,
  DialogPanel: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  DialogPopup: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  DialogTitle: ({ children }: { children: React.ReactNode }) => <h2>{children}</h2>,
}));
vi.mock("../ui/toast", () => ({
  toastManager: { add: harness.toastAdd },
  stackedThreadToast: harness.stackedToast,
}));
vi.mock("~/state/projects", () => ({
  projectEnvironment: { writeFile: { label: "write" } },
}));

import { ProposedPlanCard } from "./ProposedPlanCard";

const environmentId = EnvironmentId.make("environment-1");

function renderCard(
  overrides: Partial<React.ComponentProps<typeof ProposedPlanCard>> = {},
): string {
  return renderToStaticMarkup(
    <ProposedPlanCard
      planMarkdown={"# Plan\nBody"}
      environmentId={environmentId}
      cwd="/repo"
      workspaceRoot="/repo"
      {...overrides}
    />,
  );
}

function invokeClick(props: Record<string, unknown> | undefined): unknown {
  if (typeof props?.onClick !== "function") throw new Error("Missing click handler");
  return props.onClick();
}

function menuItem(label: string): Record<string, unknown> {
  const item = harness.menuItems.find((props) => props.children === label);
  if (!item) throw new Error(`Missing menu item: ${label}`);
  return item;
}

function button(label: string): Record<string, unknown> {
  const item = harness.buttons.find((props) => props.children === label);
  if (!item) throw new Error(`Missing button: ${label}`);
  return item;
}

beforeEach(() => {
  harness.stateValues = [];
  harness.stateIndex = 0;
  harness.setters.length = 0;
  harness.buttons.length = 0;
  harness.menuItems.length = 0;
  harness.dialogs.length = 0;
  harness.inputs.length = 0;
  harness.markdown.length = 0;
  harness.copyOptions = null;
  harness.copy.mockReset();
  harness.isCopied = false;
  harness.download.mockReset();
  harness.writeFile.mockReset();
  harness.writeFile.mockResolvedValue({
    _tag: "Success",
    value: { relativePath: "plan.md" },
  });
  harness.toastAdd.mockReset();
  harness.stackedToast.mockClear();
  harness.interrupted = false;
  harness.squashFailure.mockReset();
  harness.squashFailure.mockReturnValue(new Error("save failed"));
  harness.title = "Plan title";
  harness.collapsedPreview = "collapsed preview";
});

describe("ProposedPlanCard", () => {
  it("renders and exports a short plan", () => {
    harness.title = null;
    harness.isCopied = true;
    const markup = renderCard();
    expect(markup).toContain("Proposed plan");
    expect(markup).toContain("displayed:# Plan\nBody");
    expect(markup).toContain("Copied!");
    expect(markup).not.toContain("Expand plan");
    expect(harness.markdown[0]).toMatchObject({ cwd: "/repo", isStreaming: false });

    invokeClick(menuItem("Copied!"));
    invokeClick(menuItem("Download as markdown"));
    expect(harness.copy).toHaveBeenCalledWith("normalized:# Plan\nBody");
    expect(harness.download).toHaveBeenCalledWith("plan.md", "normalized:# Plan\nBody");

    const onError = harness.copyOptions?.onError;
    if (typeof onError !== "function") throw new Error("Missing copy error handler");
    onError(new Error("copy failed"));
    onError("unknown");
    expect(harness.toastAdd).toHaveBeenNthCalledWith(
      1,
      expect.objectContaining({ title: "Could not copy plan", description: "copy failed" }),
    );
    expect(harness.toastAdd).toHaveBeenNthCalledWith(
      2,
      expect.objectContaining({ description: "An error occurred while copying." }),
    );
  });

  it("collapses and expands long plans", () => {
    const longPlan = `# Long\n${"x".repeat(901)}`;
    const collapsed = renderCard({ planMarkdown: longPlan });
    expect(collapsed).toContain("collapsed preview");
    expect(collapsed).toContain("Expand plan");
    const toggle = invokeClick(button("Expand plan"));
    expect(toggle).toBeUndefined();
    const updateExpanded = harness.setters[0]?.mock.calls[0]?.[0] as
      | ((value: boolean) => boolean)
      | undefined;
    expect(updateExpanded?.(false)).toBe(true);
    expect(updateExpanded?.(true)).toBe(false);

    harness.stateIndex = 0;
    harness.stateValues = [true];
    harness.buttons.length = 0;
    harness.markdown.length = 0;
    const expanded = renderCard({ planMarkdown: longPlan });
    expect(expanded).toContain(`displayed:${longPlan}`);
    expect(expanded).toContain("Collapse plan");

    harness.stateIndex = 0;
    harness.stateValues = [];
    harness.buttons.length = 0;
    harness.markdown.length = 0;
    const manyLines = Array.from({ length: 21 }, (_, index) => `line ${index}`).join("\n");
    expect(renderCard({ planMarkdown: manyLines })).toContain("collapsed preview");

    harness.stateIndex = 0;
    harness.markdown.length = 0;
    harness.collapsedPreview = null;
    renderCard({ planMarkdown: longPlan });
    expect(harness.markdown[0]?.text).toBe("");
  });

  it("opens the save dialog, preserves an existing path, and supports its controls", () => {
    renderCard();
    invokeClick(menuItem("Save to workspace"));
    const initializePath = harness.setters[2]?.mock.calls[0]?.[0] as
      | ((value: string) => string)
      | undefined;
    expect(initializePath?.("")).toBe("plan.md");
    expect(initializePath?.("notes/plan.md")).toBe("notes/plan.md");
    expect(harness.setters[1]).toHaveBeenCalledWith(true);

    const dialogChange = harness.dialogs[0]?.onOpenChange;
    if (typeof dialogChange !== "function") throw new Error("Missing dialog handler");
    dialogChange(false);
    expect(harness.setters[1]).toHaveBeenLastCalledWith(false);

    const inputChange = harness.inputs[0]?.onChange;
    if (typeof inputChange !== "function") throw new Error("Missing input handler");
    inputChange({ target: { value: "notes/plan.md" } });
    expect(harness.setters[2]).toHaveBeenCalledWith("notes/plan.md");
    invokeClick(button("Cancel"));
    expect(harness.setters[1]).toHaveBeenLastCalledWith(false);

    harness.stateIndex = 0;
    harness.stateValues = [false, true, "plan.md", true];
    harness.buttons.length = 0;
    harness.dialogs.length = 0;
    renderCard();
    const savingDialogChange = harness.dialogs[0]?.onOpenChange;
    if (typeof savingDialogChange !== "function") throw new Error("Missing dialog handler");
    harness.setters[1]?.mockClear();
    savingDialogChange(false);
    expect(harness.setters[1]).not.toHaveBeenCalled();
    expect(button("Saving...").disabled).toBe(true);
  });

  it("reports unavailable workspaces and blank save paths", () => {
    renderCard({ workspaceRoot: undefined });
    expect(menuItem("Save to workspace").disabled).toBe(true);
    invokeClick(menuItem("Save to workspace"));
    expect(harness.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Workspace path is unavailable" }),
    );

    harness.stateIndex = 0;
    harness.stateValues = [false, true, "   ", false];
    harness.buttons.length = 0;
    harness.menuItems.length = 0;
    harness.toastAdd.mockReset();
    renderCard();
    invokeClick(button("Save"));
    expect(harness.toastAdd).toHaveBeenCalledWith({
      type: "warning",
      title: "Enter a workspace path",
    });
    expect(harness.writeFile).not.toHaveBeenCalled();

    harness.stateIndex = 0;
    harness.stateValues = [false, true, "plan.md", false];
    harness.buttons.length = 0;
    harness.toastAdd.mockReset();
    renderCard({ workspaceRoot: undefined });
    invokeClick(button("Save"));
    expect(harness.toastAdd).not.toHaveBeenCalled();
    expect(harness.writeFile).not.toHaveBeenCalled();
  });

  it("saves a plan to a trimmed workspace path", async () => {
    harness.stateValues = [false, true, " notes/plan.md ", false];
    renderCard();
    invokeClick(button("Save"));
    await vi.waitFor(() => expect(harness.writeFile).toHaveBeenCalledTimes(1));
    expect(harness.writeFile).toHaveBeenCalledWith({
      environmentId,
      input: {
        cwd: "/repo",
        relativePath: "notes/plan.md",
        contents: "normalized:# Plan\nBody",
      },
    });
    await vi.waitFor(() =>
      expect(harness.toastAdd).toHaveBeenCalledWith({
        type: "success",
        title: "Plan saved to workspace",
        description: "plan.md",
      }),
    );
    expect(harness.setters[3]).toHaveBeenNthCalledWith(1, true);
    expect(harness.setters[3]).toHaveBeenNthCalledWith(2, false);
    expect(harness.setters[1]).toHaveBeenCalledWith(false);
  });

  it("reports ordinary save failures and suppresses interruptions", async () => {
    harness.stateValues = [false, true, "plan.md", false];
    harness.writeFile.mockResolvedValue({ _tag: "Failure" });
    renderCard();
    invokeClick(button("Save"));
    await vi.waitFor(() => expect(harness.stackedToast).toHaveBeenCalledTimes(1));
    expect(harness.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not save plan", description: "save failed" }),
    );

    harness.stateIndex = 0;
    harness.buttons.length = 0;
    harness.toastAdd.mockReset();
    harness.stackedToast.mockClear();
    harness.squashFailure.mockReturnValue("unknown");
    renderCard();
    invokeClick(button("Save"));
    await vi.waitFor(() => expect(harness.stackedToast).toHaveBeenCalledTimes(1));
    expect(harness.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ description: "An error occurred while saving." }),
    );

    harness.stateIndex = 0;
    harness.buttons.length = 0;
    harness.toastAdd.mockReset();
    harness.stackedToast.mockClear();
    harness.interrupted = true;
    renderCard();
    invokeClick(button("Save"));
    await vi.waitFor(() => expect(harness.writeFile).toHaveBeenCalledTimes(3));
    await Promise.resolve();
    expect(harness.toastAdd).not.toHaveBeenCalled();
  });
});
