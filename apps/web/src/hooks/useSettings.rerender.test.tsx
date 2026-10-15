// @vitest-environment happy-dom

import { DEFAULT_CLIENT_SETTINGS } from "@bibcode/contracts/settings";
import { act, useLayoutEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const persistence = vi.hoisted(() => ({
  getClientSettings: vi.fn(async () => null),
  setClientSettings: vi.fn(async () => undefined),
}));

vi.mock("~/localApi", () => ({
  ensureLocalApi: () => ({ persistence }),
}));

vi.mock("~/state/server", () => ({
  primaryServerSettingsAtom: {},
  serverEnvironment: {
    settingsValueAtom: vi.fn(),
    updateSettings: {},
  },
}));

vi.mock("~/state/environments", () => ({
  usePrimaryEnvironment: () => null,
}));

vi.mock("~/state/use-atom-command", () => ({
  useAtomCommand: () => vi.fn(),
}));

import {
  __resetClientSettingsPersistenceForTests,
  __setClientSettingsForTests,
  useClientSettings,
  useUpdateClientSettings,
} from "./useSettings";

let root: Root;
let container: HTMLDivElement;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  __resetClientSettingsPersistenceForTests();
  __setClientSettingsForTests(DEFAULT_CLIENT_SETTINGS);
  persistence.getClientSettings.mockClear();
  persistence.setClientSettings.mockClear();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  __resetClientSettingsPersistenceForTests();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("useClientSettings selector subscription", () => {
  it("does not rerender a primitive subscriber for an unrelated settings update", async () => {
    let renderCount = 0;
    let updateSettings: ReturnType<typeof useUpdateClientSettings> | null = null;

    function Probe({ onRender }: { onRender: () => void }) {
      const wordWrap = useClientSettings((settings) => settings.wordWrap);
      const update = useUpdateClientSettings();
      onRender();
      useLayoutEffect(() => {
        updateSettings = update;
      }, [update]);
      return <span>{String(wordWrap)}</span>;
    }

    await act(async () =>
      root.render(
        <Probe
          onRender={() => {
            renderCount += 1;
          }}
        />,
      ),
    );
    expect(renderCount).toBe(1);

    await act(async () => updateSettings?.({ statusBarUsageMode: "compact" }));
    expect(renderCount).toBe(1);

    await act(async () => updateSettings?.({ wordWrap: false }));
    expect(renderCount).toBe(2);
    expect(container.textContent).toBe("false");
  });
});
