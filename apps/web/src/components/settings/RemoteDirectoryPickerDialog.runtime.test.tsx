// @vitest-environment happy-dom

import { EnvironmentId } from "@coddy/contracts";
import * as Cause from "effect/Cause";
import { AsyncResult } from "effect/reactivity";
import { act, type ReactElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  browseCalls: [] as Array<{ environmentId: string; input: { partialPath: string; mode: string } }>,
  refresh: vi.fn(),
  refreshCalls: [] as string[],
  createEntry: vi.fn(),
  activeCreateEntry: null as ReturnType<typeof vi.fn> | null,
  resolveOnQueryRender: null as (() => void) | null,
  responses: new Map<
    string,
    {
      data: Record<string, unknown> | null;
      error: string | null;
      isPending: boolean;
    }
  >(),
  reset() {
    this.browseCalls.length = 0;
    this.refresh.mockReset();
    this.refreshCalls.length = 0;
    this.createEntry
      .mockReset()
      .mockResolvedValue(AsyncResult.success({ relativePath: "new-folder" }));
    this.activeCreateEntry = this.createEntry;
    this.resolveOnQueryRender = null;
    this.responses.clear();
  },
}));

function queryKey(target: { environmentId: string; input: { partialPath: string } }): string {
  return `${target.environmentId}:${target.input.partialPath}`;
}

vi.mock("~/state/filesystem", () => ({
  filesystemEnvironment: {
    browse: (target: { environmentId: string; input: { partialPath: string; mode: string } }) => {
      h.browseCalls.push(target);
      return target;
    },
  },
}));

vi.mock("~/state/query", () => ({
  useEnvironmentQuery: (
    target: { environmentId: string; input: { partialPath: string } } | null,
  ) => {
    const resolve = h.resolveOnQueryRender;
    h.resolveOnQueryRender = null;
    resolve?.();
    return target === null
      ? {
          data: null,
          error: null,
          isPending: false,
          refresh: h.refresh,
          revalidate: h.refresh,
          requiresRetry: false,
        }
      : {
          ...(h.responses.get(queryKey(target)) ?? {
            data: null,
            error: null,
            isPending: true,
          }),
          refresh: () => {
            h.refresh();
            h.refreshCalls.push(queryKey(target));
          },
          revalidate: () => {
            h.refresh();
            h.refreshCalls.push(queryKey(target));
          },
          requiresRetry: false,
        };
  },
}));

vi.mock("~/state/projects", () => ({
  projectEnvironment: { createEntry: "project-create-entry" },
}));

vi.mock("~/state/use-atom-command", () => ({
  useAtomCommand: () => h.activeCreateEntry ?? h.createEntry,
}));

const { RemoteDirectoryPickerDialog } = await import("./RemoteDirectoryPickerDialog");
type RemoteDirectoryPickerDialogProps = React.ComponentProps<typeof RemoteDirectoryPickerDialog>;

interface MountedPicker {
  readonly container: HTMLDivElement;
  readonly root: Root;
}

const mounted: MountedPicker[] = [];

function pickerProps(
  overrides: Partial<RemoteDirectoryPickerDialogProps> = {},
): RemoteDirectoryPickerDialogProps {
  return {
    open: true,
    environmentId: EnvironmentId.make("environment-one"),
    initialPath: "/workspace",
    onOpenChange: vi.fn(),
    onSelect: vi.fn(),
    ...overrides,
  };
}

async function mount(element: ReactElement): Promise<MountedPicker> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const picker = { container, root };
  mounted.push(picker);
  await act(async () => root.render(element));
  return picker;
}

async function rerender(picker: MountedPicker, element: ReactElement): Promise<void> {
  await act(async () => picker.root.render(element));
}

function button(label: string): HTMLButtonElement {
  const found = Array.from(document.body.querySelectorAll("button")).find(
    (candidate) => candidate.textContent?.trim() === label,
  );
  if (!found) throw new Error(`Missing button: ${label}`);
  return found;
}

function input(label: string): HTMLInputElement {
  const found = document.body.querySelector<HTMLInputElement>(`input[aria-label="${label}"]`);
  if (!found) throw new Error(`Missing input: ${label}`);
  return found;
}

async function typeValue(element: HTMLInputElement, value: string): Promise<void> {
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
    setter?.call(element, value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

function deferred<T>(): {
  readonly promise: Promise<T>;
  readonly resolve: (value: T) => void;
} {
  let resolve!: (value: T) => void;
  return {
    promise: new Promise<T>((complete) => {
      resolve = complete;
    }),
    resolve,
  };
}

function synchronousDeferred<T>(): {
  readonly promise: Promise<T>;
  readonly resolve: (value: T) => void;
} {
  let complete: ((value: T) => unknown) | undefined;
  const promise = {};
  Object.defineProperty(promise, Promise.prototype.then.name, {
    value(onFulfilled: ((value: T) => unknown) | null | undefined) {
      complete = onFulfilled ?? undefined;
      return Promise.resolve();
    },
  });
  return {
    promise: promise as Promise<T>,
    resolve(value) {
      complete?.(value);
    },
  };
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.reset();
});

afterEach(async () => {
  for (const picker of mounted.splice(0)) {
    await act(async () => picker.root.unmount());
    picker.container.remove();
  }
});

describe("RemoteDirectoryPickerDialog runtime behavior", () => {
  it("creates a folder on the selected host and enters the server-normalized POSIX path", async () => {
    h.responses.set("environment-one:/workspace", {
      data: { directoryPath: "/canonical/workspace", breadcrumbs: [], entries: [] },
      error: null,
      isPending: false,
    });
    h.createEntry.mockResolvedValueOnce(AsyncResult.success({ relativePath: "server-normalized" }));
    await mount(<RemoteDirectoryPickerDialog {...pickerProps()} />);

    await act(async () => button("New folder").click());
    await typeValue(input("New folder name"), "  generated  ");
    await act(async () => button("Create").click());

    expect(h.createEntry).toHaveBeenCalledWith({
      environmentId: EnvironmentId.make("environment-one"),
      input: {
        cwd: "/canonical/workspace",
        relativePath: "generated",
        kind: "directory",
      },
    });
    expect(h.browseCalls.at(-1)).toEqual({
      environmentId: "environment-one",
      input: { partialPath: "/canonical/workspace/server-normalized", mode: "directory" },
    });
    expect(h.refreshCalls.at(-1)).toBe("environment-one:/canonical/workspace/server-normalized");
  });

  it("submits with Enter for the active environment and preserves Windows host syntax", async () => {
    h.responses.set("environment-two:X:\\Workspaces", {
      data: { directoryPath: "X:\\Canonical\\Workspaces", breadcrumbs: [], entries: [] },
      error: null,
      isPending: false,
    });
    h.createEntry.mockResolvedValueOnce(AsyncResult.success({ relativePath: "created" }));
    await mount(
      <RemoteDirectoryPickerDialog
        {...pickerProps({
          environmentId: EnvironmentId.make("environment-two"),
          initialPath: "X:\\Workspaces",
        })}
      />,
    );

    await act(async () => button("New folder").click());
    await typeValue(input("New folder name"), "generated");
    await act(async () =>
      input("New folder name").dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
      ),
    );

    expect(h.createEntry).toHaveBeenCalledWith({
      environmentId: EnvironmentId.make("environment-two"),
      input: {
        cwd: "X:\\Canonical\\Workspaces",
        relativePath: "generated",
        kind: "directory",
      },
    });
    expect(h.browseCalls.at(-1)).toEqual({
      environmentId: "environment-two",
      input: { partialPath: "X:\\Canonical\\Workspaces\\created", mode: "directory" },
    });
  });

  it("validates a single non-empty folder name before creating", async () => {
    h.responses.set("environment-one:/workspace", {
      data: { directoryPath: "/workspace", breadcrumbs: [], entries: [] },
      error: null,
      isPending: false,
    });
    await mount(<RemoteDirectoryPickerDialog {...pickerProps()} />);

    await act(async () => button("New folder").click());
    expect(button("Create").disabled).toBe(true);
    expect(document.body.querySelector('[role="alert"]')).toBeNull();

    await act(async () =>
      input("New folder name").dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
      ),
    );
    expect(document.body.querySelector('[role="alert"]')?.textContent).toContain(
      "Enter a folder name",
    );
    expect(h.createEntry).not.toHaveBeenCalled();
    expect(input("New folder name")).toBe(document.activeElement);

    await typeValue(input("New folder name"), "   ");
    expect(button("Create").disabled).toBe(true);

    await typeValue(input("New folder name"), "nested/folder");
    expect(button("Create").disabled).toBe(true);
    expect(document.body.querySelector('[role="alert"]')?.textContent).toContain(
      "single folder name",
    );

    await typeValue(input("New folder name"), "..");
    expect(button("Create").disabled).toBe(true);
    expect(h.createEntry).not.toHaveBeenCalled();
  });

  it("cancels the inline editor with Escape or its Cancel button", async () => {
    h.responses.set("environment-one:/workspace", {
      data: { directoryPath: "/workspace", breadcrumbs: [], entries: [] },
      error: null,
      isPending: false,
    });
    await mount(<RemoteDirectoryPickerDialog {...pickerProps()} />);

    await act(async () => button("New folder").click());
    await typeValue(input("New folder name"), "generated");
    await act(async () =>
      input("New folder name").dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
      ),
    );
    expect(document.body.querySelector('input[aria-label="New folder name"]')).toBeNull();

    await act(async () => button("New folder").click());
    await typeValue(input("New folder name"), "again");
    await act(async () => button("Cancel").click());
    expect(document.body.querySelector('input[aria-label="New folder name"]')).toBeNull();
    expect(h.createEntry).not.toHaveBeenCalled();
  });

  it("blocks duplicate submissions and locks the editor while creation is pending", async () => {
    h.responses.set("environment-one:/workspace", {
      data: { directoryPath: "/workspace", breadcrumbs: [], entries: [] },
      error: null,
      isPending: false,
    });
    const creation = deferred<ReturnType<typeof AsyncResult.success<{ relativePath: string }>>>();
    h.createEntry.mockReturnValueOnce(creation.promise);
    await mount(<RemoteDirectoryPickerDialog {...pickerProps()} />);

    await act(async () => button("New folder").click());
    await typeValue(input("New folder name"), "generated");
    await act(async () => button("Create").click());

    expect(input("New folder name").disabled).toBe(true);
    expect(button("Creating…").disabled).toBe(true);
    expect(button("Cancel").disabled).toBe(true);
    const form = input("New folder name").closest("form");
    if (!form) throw new Error("Missing new-folder form");
    await act(async () =>
      form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })),
    );
    expect(h.createEntry).toHaveBeenCalledOnce();

    await act(async () => creation.resolve(AsyncResult.success({ relativePath: "generated" })));
  });

  it("keeps one create pending across same-context query loading churn", async () => {
    const props = pickerProps();
    h.responses.set("environment-one:/workspace", {
      data: { directoryPath: "/workspace", breadcrumbs: [], entries: [] },
      error: null,
      isPending: false,
    });
    const creation = deferred<ReturnType<typeof AsyncResult.success<{ relativePath: string }>>>();
    h.createEntry.mockReturnValueOnce(creation.promise);
    const picker = await mount(<RemoteDirectoryPickerDialog {...props} />);
    await act(async () => button("New folder").click());
    await typeValue(input("New folder name"), "generated");
    await act(async () => button("Create").click());

    h.responses.set("environment-one:/workspace", {
      data: { directoryPath: "/background/canonical", breadcrumbs: [], entries: [] },
      error: null,
      isPending: true,
    });
    await rerender(picker, <RemoteDirectoryPickerDialog {...props} />);
    expect(input("New folder name").disabled).toBe(true);
    expect(button("Creating…").disabled).toBe(true);

    h.responses.set("environment-one:/workspace", {
      data: { directoryPath: "/background/canonical", breadcrumbs: [], entries: [] },
      error: "Temporary browse failure",
      isPending: false,
    });
    await rerender(picker, <RemoteDirectoryPickerDialog {...props} />);
    expect(input("New folder name").disabled).toBe(true);
    expect(button("Creating…").disabled).toBe(true);

    h.responses.set("environment-one:/workspace", {
      data: { directoryPath: "/workspace", breadcrumbs: [], entries: [] },
      error: null,
      isPending: false,
    });
    await rerender(picker, <RemoteDirectoryPickerDialog {...props} />);
    expect(input("New folder name").disabled).toBe(true);
    expect(h.createEntry).toHaveBeenCalledOnce();

    await act(async () => creation.resolve(AsyncResult.success({ relativePath: "generated" })));
    expect(
      h.browseCalls.filter(
        (call) =>
          call.environmentId === "environment-one" &&
          call.input.partialPath === "/workspace/generated",
      ),
    ).toHaveLength(1);
    expect(
      h.refreshCalls.filter((key) => key === "environment-one:/workspace/generated"),
    ).toHaveLength(1);
  });

  it("keeps the name and exposes a mutation failure so creation can be retried", async () => {
    h.responses.set("environment-one:/workspace", {
      data: { directoryPath: "/workspace", breadcrumbs: [], entries: [] },
      error: null,
      isPending: false,
    });
    h.createEntry
      .mockResolvedValueOnce(AsyncResult.failure(Cause.fail(new Error("Access denied"))))
      .mockResolvedValueOnce(AsyncResult.success({ relativePath: "generated" }));
    await mount(<RemoteDirectoryPickerDialog {...pickerProps()} />);

    await act(async () => button("New folder").click());
    await typeValue(input("New folder name"), "generated");
    await act(async () => button("Create").click());

    expect(input("New folder name").value).toBe("generated");
    expect(document.body.querySelector('[role="alert"]')?.textContent).toContain("Access denied");
    expect(button("Create").disabled).toBe(false);

    await act(async () => button("Create").click());
    expect(h.createEntry).toHaveBeenCalledTimes(2);
    expect(h.browseCalls.at(-1)?.input.partialPath).toBe("/workspace/generated");
  });

  it("keeps an interrupted creation retryable without showing a false error", async () => {
    h.responses.set("environment-one:/workspace", {
      data: { directoryPath: "/workspace", breadcrumbs: [], entries: [] },
      error: null,
      isPending: false,
    });
    h.createEntry.mockResolvedValueOnce(AsyncResult.failure(Cause.interrupt(1)));
    await mount(<RemoteDirectoryPickerDialog {...pickerProps()} />);

    await act(async () => button("New folder").click());
    await typeValue(input("New folder name"), "generated");
    await act(async () => button("Create").click());

    expect(input("New folder name").value).toBe("generated");
    expect(button("Create").disabled).toBe(false);
    expect(document.body.querySelector('[role="alert"]')).toBeNull();
  });

  it("ignores a stale creation completion after the environment changes", async () => {
    h.responses.set("environment-one:/workspace", {
      data: { directoryPath: "/canonical/one", breadcrumbs: [], entries: [] },
      error: null,
      isPending: false,
    });
    h.responses.set("environment-two:/remote", {
      data: { directoryPath: "/canonical/two", breadcrumbs: [], entries: [] },
      error: null,
      isPending: false,
    });
    const creation = deferred<ReturnType<typeof AsyncResult.success<{ relativePath: string }>>>();
    h.createEntry.mockReturnValueOnce(creation.promise);
    const picker = await mount(<RemoteDirectoryPickerDialog {...pickerProps()} />);
    await act(async () => button("New folder").click());
    await typeValue(input("New folder name"), "generated");
    await act(async () => button("Create").click());

    await rerender(
      picker,
      <RemoteDirectoryPickerDialog
        {...pickerProps({
          environmentId: EnvironmentId.make("environment-two"),
          initialPath: "/remote",
        })}
      />,
    );
    await act(async () => creation.resolve(AsyncResult.success({ relativePath: "stale" })));

    expect(h.browseCalls.at(-1)?.input.partialPath).toBe("/remote");
    expect(document.body.querySelector('input[aria-label="New folder name"]')).toBeNull();
  });

  it("ignores a stale creation completion after browsing to another path", async () => {
    h.responses.set("environment-one:/workspace", {
      data: {
        directoryPath: "/workspace",
        breadcrumbs: [],
        entries: [{ name: "elsewhere", fullPath: "/workspace/elsewhere" }],
      },
      error: null,
      isPending: false,
    });
    const creation = deferred<ReturnType<typeof AsyncResult.success<{ relativePath: string }>>>();
    h.createEntry.mockReturnValueOnce(creation.promise);
    await mount(<RemoteDirectoryPickerDialog {...pickerProps()} />);
    await act(async () => button("New folder").click());
    await typeValue(input("New folder name"), "generated");
    await act(async () => button("Create").click());

    await act(async () => button("elsewhere").click());
    await act(async () => creation.resolve(AsyncResult.success({ relativePath: "stale" })));

    expect(h.browseCalls.at(-1)?.input.partialPath).toBe("/workspace/elsewhere");
    expect(document.body.querySelector('input[aria-label="New folder name"]')).toBeNull();
  });

  it("ignores a stale creation completion after the dialog closes and reopens", async () => {
    h.responses.set("environment-one:/workspace", {
      data: { directoryPath: "/workspace", breadcrumbs: [], entries: [] },
      error: null,
      isPending: false,
    });
    const creation = deferred<ReturnType<typeof AsyncResult.success<{ relativePath: string }>>>();
    h.createEntry.mockReturnValueOnce(creation.promise);
    const picker = await mount(<RemoteDirectoryPickerDialog {...pickerProps()} />);
    await act(async () => button("New folder").click());
    await typeValue(input("New folder name"), "generated");
    await act(async () => button("Create").click());

    await rerender(picker, <RemoteDirectoryPickerDialog {...pickerProps({ open: false })} />);
    await rerender(picker, <RemoteDirectoryPickerDialog {...pickerProps()} />);
    await act(async () => creation.resolve(AsyncResult.success({ relativePath: "stale" })));

    expect(h.browseCalls.at(-1)?.input.partialPath).toBe("/workspace");
    expect(document.body.querySelector('input[aria-label="New folder name"]')).toBeNull();
  });

  // "open" is intentionally not covered here: resetKey is now environmentId, so an
  // in-flight completion resolving during the render where only `open` changes is no
  // longer rejected by resetKey — the browser stays mounted (and this context valid)
  // through Base UI's exit transition by design. A genuine close-then-reopen still
  // discards the stale completion because Base UI actually unmounts and remounts the
  // component once the exit transition finishes; see "ignores a stale creation
  // completion after the dialog closes and reopens" above.
  it.each(["environment", "initialPath", "command"] as const)(
    "rejects a completion resolved during the %s replacement render",
    async (replacement) => {
      h.responses.set("environment-one:/workspace", {
        data: { directoryPath: "/workspace", breadcrumbs: [], entries: [] },
        error: null,
        isPending: false,
      });
      h.responses.set("environment-two:/remote", {
        data: { directoryPath: "/remote", breadcrumbs: [], entries: [] },
        error: null,
        isPending: false,
      });
      h.responses.set("environment-one:/other", {
        data: { directoryPath: "/other", breadcrumbs: [], entries: [] },
        error: null,
        isPending: false,
      });
      const creation =
        synchronousDeferred<ReturnType<typeof AsyncResult.success<{ relativePath: string }>>>();
      h.createEntry.mockReturnValueOnce(creation.promise);
      const picker = await mount(<RemoteDirectoryPickerDialog {...pickerProps()} />);
      await act(async () => button("New folder").click());
      await typeValue(input("New folder name"), "generated");
      await act(async () => button("Create").click());

      let nextProps = pickerProps();
      if (replacement === "environment") {
        nextProps = pickerProps({
          environmentId: EnvironmentId.make("environment-two"),
          initialPath: "/remote",
        });
      } else if (replacement === "initialPath") {
        nextProps = pickerProps({ initialPath: "/other" });
      } else {
        h.activeCreateEntry = vi
          .fn()
          .mockResolvedValue(AsyncResult.success({ relativePath: "replacement" }));
      }

      h.resolveOnQueryRender = () =>
        creation.resolve(AsyncResult.success({ relativePath: "stale" }));
      await rerender(picker, <RemoteDirectoryPickerDialog {...nextProps} />);

      expect(h.browseCalls.some((call) => call.input.partialPath.endsWith("/stale"))).toBe(false);
      expect(h.refreshCalls.some((key) => key.endsWith("/stale"))).toBe(false);
    },
  );

  it("invalidates creation synchronously when the user browses to another path", async () => {
    h.responses.set("environment-one:/workspace", {
      data: {
        directoryPath: "/workspace",
        breadcrumbs: [],
        entries: [{ name: "elsewhere", fullPath: "/workspace/elsewhere" }],
      },
      error: null,
      isPending: false,
    });
    const creation =
      synchronousDeferred<ReturnType<typeof AsyncResult.success<{ relativePath: string }>>>();
    h.createEntry.mockReturnValueOnce(creation.promise);
    await mount(<RemoteDirectoryPickerDialog {...pickerProps()} />);
    await act(async () => button("New folder").click());
    await typeValue(input("New folder name"), "generated");
    await act(async () => button("Create").click());

    h.resolveOnQueryRender = () => creation.resolve(AsyncResult.success({ relativePath: "stale" }));
    await act(async () => button("elsewhere").click());

    expect(h.browseCalls.at(-1)?.input.partialPath).toBe("/workspace/elsewhere");
    expect(h.browseCalls.some((call) => call.input.partialPath === "/workspace/stale")).toBe(false);
    expect(h.refreshCalls.some((key) => key.endsWith("/stale"))).toBe(false);
  });

  it("does not refresh a same-path target after the host changes in the success batch", async () => {
    h.responses.set("environment-one:/workspace", {
      data: { directoryPath: "/workspace", breadcrumbs: [], entries: [] },
      error: null,
      isPending: false,
    });
    h.responses.set("environment-two:/workspace/created", {
      data: { directoryPath: "/workspace/created", breadcrumbs: [], entries: [] },
      error: null,
      isPending: false,
    });
    const creation =
      synchronousDeferred<ReturnType<typeof AsyncResult.success<{ relativePath: string }>>>();
    h.createEntry.mockReturnValueOnce(creation.promise);
    const picker = await mount(<RemoteDirectoryPickerDialog {...pickerProps()} />);
    await act(async () => button("New folder").click());
    await typeValue(input("New folder name"), "generated");
    await act(async () => button("Create").click());

    await act(async () => {
      creation.resolve(AsyncResult.success({ relativePath: "created" }));
      picker.root.render(
        <RemoteDirectoryPickerDialog
          {...pickerProps({
            environmentId: EnvironmentId.make("environment-two"),
            initialPath: "/workspace/created",
          })}
        />,
      );
    });

    expect(h.refreshCalls).not.toContain("environment-one:/workspace/created");
    expect(h.refreshCalls).not.toContain("environment-two:/workspace/created");
  });

  it.each(["", "../outside", "/absolute"])(
    "keeps the editor retryable when the server returns an unusable child path %j",
    async (relativePath) => {
      h.responses.set("environment-one:/workspace", {
        data: { directoryPath: "/workspace", breadcrumbs: [], entries: [] },
        error: null,
        isPending: false,
      });
      h.createEntry.mockResolvedValueOnce(AsyncResult.success({ relativePath }));
      await mount(<RemoteDirectoryPickerDialog {...pickerProps()} />);
      await act(async () => button("New folder").click());
      await typeValue(input("New folder name"), "generated");
      await act(async () => button("Create").click());

      expect(input("New folder name").value).toBe("generated");
      expect(button("Create").disabled).toBe(false);
      expect(document.body.querySelector('[role="alert"]')?.textContent).toContain(
        "valid created folder path",
      );
      expect(h.browseCalls.at(-1)?.input.partialPath).toBe("/workspace");
      expect(h.refreshCalls).toEqual([]);
    },
  );

  it("enables New folder only for a resolved, non-pending canonical directory", async () => {
    const picker = await mount(<RemoteDirectoryPickerDialog {...pickerProps()} />);
    expect(button("New folder").disabled).toBe(true);

    h.responses.set("environment-one:/workspace", {
      data: { directoryPath: "/workspace", breadcrumbs: [], entries: [] },
      error: null,
      isPending: true,
    });
    await rerender(picker, <RemoteDirectoryPickerDialog {...pickerProps()} />);
    expect(button("New folder").disabled).toBe(true);

    h.responses.set("environment-one:/workspace", {
      data: { directoryPath: "/workspace", breadcrumbs: [], entries: [] },
      error: null,
      isPending: false,
    });
    await rerender(picker, <RemoteDirectoryPickerDialog {...pickerProps()} />);
    expect(button("New folder").disabled).toBe(false);
  });

  it("keeps a closed dialog inert and renders ordinary navigation buttons when opened", async () => {
    const props = pickerProps({ open: false });
    const picker = await mount(<RemoteDirectoryPickerDialog {...props} />);

    expect(h.browseCalls).toEqual([]);
    expect(document.body.querySelector('[role="dialog"]')).toBeNull();

    h.responses.set("environment-one:/workspace", {
      data: {
        parentPath: "/workspace",
        directoryPath: "/workspace",
        entries: [{ name: "child", fullPath: "/workspace/child" }],
      },
      error: null,
      isPending: false,
    });
    await rerender(picker, <RemoteDirectoryPickerDialog {...pickerProps()} />);

    expect(h.browseCalls.at(-1)).toEqual({
      environmentId: "environment-one",
      input: { partialPath: "/workspace", mode: "directory" },
    });
    expect(document.body.querySelector('[role="dialog"]')).not.toBeNull();
    expect(document.body.querySelector('[role="listbox"]')).toBeNull();
    expect(document.body.querySelector('[role="option"]')).toBeNull();
    expect(button("child").getAttribute("type")).toBe("button");
    expect(button("child").querySelector("svg")).not.toBeNull();
    expect(button("child").className).toContain("justify-start");
  });

  it("shows the canonical resolved path without changing the active browse request", async () => {
    h.responses.set("environment-one:~", {
      data: {
        parentPath: "C:\\Users\\mauro",
        directoryPath: "C:\\Users\\mauro",
        ancestorPath: "C:\\Users",
        breadcrumbs: [
          { name: "C:\\", fullPath: "C:\\" },
          { name: "Users", fullPath: "C:\\Users" },
          { name: "mauro", fullPath: "C:\\Users\\mauro" },
        ],
        entries: [],
      },
      error: null,
      isPending: false,
    });

    await mount(<RemoteDirectoryPickerDialog {...pickerProps({ initialPath: "~" })} />);

    expect(h.browseCalls.at(-1)).toEqual({
      environmentId: "environment-one",
      input: { partialPath: "~", mode: "directory" },
    });
    expect(
      document.body.querySelector<HTMLInputElement>('input[aria-label="Server directory path"]')
        ?.value,
    ).toBe("C:\\Users\\mauro");
    expect(document.body.querySelectorAll("[data-directory-breadcrumb-separator]")).toHaveLength(2);
  });

  it("keeps dot-directories collapsed until the hidden-folders control is used", async () => {
    h.responses.set("environment-one:/workspace", {
      data: {
        parentPath: "/workspace",
        directoryPath: "/workspace",
        entries: [
          { name: ".git", fullPath: "/workspace/.git" },
          { name: "packages", fullPath: "/workspace/packages" },
        ],
      },
      error: null,
      isPending: false,
    });
    await mount(<RemoteDirectoryPickerDialog {...pickerProps()} />);

    expect(() => button(".git")).toThrow("Missing button: .git");
    expect(button("packages")).toBeTruthy();

    await act(async () => button("Show hidden folders").click());
    expect(button(".git")).toBeTruthy();
    expect(button("Hide hidden folders")).toBeTruthy();
  });

  it("keeps stale directory data disabled while path and environment requests change", async () => {
    h.responses.set("environment-one:/workspace", {
      data: {
        parentPath: "/workspace",
        directoryPath: "/canonical/one",
        entries: [{ name: "next", fullPath: "/workspace/next" }],
      },
      error: null,
      isPending: false,
    });
    const onSelect = vi.fn();
    const picker = await mount(<RemoteDirectoryPickerDialog {...pickerProps({ onSelect })} />);

    await act(async () => button("next").click());
    expect(h.browseCalls.at(-1)).toEqual({
      environmentId: "environment-one",
      input: { partialPath: "/workspace/next", mode: "directory" },
    });
    expect(button("Select folder").disabled).toBe(true);

    await rerender(
      picker,
      <RemoteDirectoryPickerDialog
        {...pickerProps({
          environmentId: EnvironmentId.make("environment-two"),
          initialPath: "/remote",
          onSelect,
        })}
      />,
    );
    expect(h.browseCalls.at(-1)).toEqual({
      environmentId: "environment-two",
      input: { partialPath: "/remote", mode: "directory" },
    });
    expect(button("Select folder").disabled).toBe(true);

    h.responses.set("environment-two:/remote", {
      data: { parentPath: "/remote", directoryPath: "/canonical/two", entries: [] },
      error: null,
      isPending: false,
    });
    await rerender(
      picker,
      <RemoteDirectoryPickerDialog
        {...pickerProps({
          environmentId: EnvironmentId.make("environment-two"),
          initialPath: "/remote",
          onSelect,
        })}
      />,
    );
    await act(async () => button("Select folder").click());

    expect(onSelect).toHaveBeenCalledWith("/canonical/two");
    expect(onSelect).not.toHaveBeenCalledWith("/canonical/one");
  });
});
