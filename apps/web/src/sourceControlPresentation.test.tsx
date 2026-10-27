import type { SourceControlProviderInfo } from "@coddy/contracts";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vite-plus/test";

import { getSourceControlPresentation } from "./sourceControlPresentation";

function provider(kind: SourceControlProviderInfo["kind"], name = "Custom host") {
  return { kind, name, baseUrl: "https://example.test" } as SourceControlProviderInfo;
}

describe("getSourceControlPresentation", () => {
  it.each([
    ["github", "GitHub"],
    ["gitlab", "GitLab"],
    ["azure-devops", "Azure DevOps"],
    ["bitbucket", "Bitbucket"],
  ] as const)("presents %s with its provider icon", (kind, fallbackName) => {
    const presentation = getSourceControlPresentation(provider(kind, ""));

    expect(presentation.providerName).toBe(fallbackName);
    expect(presentation.showProviderIcon).toBe(true);
    expect(renderToStaticMarkup(<presentation.Icon />)).toContain("svg");
  });

  it("prefers a custom provider name", () => {
    expect(getSourceControlPresentation(provider("github")).providerName).toBe("Custom host");
  });

  it("uses the default and generic change-request presentations", () => {
    const absent = getSourceControlPresentation(null);
    const unknown = getSourceControlPresentation(provider("unknown"));

    expect(absent.showProviderIcon).toBe(true);
    expect(absent.providerName).toBe("GitHub");
    expect(unknown.showProviderIcon).toBe(false);
    expect(unknown.providerName).toBe("Custom host");
    expect(renderToStaticMarkup(<absent.Icon />)).toContain("svg");
  });
});
