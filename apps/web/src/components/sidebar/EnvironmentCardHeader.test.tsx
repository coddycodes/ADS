import { EnvironmentId } from "@coddy/contracts";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vite-plus/test";

import { EnvironmentCardHeader } from "./EnvironmentCardHeader";

describe("EnvironmentCardHeader", () => {
  it("names the environment, its state and the checkout path", () => {
    const markup = renderToStaticMarkup(
      <EnvironmentCardHeader
        identity={{
          environmentId: EnvironmentId.make("env-ai"),
          label: "ai-server",
          isPrimary: false,
          isLocal: false,
          avatar: "AS",
          status: "connected",
          statusLabel: "Connected",
          available: true,
        }}
        workspaceRoot="/work/tripunkt/pathfinder-application-server"
      />,
    );
    expect(markup).toContain('data-testid="environment-card-header-env-ai"');
    expect(markup).toContain("ai-server");
    expect(markup).toContain("Connected");
    expect(markup).toContain("AS");
    expect(markup).toContain("/work/tripunkt/pathfinder-application-server");
  });

  it("shows the monitor icon instead of avatar text for the local environment", () => {
    const markup = renderToStaticMarkup(
      <EnvironmentCardHeader
        identity={{
          environmentId: EnvironmentId.make("env-local"),
          label: "Local",
          isPrimary: true,
          isLocal: true,
          avatar: "LO",
          status: "connected",
          statusLabel: "Connected",
          available: true,
        }}
        workspaceRoot="/work/app"
      />,
    );
    expect(markup).toContain("<svg");
    expect(markup).not.toContain(">LO<");
  });
});
