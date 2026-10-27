// @vitest-environment happy-dom

import {
  AgentSessionsError,
  EnvironmentId,
  ProjectId,
  ThreadId,
  type AgentSessionCandidate,
} from "@coddy/contracts";
import * as Cause from "effect/Cause";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => {
  const commands = new Map<string, (input: unknown) => Promise<unknown>>();
  const calls: Array<{
    readonly label: string;
    readonly input: unknown;
    readonly signal: AbortSignal | undefined;
  }> = [];
  // One runner per command, stable across renders like the real hook's callback.
  const runners = new Map(
    ["scan", "import"].map((label) => [
      label,
      async (input: unknown, runOptions?: { signal?: AbortSignal }): Promise<unknown> => {
        calls.push({ label, input, signal: runOptions?.signal });
        const run = commands.get(label);
        if (!run) throw new Error(`Missing ${label} command mock`);
        return run(input);
      },
    ]),
  );
  return { commands, calls, runners };
});

vi.mock("../state/use-atom-command", () => ({
  useAtomCommand: (command: { label: string }) => h.runners.get(command.label),
}));

vi.mock("../state/projects", () => ({
  projectEnvironment: {
    scanAgentSessions: { label: "scan" },
    importAgentSessions: { label: "import" },
  },
}));

vi.mock("./ui/checkbox", () => ({
  Checkbox: (props: {
    checked: boolean;
    indeterminate?: boolean;
    disabled?: boolean;
    onCheckedChange: (checked: boolean) => void;
  }) => (
    <input
      type="checkbox"
      checked={props.checked}
      disabled={props.disabled}
      data-indeterminate={props.indeterminate ? "true" : undefined}
      onChange={(event) => props.onCheckedChange(event.currentTarget.checked)}
    />
  ),
}));

vi.mock("./ui/dialog", () => ({
  Dialog: ({ open, children }: { open: boolean; children: unknown }) =>
    open ? <div role="presentation">{children as never}</div> : null,
  DialogPopup: ({ children }: { children: unknown }) => (
    <div role="dialog">{children as never}</div>
  ),
  DialogHeader: ({ children }: { children: unknown }) => <header>{children as never}</header>,
  DialogTitle: ({ children }: { children: unknown }) => <h2>{children as never}</h2>,
  DialogDescription: ({ children }: { children: unknown }) => <p>{children as never}</p>,
  DialogPanel: ({ children }: { children: unknown }) => <div>{children as never}</div>,
  DialogFooter: ({ children }: { children: unknown }) => <footer>{children as never}</footer>,
}));

import { ImportCliSessionsDialog, summarizeImport } from "./ImportCliSessionsDialog";

const target = {
  environmentId: EnvironmentId.make("environment-one"),
  projectId: ProjectId.make("project-one"),
  workspaceRoot: "/work/repo",
};

const minutesAgo = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString();

const candidates: AgentSessionCandidate[] = [
  {
    provider: "codex",
    sessionId: "codex-new",
    title: "Add a test",
    messageCount: 4,
    lastActiveAt: minutesAgo(5),
    alreadyImported: false,
  },
  {
    provider: "claudeAgent",
    sessionId: "claude-old",
    title: "Explain the build",
    messageCount: 1,
    lastActiveAt: minutesAgo(120),
    alreadyImported: false,
  },
  {
    provider: "claudeAgent",
    sessionId: "claude-done",
    title: "Earlier import",
    messageCount: 9,
    lastActiveAt: minutesAgo(600),
    alreadyImported: true,
    threadId: ThreadId.make("import:claudeAgent:claude-done"),
  },
];

let root: Root;
let container: HTMLDivElement;
const onImported = vi.fn();
const onOpenThread = vi.fn();

async function settle() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function renderDialog() {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () => {
    root.render(
      <ImportCliSessionsDialog
        open
        target={target}
        onOpenChange={vi.fn()}
        onImported={onImported}
        onOpenThread={onOpenThread}
      />,
    );
  });
  await settle();
}

function button(name: string): HTMLButtonElement {
  const match = [...container.querySelectorAll<HTMLButtonElement>("button")].find(
    (candidate) => candidate.textContent?.trim() === name,
  );
  expect(match, name).toBeDefined();
  return match!;
}

function checkbox(labelText: string): HTMLInputElement {
  const label = [...container.querySelectorAll("label")].find((candidate) =>
    candidate.textContent?.includes(labelText),
  );
  expect(label, labelText).toBeDefined();
  return label!.querySelector("input")!;
}

async function click(element: HTMLElement) {
  await act(async () => {
    element.click();
  });
  await settle();
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.calls.length = 0;
  h.commands.clear();
  onImported.mockReset();
  onOpenThread.mockReset();
  h.commands.set("scan", async () => ({
    _tag: "Success",
    value: { candidates, truncated: false },
  }));
});

afterEach(async () => {
  if (root) await act(async () => root.unmount());
  container?.remove();
});

describe("ImportCliSessionsDialog", () => {
  it("lists sessions and explains why Import waits for a selection", async () => {
    await renderDialog();

    expect(h.calls.map(({ label, input }) => ({ label, input }))).toEqual([
      {
        label: "scan",
        input: { environmentId: target.environmentId, input: { projectId: target.projectId } },
      },
    ]);
    expect(container.textContent).toContain("Codex · 5m ago · 4 messages");
    expect(container.textContent).toContain("Claude Code · 2h ago · 1 message");
    expect(checkbox("Earlier import").disabled).toBe(true);
    expect(container.textContent).toContain("Already imported");
    const importButton = button("Import");
    expect(importButton.disabled).toBe(true);
    const reason = container.querySelector(
      `#${CSS.escape(importButton.getAttribute("aria-describedby")!)}`,
    );
    expect(reason?.textContent).toBe("Select the sessions to import.");

    await click(button("Open"));
    expect(onOpenThread).toHaveBeenCalledWith(target, "import:claudeAgent:claude-done");
  });

  it("imports the selected sessions and opens the most recent one", async () => {
    h.commands.set("import", async () => ({
      _tag: "Success",
      value: {
        imported: [
          { sessionId: "claude-old", threadId: "import:claudeAgent:claude-old" },
          { sessionId: "codex-new", threadId: "import:codex:codex-new" },
        ],
        skipped: [],
      },
    }));
    await renderDialog();

    await click(checkbox("Select all (2)"));
    await click(button("Import 2 sessions"));

    expect(h.calls.at(-1)).toEqual({
      label: "import",
      input: {
        environmentId: target.environmentId,
        input: {
          projectId: target.projectId,
          sessions: [
            { provider: "codex", sessionId: "codex-new" },
            { provider: "claudeAgent", sessionId: "claude-old" },
          ],
        },
      },
    });
    expect(onImported).toHaveBeenCalledWith(target, {
      type: "success",
      title: "Imported 2 sessions",
      newestThreadId: "import:codex:codex-new",
    });
  });

  it("keeps Open unavailable while an import runs, so it cannot race the import's navigation", async () => {
    let finish: (value: unknown) => void = () => {};
    h.commands.set(
      "import",
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    await renderDialog();

    await click(checkbox("Add a test"));
    await click(button("Import 1 session"));
    expect(button("Open").disabled).toBe(true);
    expect(button("Importing…").disabled).toBe(true);

    await act(async () => {
      finish({ _tag: "Success", value: { imported: [], skipped: [] } });
    });
    await settle();
    expect(onImported).toHaveBeenCalledTimes(1);
  });

  it("shows the empty state for the workspace root", async () => {
    h.commands.set("scan", async () => ({
      _tag: "Success",
      value: { candidates: [], truncated: false },
    }));
    await renderDialog();

    expect(container.textContent).toContain(
      "No Claude Code or Codex sessions found for /work/repo in the last 30 days.",
    );
    expect(container.textContent).not.toContain("Import 0");
  });

  it("stops a running scan when the dialog closes", async () => {
    h.commands.set("scan", () => new Promise<never>(() => {}));
    await renderDialog();
    const signal = h.calls.find((call) => call.label === "scan")?.signal;
    expect(signal?.aborted).toBe(false);

    await act(async () => {
      root.render(
        <ImportCliSessionsDialog
          open={false}
          target={target}
          onOpenChange={vi.fn()}
          onImported={onImported}
          onOpenThread={onOpenThread}
        />,
      );
    });

    expect(signal?.aborted).toBe(true);
  });

  it("stops a retried scan when the dialog closes", async () => {
    h.commands.set("scan", async () => ({
      _tag: "Failure",
      cause: Cause.fail(new AgentSessionsError({ message: "The project no longer exists." })),
    }));
    await renderDialog();
    h.commands.set("scan", () => new Promise<never>(() => {}));
    await click(button("Try again"));
    const retry = h.calls.findLast((call) => call.label === "scan")?.signal;
    expect(retry?.aborted).toBe(false);

    await act(async () => {
      root.render(
        <ImportCliSessionsDialog
          open={false}
          target={target}
          onOpenChange={vi.fn()}
          onImported={onImported}
          onOpenThread={onOpenThread}
        />,
      );
    });

    expect(retry?.aborted).toBe(true);
  });

  it("reports a failed scan and scans again on request", async () => {
    h.commands.set("scan", async () => ({
      _tag: "Failure",
      cause: Cause.fail(new AgentSessionsError({ message: "The project no longer exists." })),
    }));
    await renderDialog();

    expect(container.querySelector('[role="alert"]')?.textContent).toContain(
      "The project no longer exists.",
    );
    h.commands.set("scan", async () => ({
      _tag: "Success",
      value: { candidates, truncated: true },
    }));
    await click(button("Try again"));

    expect(container.querySelector('[role="alert"]')).toBeNull();
    expect(container.textContent).toContain("Showing the 3 most recent sessions.");
  });
});

describe("summarizeImport", () => {
  it("names skipped sessions and their reasons", () => {
    expect(
      summarizeImport(
        {
          imported: [{ sessionId: "codex-new", threadId: ThreadId.make("import:codex:codex-new") }],
          skipped: [{ sessionId: "claude-old", reason: "Already imported." }],
        },
        candidates,
      ),
    ).toEqual({
      type: "warning",
      title: "Imported 1 session",
      description: "1 session skipped: Already imported.",
      newestThreadId: "import:codex:codex-new",
    });
    expect(
      summarizeImport(
        {
          imported: [],
          skipped: [
            { sessionId: "a", reason: "The session was not run in this project's folder." },
            { sessionId: "b", reason: "The session was not run in this project's folder." },
          ],
        },
        candidates,
      ),
    ).toEqual({
      type: "error",
      title: "No sessions imported",
      description: "2 sessions skipped: The session was not run in this project's folder.",
      newestThreadId: null,
    });
  });
});
