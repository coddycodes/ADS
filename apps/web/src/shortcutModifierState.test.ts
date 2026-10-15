// @vitest-environment happy-dom

import { act, createElement, useEffect } from "react";
import { createRoot } from "react-dom/client";
import { describe, expect, it } from "vite-plus/test";

import {
  areShortcutModifierStatesEqual,
  shortcutModifierStateAfterKeyboardEvent,
  type ShortcutModifierState,
  useShortcutModifierState,
} from "./shortcutModifierState";

const emptyState = (): ShortcutModifierState => ({
  metaKey: false,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
});

function keyboardEventLike(type: "keydown" | "keyup", init: Partial<KeyboardEvent>): KeyboardEvent {
  return {
    type,
    key: "",
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    ...init,
  } as KeyboardEvent;
}

describe("shortcutModifierState", () => {
  it("compares modifier states by value", () => {
    expect(
      areShortcutModifierStatesEqual(
        { metaKey: false, ctrlKey: true, altKey: false, shiftKey: true },
        { metaKey: false, ctrlKey: true, altKey: false, shiftKey: true },
      ),
    ).toBe(true);
    expect(
      areShortcutModifierStatesEqual(
        { metaKey: false, ctrlKey: true, altKey: false, shiftKey: true },
        { metaKey: false, ctrlKey: false, altKey: false, shiftKey: true },
      ),
    ).toBe(false);
    expect(
      areShortcutModifierStatesEqual(
        { metaKey: true, ctrlKey: false, altKey: false, shiftKey: false },
        { metaKey: false, ctrlKey: false, altKey: false, shiftKey: false },
      ),
    ).toBe(false);
    expect(
      areShortcutModifierStatesEqual(
        { metaKey: false, ctrlKey: false, altKey: true, shiftKey: false },
        { metaKey: false, ctrlKey: false, altKey: false, shiftKey: false },
      ),
    ).toBe(false);
    expect(
      areShortcutModifierStatesEqual(
        { metaKey: false, ctrlKey: false, altKey: false, shiftKey: true },
        { metaKey: false, ctrlKey: false, altKey: false, shiftKey: false },
      ),
    ).toBe(false);
  });

  it("preserves the current object when modifier values do not change", () => {
    const initialState = emptyState();
    const nextState = shortcutModifierStateAfterKeyboardEvent(
      initialState,
      keyboardEventLike("keyup", { key: "Shift" }),
    );
    expect(nextState).toBe(initialState);
  });

  it("tracks bare modifier keydown and keyup events explicitly", () => {
    let state = emptyState();
    state = shortcutModifierStateAfterKeyboardEvent(
      state,
      keyboardEventLike("keydown", {
        key: "Meta",
        metaKey: false,
      }),
    );
    expect(state).toEqual({
      metaKey: true,
      ctrlKey: false,
      altKey: false,
      shiftKey: false,
    });

    state = shortcutModifierStateAfterKeyboardEvent(
      state,
      keyboardEventLike("keydown", {
        key: "Shift",
        metaKey: true,
        shiftKey: false,
      }),
    );
    expect(state).toEqual({
      metaKey: true,
      ctrlKey: false,
      altKey: false,
      shiftKey: true,
    });

    state = shortcutModifierStateAfterKeyboardEvent(
      state,
      keyboardEventLike("keyup", {
        key: "Meta",
        metaKey: true,
        shiftKey: true,
      }),
    );
    expect(state).toEqual({
      metaKey: false,
      ctrlKey: false,
      altKey: false,
      shiftKey: true,
    });

    state = shortcutModifierStateAfterKeyboardEvent(
      state,
      keyboardEventLike("keyup", {
        key: "Shift",
        shiftKey: true,
      }),
    );
    expect(state).toEqual({
      metaKey: false,
      ctrlKey: false,
      altKey: false,
      shiftKey: false,
    });
  });

  it.each([
    ["OS", "metaKey"],
    ["Command", "metaKey"],
    ["Control", "ctrlKey"],
    ["Alt", "altKey"],
    ["Option", "altKey"],
  ] as const)("normalizes the %s modifier alias", (key, property) => {
    const state = shortcutModifierStateAfterKeyboardEvent(
      emptyState(),
      keyboardEventLike("keydown", { key }),
    );
    expect(state[property]).toBe(true);
  });

  it("reads modifier flags from ordinary key events", () => {
    const state = shortcutModifierStateAfterKeyboardEvent(
      emptyState(),
      keyboardEventLike("keydown", {
        key: "a",
        metaKey: true,
        ctrlKey: true,
        altKey: true,
        shiftKey: true,
      }),
    );
    expect(state).toEqual({
      metaKey: true,
      ctrlKey: true,
      altKey: true,
      shiftKey: true,
    });
  });

  it("preserves an empty hook state on blur and clears active modifiers", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    let latestState: ShortcutModifierState | null = null;
    const Probe = () => {
      const state = useShortcutModifierState();
      useEffect(() => {
        latestState = state;
      });
      return null;
    };

    try {
      await act(async () => root.render(createElement(Probe)));
      const initialState = latestState;
      await act(async () => window.dispatchEvent(new Event("blur")));
      expect(latestState).toBe(initialState);

      await act(async () =>
        window.dispatchEvent(new KeyboardEvent("keydown", { key: "Meta", bubbles: true })),
      );
      expect(latestState).toMatchObject({ metaKey: true });

      await act(async () => window.dispatchEvent(new Event("blur")));
      expect(latestState).toEqual(emptyState());
    } finally {
      await act(async () => root.unmount());
      container.remove();
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });
});
