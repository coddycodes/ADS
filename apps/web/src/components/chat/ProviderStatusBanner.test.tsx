import type { ServerProvider } from "@coddy/contracts";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vite-plus/test";

vi.mock("../ui/tooltip", () => ({
  Tooltip: ({ children }: { children?: React.ReactNode }) => <>{children}</>,
  TooltipTrigger: ({ render }: { render?: React.ReactNode }) => <>{render}</>,
  TooltipPopup: ({ children }: { children?: React.ReactNode }) => <span>{children}</span>,
}));

import { ProviderStatusBanner } from "./ProviderStatusBanner";

function provider(overrides: Partial<ServerProvider> = {}): ServerProvider {
  return {
    driver: "codex",
    displayName: "Codex Work",
    status: "warning",
    auth: { status: "unknown" },
    ...overrides,
  } as ServerProvider;
}

describe("ProviderStatusBanner", () => {
  it("hides absent, ready, and disabled status", () => {
    expect(renderToStaticMarkup(<ProviderStatusBanner status={null} />)).toBe("");
    expect(
      renderToStaticMarkup(<ProviderStatusBanner status={provider({ status: "ready" })} />),
    ).toBe("");
    expect(
      renderToStaticMarkup(<ProviderStatusBanner status={provider({ status: "disabled" })} />),
    ).toBe("");
  });

  it("shows warning detail from the provider", () => {
    const markup = renderToStaticMarkup(
      <ProviderStatusBanner status={provider({ message: "Models are temporarily limited" })} />,
    );

    expect(markup).toContain("Codex Work provider status");
    expect(markup).toContain("Models are temporarily limited");
    expect(markup).toContain("border-warning");
  });

  it("shows generic warning and error detail using the driver label", () => {
    expect(
      renderToStaticMarkup(
        <ProviderStatusBanner status={provider({ displayName: "  ", message: undefined })} />,
      ),
    ).toContain("Codex provider has limited availability.");
    expect(
      renderToStaticMarkup(
        <ProviderStatusBanner status={provider({ status: "error", message: undefined })} />,
      ),
    ).toContain("Codex Work provider is unavailable.");
  });

  it("special-cases unauthenticated errors", () => {
    const markup = renderToStaticMarkup(
      <ProviderStatusBanner
        status={provider({ status: "error", auth: { status: "unauthenticated" } })}
      />,
    );

    expect(markup).toContain("Codex Work is unauthenticated");
    expect(markup).toContain("Sign in via the CLI to authenticate again.");
  });
});
