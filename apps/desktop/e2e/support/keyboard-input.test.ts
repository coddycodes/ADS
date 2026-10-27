// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from "vite-plus/test";

import {
  describeDesktopUiFocus,
  focusDesktopUiElement,
  sendFocusedKeyboardKey,
} from "./keyboard-input.ts";

afterEach(() => {
  document.body.replaceChildren();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function installDroppedKeyTransport(): void {
  vi.stubGlobal("browser", {
    execute: async (
      callback: (...args: ReadonlyArray<unknown>) => unknown,
      ...args: ReadonlyArray<unknown>
    ) => callback(...args),
    keys: vi.fn(async () => undefined),
  });
}

function installKeyTransport(
  send: (target: Element | null, keys: string | ReadonlyArray<string>) => void,
): void {
  vi.stubGlobal("browser", {
    execute: async (
      callback: (...args: ReadonlyArray<unknown>) => unknown,
      ...args: ReadonlyArray<unknown>
    ) => callback(...args),
    keys: vi.fn(async (keys: string | ReadonlyArray<string>) => {
      send(document.activeElement, keys);
    }),
  });
}

function installNativeEnterTransport(): void {
  installKeyTransport((target) => {
    target?.dispatchEvent(
      new KeyboardEvent("keydown", { bubbles: true, cancelable: true, key: "Enter" }),
    );
    if (target instanceof HTMLButtonElement) target.click();
    target?.dispatchEvent(
      new KeyboardEvent("keyup", { bubbles: true, cancelable: true, key: "Enter" }),
    );
  });
}

function expectKeyboardOwnershipReleased(target: HTMLElement): void {
  const globals = window as Window & {
    __coddyStudioDesktopUiKeyboardOwnership?: unknown;
    __coddyStudioDesktopUiKeyCapture?: unknown;
    __coddyStudioDesktopUiKeyTarget?: unknown;
  };
  expect(document.querySelector("[data-coddy-studio-keyboard-probe]")).toBeNull();
  expect(globals.__coddyStudioDesktopUiKeyboardOwnership).toBeUndefined();
  expect(globals.__coddyStudioDesktopUiKeyCapture).toBeUndefined();
  expect(globals.__coddyStudioDesktopUiKeyTarget).toBeUndefined();
  expect(document.activeElement).toBe(target);
}

function failBrowserExecuteCall(call: number, message: string): void {
  const driver = browser as unknown as {
    execute: (
      callback: (...args: ReadonlyArray<unknown>) => unknown,
      ...args: ReadonlyArray<unknown>
    ) => Promise<unknown>;
  };
  const execute = driver.execute.bind(driver);
  let calls = 0;
  driver.execute = vi.fn(async (callback, ...args) => {
    calls += 1;
    if (calls === call) throw new Error(message);
    return execute(callback, ...args);
  });
}

function makeVisible(element: HTMLElement): void {
  vi.spyOn(element, "getBoundingClientRect").mockReturnValue({
    bottom: 10,
    height: 10,
    left: 0,
    right: 10,
    top: 0,
    width: 10,
    x: 0,
    y: 0,
    toJSON: () => ({}),
  });
}

describe("sendFocusedKeyboardKey", () => {
  it("focuses a production selector and returns a narrow active-element description", async () => {
    installDroppedKeyTransport();
    const button = document.createElement("button");
    button.dataset.activityRow = "fixture-actor";
    button.dataset.testid = "fixture-button";
    button.setAttribute("aria-label", "Fixture actor");
    document.body.append(button);

    expect(await focusDesktopUiElement('[data-testid="fixture-button"]')).toEqual({
      activityRow: "fixture-actor",
      activitySection: null,
      ariaLabel: "Fixture actor",
      tagName: "BUTTON",
      testId: "fixture-button",
    });
    expect(await describeDesktopUiFocus()).toEqual({
      activityRow: "fixture-actor",
      activitySection: null,
      ariaLabel: "Fixture actor",
      tagName: "BUTTON",
      testId: "fixture-button",
    });
  });

  it("probes native Enter away from production controls before selecting the synthetic transport", async () => {
    installKeyTransport((target) => {
      target?.dispatchEvent(
        new KeyboardEvent("keydown", { bubbles: true, cancelable: true, key: "Enter" }),
      );
      target?.dispatchEvent(
        new KeyboardEvent("keyup", { bubbles: true, cancelable: true, key: "Enter" }),
      );
    });
    const button = document.createElement("button");
    const onClick = vi.fn();
    const onKeyDown = vi.fn();
    const onKeyUp = vi.fn();
    button.addEventListener("click", onClick);
    button.addEventListener("keydown", onKeyDown);
    button.addEventListener("keyup", onKeyUp);
    document.body.append(button);
    button.focus();

    const result = await sendFocusedKeyboardKey("Enter");

    expect(result.transport).toBe("synthetic");
    expect(result.driver).toEqual({ click: 0, focusChanged: false, keydown: 0, keyup: 0 });
    expect(result.synthetic?.click).toBe(1);
    expect(onKeyDown).toHaveBeenCalledTimes(1);
    expect(onKeyUp).toHaveBeenCalledTimes(1);
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it("dispatches synthetic Enter as keydown, activation, keyup on the original target exactly once", async () => {
    installDroppedKeyTransport();
    const button = document.createElement("button");
    button.type = "button";
    button.setAttribute("aria-label", "Expand activity summary");
    const order: string[] = [];
    button.addEventListener("keydown", () => {
      order.push("keydown");
      document.body.focus();
    });
    button.addEventListener("click", () => order.push("click"));
    button.addEventListener("keyup", () => order.push("keyup"));
    document.body.append(button);
    button.focus();

    const result = await sendFocusedKeyboardKey("Enter");

    expect(result).toEqual({
      transport: "synthetic",
      before: {
        activityRow: null,
        activitySection: null,
        ariaLabel: "Expand activity summary",
        tagName: "BUTTON",
        testId: null,
      },
      after: {
        activityRow: null,
        activitySection: null,
        ariaLabel: null,
        tagName: "BODY",
        testId: null,
      },
      driver: { click: 0, focusChanged: false, keydown: 0, keyup: 0 },
      synthetic: {
        click: 1,
        defaultPrevented: false,
        keydown: 1,
        keyup: 1,
      },
    });
    expect(order).toEqual(["keydown", "click", "keyup"]);
  });

  it("does not emulate Enter activation when the focused button prevents keydown", async () => {
    installDroppedKeyTransport();
    const button = document.createElement("button");
    button.type = "button";
    const onClick = vi.fn();
    button.addEventListener("keydown", (event) => event.preventDefault());
    button.addEventListener("click", onClick);
    document.body.append(button);
    button.focus();

    const result = await sendFocusedKeyboardKey("Enter");

    expect(result.transport).toBe("synthetic");
    expect(result.synthetic).toEqual({
      click: 0,
      defaultPrevented: true,
      keydown: 1,
      keyup: 1,
    });
    expect(onClick).not.toHaveBeenCalled();
  });

  it("moves Tab focus through the real tabbable order when WebDriver drops the key", async () => {
    installDroppedKeyTransport();
    const first = document.createElement("button");
    first.setAttribute("aria-label", "Collapse activity summary");
    const second = document.createElement("button");
    second.dataset.activitySection = "subagents";
    second.setAttribute("aria-label", "Open Subagents");
    makeVisible(first);
    makeVisible(second);
    document.body.append(first, second);
    first.focus();

    const result = await sendFocusedKeyboardKey("Tab");

    expect(result.transport).toBe("synthetic");
    expect(result.before.ariaLabel).toBe("Collapse activity summary");
    expect(result.after).toEqual({
      activityRow: null,
      activitySection: "subagents",
      ariaLabel: "Open Subagents",
      tagName: "BUTTON",
      testId: null,
    });
    expect(document.activeElement).toBe(second);
  });

  it("moves backward from a programmatically focused heading to the preceding control", async () => {
    installDroppedKeyTransport();
    const back = document.createElement("button");
    back.setAttribute("aria-label", "Back to Subagents");
    const heading = document.createElement("h2");
    heading.tabIndex = -1;
    const following = document.createElement("button");
    following.setAttribute("aria-label", "Following control");
    for (const element of [back, heading, following]) makeVisible(element);
    document.body.append(back, heading, following);
    heading.focus();

    const result = await sendFocusedKeyboardKey("Tab", true);

    expect(result.transport).toBe("synthetic");
    expect(result.before.tagName).toBe("H2");
    expect(result.after.ariaLabel).toBe("Back to Subagents");
    expect(document.activeElement).toBe(back);
  });

  it("uses native semantics without duplicate production events when the capability probe succeeds", async () => {
    installKeyTransport((target) => {
      target?.dispatchEvent(
        new KeyboardEvent("keydown", { bubbles: true, cancelable: true, key: "Enter" }),
      );
      if (target instanceof HTMLButtonElement) target.click();
      target?.dispatchEvent(
        new KeyboardEvent("keyup", { bubbles: true, cancelable: true, key: "Enter" }),
      );
    });
    const button = document.createElement("button");
    const events: string[] = [];
    button.addEventListener("keydown", () => events.push("keydown"));
    button.addEventListener("click", () => events.push("click"));
    button.addEventListener("keyup", () => events.push("keyup"));
    document.body.append(button);
    button.focus();

    const result = await sendFocusedKeyboardKey("Enter");

    expect(result.transport).toBe("webdriver");
    expect(result.driver).toEqual({ click: 1, focusChanged: false, keydown: 1, keyup: 1 });
    expect(result.synthetic).toBeNull();
    expect(events).toEqual(["keydown", "click", "keyup"]);
  });

  it("orders positive tabindex before native/contenteditable controls and excludes untabbable nodes", async () => {
    installDroppedKeyTransport();
    const nativeZero = document.createElement("button");
    nativeZero.setAttribute("aria-label", "native zero");
    const positiveTwo = document.createElement("button");
    positiveTwo.tabIndex = 2;
    positiveTwo.setAttribute("aria-label", "positive two");
    const contentEditable = document.createElement("div");
    contentEditable.contentEditable = "true";
    contentEditable.setAttribute("aria-label", "editable zero");
    const positiveOne = document.createElement("button");
    positiveOne.tabIndex = 1;
    positiveOne.setAttribute("aria-label", "positive one");
    const negative = document.createElement("button");
    negative.tabIndex = -1;
    const disabled = document.createElement("button");
    disabled.disabled = true;
    const hidden = document.createElement("button");
    hidden.hidden = true;
    const inertHost = document.createElement("div");
    inertHost.setAttribute("inert", "");
    inertHost.append(document.createElement("button"));
    for (const element of [
      nativeZero,
      positiveTwo,
      contentEditable,
      positiveOne,
      negative,
      disabled,
      hidden,
    ]) {
      makeVisible(element);
    }
    makeVisible(inertHost.firstElementChild as HTMLElement);
    document.body.append(
      nativeZero,
      positiveTwo,
      contentEditable,
      positiveOne,
      negative,
      disabled,
      hidden,
      inertHost,
    );
    positiveOne.focus();

    expect((await sendFocusedKeyboardKey("Tab")).after.ariaLabel).toBe("positive two");
    expect((await sendFocusedKeyboardKey("Tab")).after.ariaLabel).toBe("native zero");
    expect((await sendFocusedKeyboardKey("Tab")).after.ariaLabel).toBe("editable zero");
    expect((await sendFocusedKeyboardKey("Tab")).after.ariaLabel).toBe("positive one");
    expect((await sendFocusedKeyboardKey("Tab", true)).after.ariaLabel).toBe("editable zero");
  });

  it("keeps click evidence target-scoped and cleans its capability probe after transport errors", async () => {
    const button = document.createElement("button");
    document.body.append(button);
    button.focus();
    const unrelated = document.createElement("button");
    document.body.append(unrelated);
    vi.stubGlobal("browser", {
      execute: async (
        callback: (...args: ReadonlyArray<unknown>) => unknown,
        ...args: ReadonlyArray<unknown>
      ) => callback(...args),
      keys: vi.fn(async () => {
        unrelated.click();
        throw new Error("driver unavailable");
      }),
    });

    await expect(sendFocusedKeyboardKey("Enter")).rejects.toThrow("driver unavailable");
    expect(document.querySelector("[data-coddy-studio-keyboard-probe]")).toBeNull();
    expect(document.activeElement).toBe(button);
  });

  it("releases probe ownership when capability inspection fails", async () => {
    installDroppedKeyTransport();
    const button = document.createElement("button");
    document.body.append(button);
    button.focus();
    failBrowserExecuteCall(3, "probe inspection failed");

    await expect(sendFocusedKeyboardKey("Enter")).rejects.toThrow("probe inspection failed");

    expectKeyboardOwnershipReleased(button);
  });

  it("removes partially installed native listeners when native setup fails", async () => {
    installNativeEnterTransport();
    const button = document.createElement("button");
    document.body.append(button);
    button.focus();
    const originalAddEventListener = button.addEventListener.bind(button);
    vi.spyOn(button, "addEventListener").mockImplementation((type, listener, options) => {
      originalAddEventListener(type, listener, options);
      if (type === "keyup") throw new Error("native setup failed");
    });
    const removeEventListener = vi.spyOn(button, "removeEventListener");

    await expect(sendFocusedKeyboardKey("Enter")).rejects.toThrow("native setup failed");

    expect(removeEventListener).toHaveBeenCalledWith("keydown", expect.any(Function), true);
    expect(removeEventListener).toHaveBeenCalledWith("keyup", expect.any(Function), true);
    expectKeyboardOwnershipReleased(button);
  });

  it("removes native listeners when evidence collection fails", async () => {
    installNativeEnterTransport();
    const button = document.createElement("button");
    document.body.append(button);
    button.focus();
    const removeEventListener = vi.spyOn(button, "removeEventListener");
    failBrowserExecuteCall(5, "native evidence failed");

    await expect(sendFocusedKeyboardKey("Enter")).rejects.toThrow("native evidence failed");

    expect(removeEventListener.mock.calls.length).toBeGreaterThan(1);
    expectKeyboardOwnershipReleased(button);
  });

  it("restores original focus when native Tab moves before evidence collection fails", async () => {
    const original = document.createElement("button");
    original.setAttribute("aria-label", "Original");
    const next = document.createElement("button");
    next.setAttribute("aria-label", "Next");
    makeVisible(original);
    makeVisible(next);
    document.body.append(original, next);
    installKeyTransport((target) => {
      target?.dispatchEvent(
        new KeyboardEvent("keydown", { bubbles: true, cancelable: true, key: "Tab" }),
      );
      const probeNext = target?.nextElementSibling;
      if (probeNext instanceof HTMLElement && target?.closest("[data-coddy-studio-keyboard-probe]")) {
        probeNext.focus();
      } else {
        next.focus();
      }
      target?.dispatchEvent(new KeyboardEvent("keyup", { bubbles: true, key: "Tab" }));
    });
    original.focus();
    failBrowserExecuteCall(5, "native Tab evidence failed");

    await expect(sendFocusedKeyboardKey("Tab")).rejects.toThrow("native Tab evidence failed");

    expectKeyboardOwnershipReleased(original);
  });

  it("removes synthetic listeners when fallback execution fails", async () => {
    installDroppedKeyTransport();
    const button = document.createElement("button");
    document.body.append(button);
    button.focus();
    const dispatchEvent = button.dispatchEvent.bind(button);
    vi.spyOn(button, "dispatchEvent").mockImplementation((event) => {
      if (event instanceof KeyboardEvent && event.type === "keydown") {
        throw new Error("synthetic execution failed");
      }
      return dispatchEvent(event);
    });
    const removeEventListener = vi.spyOn(button, "removeEventListener");

    await expect(sendFocusedKeyboardKey("Enter")).rejects.toThrow("synthetic execution failed");

    expect(removeEventListener).toHaveBeenCalledWith("click", expect.any(Function));
    expectKeyboardOwnershipReleased(button);
  });

  it("restores original focus when synthetic Tab moves before keyup fails", async () => {
    installDroppedKeyTransport();
    const original = document.createElement("button");
    original.setAttribute("aria-label", "Original");
    const next = document.createElement("button");
    next.setAttribute("aria-label", "Next");
    makeVisible(original);
    makeVisible(next);
    document.body.append(original, next);
    const dispatchEvent = original.dispatchEvent.bind(original);
    vi.spyOn(original, "dispatchEvent").mockImplementation((event) => {
      if (event instanceof KeyboardEvent && event.type === "keyup") {
        throw new Error("synthetic Tab keyup failed");
      }
      return dispatchEvent(event);
    });
    original.focus();

    await expect(sendFocusedKeyboardKey("Tab")).rejects.toThrow("synthetic Tab keyup failed");

    expectKeyboardOwnershipReleased(original);
  });
});
