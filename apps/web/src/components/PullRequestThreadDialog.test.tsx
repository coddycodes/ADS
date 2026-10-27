// @vitest-environment happy-dom

import { EnvironmentId } from "@coddy/contracts";
import { act, type ReactElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterAll, afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const testState = vi.hoisted(() => ({
  cachedPullRequest: null as Record<string, unknown> | null,
  resolution: {
    data: undefined as { pullRequest: Record<string, unknown> | null } | undefined,
    error: null as string | null,
    isPending: false,
    isFetching: false,
  },
  action: {
    run: vi.fn(),
    resetError: vi.fn(),
    error: null as unknown,
    isPending: false,
  },
  interrupted: false,
  sourceControlProvider: "github" as string | undefined,
}));

vi.mock("@coddy/client-runtime/state/runtime", () => ({
  isAtomCommandInterrupted: () => testState.interrupted,
}));

vi.mock("~/lib/sourceControlActions", () => ({
  readCachedPullRequestResolution: () =>
    testState.cachedPullRequest ? { pullRequest: testState.cachedPullRequest } : null,
  usePullRequestResolution: () => testState.resolution,
  usePreparePullRequestThreadAction: () => testState.action,
}));

vi.mock("~/state/query", () => ({
  useEnvironmentQuery: (descriptor: unknown) =>
    descriptor
      ? { data: { sourceControlProvider: testState.sourceControlProvider } }
      : { data: undefined },
}));

vi.mock("~/state/vcs", () => ({
  vcsEnvironment: {
    status: (args: unknown) => ({ kind: "status", args }),
  },
}));

import { PullRequestThreadDialog } from "./PullRequestThreadDialog";

interface MountedTree {
  readonly container: HTMLDivElement;
  readonly root: Root;
}

const mountedTrees: MountedTree[] = [];
const suiteGetAnimationsDescriptor = Object.getOwnPropertyDescriptor(
  Element.prototype,
  "getAnimations",
);
let originalGetAnimationsDescriptor: PropertyDescriptor | undefined;

function pullRequest(state: "open" | "closed" | "merged" | "draft" = "open") {
  return {
    number: 42,
    title: "Improve diagnostics",
    headBranch: "feature/diagnostics",
    baseBranch: "main",
    state,
  };
}

function renderDialog(
  overrides: Partial<React.ComponentProps<typeof PullRequestThreadDialog>> = {},
): ReactElement {
  return (
    <PullRequestThreadDialog
      open
      environmentId={EnvironmentId.make("local-test")}
      cwd="X:\\coddy-studio"
      initialReference="#42"
      onOpenChange={vi.fn()}
      onPrepared={vi.fn()}
      {...overrides}
    />
  );
}

async function mount(element: ReactElement): Promise<MountedTree> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const mounted = { container, root };
  mountedTrees.push(mounted);
  await act(async () => {
    root.render(element);
    await Promise.resolve();
  });
  return mounted;
}

async function click(element: HTMLElement): Promise<void> {
  await act(async () => {
    element.click();
    await Promise.resolve();
  });
}

async function changeInput(input: HTMLInputElement, value: string): Promise<void> {
  await act(async () => {
    const valueSetter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
    valueSetter?.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

async function pressEnter(input: HTMLInputElement): Promise<void> {
  await act(async () => {
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await Promise.resolve();
  });
}

function buttonWithText(text: string): HTMLButtonElement {
  const button = Array.from(document.querySelectorAll<HTMLButtonElement>("button")).find(
    (candidate) => candidate.textContent?.trim() === text,
  );
  expect(button).toBeDefined();
  return button!;
}

function referenceInput(): HTMLInputElement {
  const input = document.querySelector<HTMLInputElement>(
    'input[placeholder*="URL, checkout command"]',
  );
  expect(input).not.toBeNull();
  return input!;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  originalGetAnimationsDescriptor = Object.getOwnPropertyDescriptor(
    Element.prototype,
    "getAnimations",
  );
  Object.defineProperty(Element.prototype, "getAnimations", {
    configurable: true,
    value: () => [],
  });
  testState.cachedPullRequest = pullRequest();
  testState.resolution = {
    data: { pullRequest: pullRequest() },
    error: null,
    isPending: false,
    isFetching: false,
  };
  testState.action.run.mockReset().mockResolvedValue({
    _tag: "Success",
    value: { branch: "feature/diagnostics", worktreePath: null },
  });
  testState.action.resetError.mockReset();
  testState.action.error = null;
  testState.action.isPending = false;
  testState.interrupted = false;
  testState.sourceControlProvider = "github";
});

afterEach(async () => {
  for (const mounted of mountedTrees.splice(0)) {
    await act(async () => mounted.root.unmount());
    mounted.container.remove();
  }
  document.body.replaceChildren();
  if (originalGetAnimationsDescriptor) {
    Object.defineProperty(Element.prototype, "getAnimations", originalGetAnimationsDescriptor);
  } else {
    Reflect.deleteProperty(Element.prototype, "getAnimations");
  }
  originalGetAnimationsDescriptor = undefined;
  vi.restoreAllMocks();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

afterAll(() => {
  expect(Object.getOwnPropertyDescriptor(Element.prototype, "getAnimations")).toEqual(
    suiteGetAnimationsDescriptor,
  );
});

describe("PullRequestThreadDialog mounted behavior", () => {
  it("renders nothing while closed and focuses the reference field when opened", async () => {
    const mounted = await mount(renderDialog({ open: false }));
    expect(document.querySelector("[role='dialog']")).toBeNull();

    await act(async () => {
      mounted.root.render(renderDialog({ open: true }));
      await new Promise((resolve) => requestAnimationFrame(resolve));
    });
    expect(document.querySelector("[role='dialog']")).not.toBeNull();
    expect(document.activeElement).toBe(referenceInput());
  });

  it("shows distinct empty and malformed reference validation", async () => {
    await mount(renderDialog({ initialReference: "" }));
    const input = referenceInput();

    await pressEnter(input);
    expect(document.body.textContent).toContain("Paste a pull request URL");

    await changeInput(input, "not a pull request");
    await pressEnter(input);
    expect(document.body.textContent).toContain("Use a pull request URL");
    expect(testState.action.run).not.toHaveBeenCalled();
  });

  it("renders resolved metadata and prepares both local and worktree threads", async () => {
    const onPrepared = vi.fn().mockResolvedValue(undefined);
    const onOpenChange = vi.fn();
    await mount(renderDialog({ onPrepared, onOpenChange }));

    expect(document.body.textContent).toContain("Improve diagnostics");
    expect(document.body.textContent).toContain("#42 · feature/diagnostics to main");
    expect(document.body.textContent).toContain("open");

    await click(buttonWithText("Local"));
    expect(testState.action.run).toHaveBeenNthCalledWith(1, {
      reference: "42",
    });
    expect(onPrepared).toHaveBeenNthCalledWith(1, {
      branch: "feature/diagnostics",
      mode: "local",
    });

    await click(buttonWithText("Worktree"));
    expect(testState.action.run).toHaveBeenCalledTimes(1);
    expect(onPrepared).toHaveBeenNthCalledWith(2, {
      branch: "feature/diagnostics",
      mode: "worktree",
    });
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("resets interrupted preparation failures without closing the dialog", async () => {
    const onOpenChange = vi.fn();
    const onPrepared = vi.fn();
    testState.interrupted = true;
    testState.action.run.mockResolvedValue({ _tag: "Failure", cause: "interrupted" });
    await mount(renderDialog({ onOpenChange, onPrepared }));

    await click(buttonWithText("Local"));
    expect(testState.action.resetError).toHaveBeenCalledTimes(1);
    expect(onPrepared).not.toHaveBeenCalled();
    expect(onOpenChange).not.toHaveBeenCalled();
  });

  it("surfaces resolution and preparation errors and resolving progress", async () => {
    testState.cachedPullRequest = null;
    testState.resolution = {
      data: undefined,
      error: "Unable to resolve pull request",
      isPending: false,
      isFetching: false,
    };
    await mount(renderDialog());
    expect(document.body.textContent).toContain("Unable to resolve pull request");

    testState.cachedPullRequest = pullRequest("closed");
    testState.resolution = {
      data: { pullRequest: pullRequest("closed") },
      error: null,
      isPending: false,
      isFetching: false,
    };
    testState.action.error = new Error("Checkout failed");
    await mount(renderDialog());
    expect(document.body.textContent).toContain("Checkout failed");
    expect(document.body.textContent).toContain("closed");

    testState.action.error = "opaque failure";
    await mount(renderDialog());
    expect(document.body.textContent).toContain("Failed to prepare pull request thread.");

    testState.cachedPullRequest = null;
    testState.action.error = null;
    testState.resolution = {
      data: undefined,
      error: null,
      isPending: true,
      isFetching: true,
    };
    await mount(renderDialog());
    expect(document.body.textContent).toContain("Resolving pull request...");
  });

  it("blocks dismiss and preparation controls while an action is pending", async () => {
    const onOpenChange = vi.fn();
    testState.action.isPending = true;
    await mount(renderDialog({ onOpenChange }));

    expect(buttonWithText("Cancel").disabled).toBe(true);
    expect(buttonWithText("Local").disabled).toBe(true);
    expect(buttonWithText("Worktree").disabled).toBe(true);
    await click(document.querySelector<HTMLButtonElement>('button[aria-label="Close"]')!);
    expect(onOpenChange).not.toHaveBeenCalled();
  });

  it("disables preparation without a repository and allows explicit cancellation otherwise", async () => {
    const onOpenChange = vi.fn();
    const mounted = await mount(renderDialog({ cwd: null, onOpenChange }));
    expect(buttonWithText("Local").disabled).toBe(true);
    expect(buttonWithText("Worktree").disabled).toBe(true);

    await act(async () => mounted.root.render(renderDialog({ onOpenChange })));
    await click(buttonWithText("Cancel"));
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });
});
