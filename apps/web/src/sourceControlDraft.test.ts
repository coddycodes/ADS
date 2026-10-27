// @vitest-environment happy-dom

import { act, createElement, Fragment } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vite-plus/test";
import { createJSONStorage } from "zustand/middleware";

import { sourceControlDraftKey, useSourceControlDraft } from "./sourceControlDraft";
import { useSourceControlPanelStore } from "./sourceControlPanelStore";

const STORAGE_KEY = "coddy-studio:source-control-panel-state:v1";
const persisted = new Map<string, string>();
const storage = {
  getItem: (name: string) => persisted.get(name) ?? null,
  setItem: (name: string, value: string) => {
    persisted.set(name, value);
  },
  removeItem: (name: string) => {
    persisted.delete(name);
  },
};
let container: HTMLDivElement;
let root: Root | null;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  persisted.clear();
  useSourceControlPanelStore.persist.setOptions({ storage: createJSONStorage(() => storage) });
  useSourceControlPanelStore.setState({ byThreadKey: {}, byCwdKey: {} });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root?.unmount());
  root = null;
  container.remove();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("sourceControlDraftKey", () => {
  it("keys a draft by environment and cwd, so ids cannot collide across environments", () => {
    const a = sourceControlDraftKey({ environmentId: "env-a", cwd: "/repo" });
    const b = sourceControlDraftKey({ environmentId: "env-b", cwd: "/repo" });
    expect(a).not.toEqual(b);
    expect(sourceControlDraftKey({ environmentId: "env-a", cwd: "/repo/" })).toEqual(a);
  });
});

describe("source control draft persistence", () => {
  it("migrates a v1 thread-keyed draft without losing the half-written message", async () => {
    persisted.set(
      STORAGE_KEY,
      JSON.stringify({
        version: 1,
        state: { byThreadKey: { "env-a:thread-a": { message: "Keep this draft" } } },
      }),
    );

    await useSourceControlPanelStore.persist.rehydrate();

    expect(useSourceControlPanelStore.getState().byThreadKey).toEqual({
      "env-a:thread-a": { message: "Keep this draft" },
    });
    expect(useSourceControlPanelStore.getState().byCwdKey).toEqual({});

    useSourceControlPanelStore.getState().promoteThreadDraft("env-a:thread-a", "env-a::/repo");
    expect(useSourceControlPanelStore.getState().byCwdKey).toEqual({
      "env-a::/repo": { message: "Keep this draft" },
    });
    expect(useSourceControlPanelStore.getState().byThreadKey).toEqual({});
  });

  it("round-trips v2 thread and checkout drafts", async () => {
    const store = useSourceControlPanelStore.getState();
    store.setCwdMessage("env-a::/repo", "Shared checkout draft");

    const serialized = persisted.get(STORAGE_KEY);
    expect(serialized).toBeDefined();
    expect(JSON.parse(serialized!)).toMatchObject({
      version: 2,
      state: { byCwdKey: { "env-a::/repo": { message: "Shared checkout draft" } } },
    });

    useSourceControlPanelStore.setState({ byThreadKey: {}, byCwdKey: {} });
    persisted.set(STORAGE_KEY, serialized!);
    await useSourceControlPanelStore.persist.rehydrate();

    expect(useSourceControlPanelStore.getState().byCwdKey).toEqual({
      "env-a::/repo": { message: "Shared checkout draft" },
    });
  });

  it("clears only the requested checkout draft", () => {
    const store = useSourceControlPanelStore.getState();
    store.setCwdMessage("env-a::/one", "One");
    store.setCwdMessage("env-a::/two", "Two");

    useSourceControlPanelStore.getState().clearCwdDraft("env-a::/one");

    expect(useSourceControlPanelStore.getState().byCwdKey).toEqual({
      "env-a::/two": { message: "Two" },
    });
  });
});

function DraftWriter() {
  const draft = useSourceControlDraft({ environmentId: "env-a", cwd: "/repo" });
  return createElement(
    "button",
    { type: "button", onClick: () => draft.setMessage("Typed from writer") },
    "Write",
  );
}

function DraftReader() {
  const draft = useSourceControlDraft({ environmentId: "env-a", cwd: "/repo/" });
  return createElement("output", null, draft.message);
}

describe("useSourceControlDraft", () => {
  it("shares a message between two consumers of the same checkout", async () => {
    await act(async () =>
      root?.render(
        createElement(Fragment, null, createElement(DraftWriter), createElement(DraftReader)),
      ),
    );

    await act(async () => container.querySelector("button")?.click());

    expect(container.querySelector("output")?.textContent).toBe("Typed from writer");
  });
});
