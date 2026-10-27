import { EditorId, EnvironmentId } from "@coddy/contracts";
import { afterEach, describe, expect, it } from "vite-plus/test";

import {
  PreferredEditorEnvironmentRequiredError,
  PreferredEditorUnavailableError,
  resolveAndPersistPreferredEditor,
} from "./editorPreferences";
import { removeLocalStorageItem, setLocalStorageItem } from "./hooks/useLocalStorage";

const storageKey = "coddy-studio:last-editor";

afterEach(() => {
  removeLocalStorageItem(storageKey);
});

describe("preferred editor resolution", () => {
  it("keeps a stored editor when it remains available", () => {
    setLocalStorageItem(storageKey, EditorId.make("vscode"), EditorId);

    expect(
      resolveAndPersistPreferredEditor([EditorId.make("cursor"), EditorId.make("vscode")]),
    ).toBe("vscode");
  });

  it("uses editor priority and persists the replacement", () => {
    setLocalStorageItem(storageKey, EditorId.make("vscode"), EditorId);

    expect(
      resolveAndPersistPreferredEditor([EditorId.make("file-manager"), EditorId.make("cursor")]),
    ).toBe("cursor");
    expect(resolveAndPersistPreferredEditor([EditorId.make("cursor")])).toBe("cursor");
  });

  it("returns null when no editor is available", () => {
    expect(resolveAndPersistPreferredEditor([])).toBeNull();
  });
});

describe("preferred editor errors", () => {
  it("explains missing environments and editors", () => {
    expect(new PreferredEditorEnvironmentRequiredError({ targetPath: "/repo" }).message).toBe(
      "Cannot open /repo because no environment is selected.",
    );
    expect(
      new PreferredEditorUnavailableError({
        environmentId: EnvironmentId.make("environment-1"),
        targetPath: "/repo",
        availableEditorIds: [],
      }).message,
    ).toBe("No available editor can open /repo in environment environment-1.");
  });
});
