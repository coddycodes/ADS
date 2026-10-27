import { EnvironmentId } from "@coddy/contracts";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it, vi } from "vite-plus/test";

const { useFileTreeEntries } = vi.hoisted(() => ({
  useFileTreeEntries: vi.fn(),
}));

vi.mock("./projectFilesQueryState", () => ({ useFileTreeEntries }));

import { ProjectFilesPreloader } from "./ProjectFilesPreloader";

it("holds the Files list and its subscription without rendering UI or re-reading", () => {
  const environmentId = EnvironmentId.make("environment-project-files-preloader-test");

  expect(
    renderToStaticMarkup(<ProjectFilesPreloader environmentId={environmentId} cwd="X:/demo" />),
  ).toBe("");
  expect(useFileTreeEntries).toHaveBeenCalledWith(environmentId, "X:/demo", false);
});
