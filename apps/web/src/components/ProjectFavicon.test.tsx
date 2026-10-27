// @vitest-environment happy-dom

import { EnvironmentId } from "@coddy/contracts";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vite-plus/test";

const harness = vi.hoisted(() => ({ src: null as string | null }));

vi.mock("../assets/assetUrls", () => ({
  useAssetUrl: () => harness.src,
}));

import { ProjectFavicon } from "./ProjectFavicon";

let container: HTMLDivElement;
let root: Root;

beforeAll(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

afterAll(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

beforeEach(() => {
  harness.src = null;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

async function render(withClassName = true): Promise<void> {
  await act(async () =>
    root.render(
      <ProjectFavicon
        environmentId={EnvironmentId.make("environment-1")}
        cwd="/repo"
        {...(withClassName ? { className: "custom-icon" } : {})}
      />,
    ),
  );
}

describe("ProjectFavicon", () => {
  it("renders a folder when no asset URL exists", async () => {
    await render(false);
    expect(container.querySelector("svg")?.getAttribute("class")).not.toContain("undefined");
    await render();
    expect(container.querySelector("svg")?.getAttribute("class")).toContain("custom-icon");
    expect(container.querySelector("img")).toBeNull();
  });

  it("shows a fallback while loading and reveals a loaded image", async () => {
    harness.src = "https://assets.test/one.png";
    await render();
    const image = container.querySelector("img")!;
    expect(image.className).toContain("hidden");
    expect(container.querySelector("svg")).not.toBeNull();

    await act(async () => image.dispatchEvent(new Event("load")));
    expect(container.querySelector("img")?.className).not.toContain("hidden");
    expect(container.querySelector("svg")).toBeNull();
  });

  it("keeps the fallback after an image error and remembers loaded URLs", async () => {
    harness.src = "https://assets.test/two.png";
    await render();
    await act(async () => container.querySelector("img")?.dispatchEvent(new Event("error")));
    expect(container.querySelector("svg")).not.toBeNull();

    harness.src = "https://assets.test/one.png";
    await render();
    expect(container.querySelector("img")?.className).not.toContain("hidden");
  });
});
