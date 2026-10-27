// @vitest-environment happy-dom
import { afterEach, expect, it, vi } from "vite-plus/test";
import { installDesktopUiMotionGuard, setDesktopUiMotionMode } from "./motion-guard.ts";

afterEach(() => {
  document.body.replaceChildren();
  document
    .querySelectorAll("style[data-coddy-studio-desktop-ui-automation]")
    .forEach((style) => style.remove());
  Reflect.deleteProperty(document.documentElement.dataset, "coddyStudioDesktopUiMotion");
  vi.unstubAllGlobals();
});

it("installs once per document and lets explicit native-motion checks retain real transitions", async () => {
  vi.stubGlobal("browser", {
    execute: async (callback: (value?: string) => unknown, value?: string) => callback(value),
  });
  const element = document.createElement("div");
  element.style.transitionDuration = "1s";
  element.style.transitionProperty = "opacity";
  document.body.append(element);
  await installDesktopUiMotionGuard();
  await installDesktopUiMotionGuard();
  expect(document.querySelectorAll("style[data-coddy-studio-desktop-ui-automation]")).toHaveLength(1);
  expect(getComputedStyle(element).transitionDuration).toBe("0s");
  await setDesktopUiMotionMode("native");
  expect(document.documentElement.dataset.coddyStudioDesktopUiMotion).toBe("native");
  expect(
    document.querySelectorAll('html:not([data-coddy-studio-desktop-ui-motion="native"]) div'),
  ).toHaveLength(0);
  // Use a fresh probe to avoid happy-dom's cached ancestor-selector styles.
  // The native acceptance suite checks live computed transition changes.
  const nativeProbe = element.cloneNode(true) as HTMLElement;
  document.body.append(nativeProbe);
  expect(getComputedStyle(nativeProbe).transitionDuration).toBe("1s");
  await setDesktopUiMotionMode("disabled");
  const disabledProbe = element.cloneNode(true) as HTMLElement;
  document.body.append(disabledProbe);
  expect(getComputedStyle(disabledProbe).transitionDuration).toBe("0s");
  expect(element.style.transitionDuration).toBe("1s");
});
