import { ProviderDriverKind } from "@bibcode/contracts";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vite-plus/test";

vi.mock("./providerIconUtils", () => ({
  PROVIDER_ICON_BY_PROVIDER: {
    codex: ({ className }: { className?: string }) => <svg className={className} data-codex-icon />,
  },
}));

import { ProviderInstanceIcon, providerInstanceInitials } from "./ProviderInstanceIcon";

describe("providerInstanceInitials", () => {
  it.each([
    ["", ""],
    ["   ", ""],
    ["codex", "CO"],
    ["a", "A"],
    ["claude-agent", "CA"],
    ["open_code provider", "OC"],
    ["one two three", "OT"],
  ])("formats %j as %j", (label, expected) => {
    expect(providerInstanceInitials(label)).toBe(expected);
  });

  it("defensively handles an empty word in the initials mapper", () => {
    const filter = vi.spyOn(Array.prototype, "filter").mockImplementation(function passthrough(
      this: unknown[],
    ) {
      return this;
    });
    const initials = providerInstanceInitials(" one");
    filter.mockRestore();
    expect(initials).toBe("O");
  });
});

describe("ProviderInstanceIcon", () => {
  it("keeps readable initials in flow beside the compact provider logo", () => {
    const markup = renderToStaticMarkup(
      <ProviderInstanceIcon
        driverKind={ProviderDriverKind.make("codex")}
        displayName="Codex Agent"
        showBadge
        className="h-5"
        iconClassName="size-4"
      />,
    );
    expect(markup).toContain("data-codex-icon");
    const badge = markup.match(/<span class="([^"]+)"[^>]*>CA<\/span>/)?.[1];
    expect(badge).toBeDefined();
    expect(badge?.split(" ")).not.toContain("absolute");
    expect(badge?.split(" ")).toContain("shrink-0");
    expect(badge?.split(" ")).toContain("text-xs");
  });

  it("renders a known provider icon without indicators", () => {
    const markup = renderToStaticMarkup(
      <ProviderInstanceIcon
        driverKind={ProviderDriverKind.make("codex")}
        displayName="Codex"
        className="root"
        iconClassName="provider-icon"
      />,
    );
    expect(markup).toContain("data-codex-icon");
    expect(markup).toContain("provider-icon");
    expect(markup).not.toContain("box-shadow");
  });

  it("falls back to initials for unknown providers", () => {
    const markup = renderToStaticMarkup(
      <ProviderInstanceIcon
        driverKind={ProviderDriverKind.make("custom")}
        displayName="Custom Provider"
      />,
    );
    expect(markup).toContain("CP");
    expect(markup).not.toContain("data-codex-icon");
  });

  it("renders accent badges and custom status-dot backgrounds", () => {
    const markup = renderToStaticMarkup(
      <ProviderInstanceIcon
        driverKind={ProviderDriverKind.make("codex")}
        displayName="Codex Agent"
        accentColor="#123456"
        showBadge
        badgeClassName="badge"
        statusDotClassName="status"
        indicatorBackground="#ffffff"
      />,
    );
    expect(markup).toContain("--provider-accent:#123456");
    expect(markup).toContain("CA");
    expect(markup).toContain("bg-[var(--provider-accent)]");
    expect(markup).toContain("0 0 0 2px #ffffff");
    expect(markup).toContain("border-color:#ffffff");
  });

  it("renders muted badges without text when requested", () => {
    const markup = renderToStaticMarkup(
      <ProviderInstanceIcon
        driverKind={ProviderDriverKind.make("codex")}
        displayName="Codex"
        showBadge
        badgeContent="none"
        statusDotClassName="status"
      />,
    );
    expect(markup).toContain("bg-muted");
    expect(markup).toContain("var(--card)");
    expect(markup).not.toContain(">CO<");
  });
});
