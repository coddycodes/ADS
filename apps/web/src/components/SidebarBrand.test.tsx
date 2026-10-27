import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vite-plus/test";

import { SidebarBrandContent } from "./Sidebar";

describe("SidebarBrandContent", () => {
  it("renders the configured app base name and stage", () => {
    const markup = renderToStaticMarkup(
      <SidebarBrandContent appBaseName="Coddy Studio" stageLabel="Dev" />,
    );

    expect(markup).toContain(">Coddy Studio<");
    expect(markup).toContain(">Dev<");
  });

  it("omits the stable release stage", () => {
    const markup = renderToStaticMarkup(
      <SidebarBrandContent appBaseName="Coddy Studio" stageLabel={null} />,
    );

    expect(markup).toContain(">Coddy Studio<");
    expect(markup).not.toContain("sidebar-brand-stage");
  });
});
