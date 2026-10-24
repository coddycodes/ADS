// @vitest-environment happy-dom

import { EditorId, EnvironmentId } from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import { AsyncResult } from "effect/reactivity";
import { act, type ReactElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  openInEditor: vi.fn(),
  results: [] as unknown[],
  storedEditor: null as EditorId | null,
}));

vi.mock("./hooks/useLocalStorage", () => ({
  getLocalStorageItem: () => h.storedEditor,
  setLocalStorageItem: (_key: string, value: EditorId) => {
    h.storedEditor = value;
  },
  useLocalStorage: () => [
    h.storedEditor,
    (value: EditorId | null) => {
      h.storedEditor = value;
    },
  ],
}));

vi.mock("./state/shell", () => ({
  shellEnvironment: { openInEditor: "open-in-editor" },
}));

vi.mock("./state/use-atom-command", () => ({
  useAtomCommand: () => h.openInEditor,
}));

import { useOpenInPreferredEditor, usePreferredEditor } from "./editorPreferences";

const mounted: Array<{ root: Root; container: HTMLDivElement }> = [];

async function mount(element: ReactElement): Promise<HTMLDivElement> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  mounted.push({ root, container });
  await act(async () => root.render(element));
  return container;
}

function Harness({
  environmentId,
  editors,
}: {
  readonly environmentId: EnvironmentId | null;
  readonly editors: readonly EditorId[];
}) {
  const [preferredEditor] = usePreferredEditor(editors);
  const open = useOpenInPreferredEditor(environmentId, editors);
  return (
    <>
      <output data-testid="preferred">{preferredEditor ?? "none"}</output>
      <button
        type="button"
        onClick={() => {
          void open("/repo").then((result) => h.results.push(result));
        }}
      >
        Open
      </button>
    </>
  );
}

async function clickOpen(container: HTMLElement): Promise<unknown> {
  await act(async () => {
    container.querySelector<HTMLButtonElement>("button")?.click();
    await Promise.resolve();
    await Promise.resolve();
  });
  return h.results.at(-1);
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.results = [];
  h.storedEditor = null;
  h.openInEditor.mockReset().mockResolvedValue(AsyncResult.success(undefined));
});

afterEach(async () => {
  for (const item of mounted.splice(0)) {
    await act(async () => item.root.unmount());
    item.container.remove();
  }
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("editor preference hooks", () => {
  it("prefers a stored available editor and falls back to configured priority", async () => {
    h.storedEditor = EditorId.make("vscode");
    let container = await mount(
      <Harness
        environmentId={EnvironmentId.make("local")}
        editors={[EditorId.make("cursor"), EditorId.make("vscode")]}
      />,
    );
    expect(container.querySelector('[data-testid="preferred"]')?.textContent).toBe("vscode");

    h.storedEditor = EditorId.make("vscode");
    container = await mount(
      <Harness
        environmentId={EnvironmentId.make("local")}
        editors={[EditorId.make("file-manager"), EditorId.make("cursor")]}
      />,
    );
    expect(container.querySelector('[data-testid="preferred"]')?.textContent).toBe("cursor");

    container = await mount(<Harness environmentId={EnvironmentId.make("local")} editors={[]} />);
    expect(container.querySelector('[data-testid="preferred"]')?.textContent).toBe("none");
  });

  it("returns typed failures when the environment or editor is unavailable", async () => {
    let container = await mount(
      <Harness environmentId={null} editors={[EditorId.make("vscode")]} />,
    );
    let result = (await clickOpen(container)) as { _tag: string; cause: Cause.Cause<unknown> };
    expect(result._tag).toBe("Failure");
    expect(Cause.squash(result.cause)).toMatchObject({
      _tag: "PreferredEditorEnvironmentRequiredError",
    });

    container = await mount(<Harness environmentId={EnvironmentId.make("local")} editors={[]} />);
    result = (await clickOpen(container)) as typeof result;
    expect(result._tag).toBe("Failure");
    expect(Cause.squash(result.cause)).toMatchObject({ _tag: "PreferredEditorUnavailableError" });
    expect(h.openInEditor).not.toHaveBeenCalled();
  });

  it("opens with the resolved editor and preserves command failures", async () => {
    const container = await mount(
      <Harness environmentId={EnvironmentId.make("local")} editors={[EditorId.make("cursor")]} />,
    );
    const success = (await clickOpen(container)) as { _tag: string; value?: unknown };
    expect(success).toMatchObject({ _tag: "Success", value: "cursor" });
    expect(h.openInEditor).toHaveBeenCalledWith({
      environmentId: EnvironmentId.make("local"),
      input: { cwd: "/repo", editor: EditorId.make("cursor") },
    });

    const failureCause = Cause.fail(new Error("editor crashed"));
    h.openInEditor.mockResolvedValueOnce(AsyncResult.failure(failureCause));
    const failure = (await clickOpen(container)) as { _tag: string; cause?: unknown };
    expect(failure).toMatchObject({ _tag: "Failure", cause: failureCause });
  });
});
