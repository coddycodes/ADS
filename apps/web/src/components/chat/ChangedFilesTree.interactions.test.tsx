// @vitest-environment happy-dom

import { TurnId } from "@coddy/contracts";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vite-plus/test";

import { ChangedFilesTree } from "./ChangedFilesTree";

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

afterEach(() => {
  document.body.replaceChildren();
});

describe("ChangedFilesTree interactions", () => {
  it("toggles overrides and resets stale overrides when the tree identity changes", async () => {
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    const turnId = TurnId.make("turn-interactive");
    const renderTree = (path: string) => (
      <ChangedFilesTree
        turnId={turnId}
        files={[{ path, kind: "modified", additions: 1, deletions: 0 }]}
        allDirectoriesExpanded={false}
        resolvedTheme="light"
        onOpenTurnDiff={() => {}}
      />
    );

    await act(async () => root.render(renderTree("src/one/file.ts")));
    const firstDirectory = container.querySelector<HTMLButtonElement>("button");
    expect(firstDirectory).not.toBeNull();

    await act(async () => firstDirectory!.click());
    expect(container.textContent).toContain("file.ts");

    await act(async () => firstDirectory!.click());
    expect(container.textContent).not.toContain("file.ts");

    await act(async () => root.render(renderTree("packages/two/index.ts")));
    expect(container.textContent).not.toContain("index.ts");
    const replacementDirectory = container.querySelector<HTMLButtonElement>("button");
    await act(async () => replacementDirectory!.click());
    expect(container.textContent).toContain("index.ts");

    await act(async () => root.unmount());
  });
});
