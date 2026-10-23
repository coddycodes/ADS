import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vite-plus/test";

vi.mock("../ui/tooltip", () => ({
  Tooltip: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  TooltipTrigger: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  TooltipPopup: ({ children }: { children: React.ReactNode }) => <section>{children}</section>,
}));

import { ThreadErrorBanner } from "./ThreadErrorBanner";

describe("ThreadErrorBanner", () => {
  it("offers its recovery action next to dismiss", () => {
    const markup = renderToStaticMarkup(
      <ThreadErrorBanner
        error="Claude's context window is full."
        attribution="Claude ran out of context"
        action={{ label: "Compact conversation", onClick: () => {}, disabledReason: null }}
        onDismiss={() => {}}
      />,
    );
    expect(markup).toContain("Compact conversation");
    expect(markup).toContain('aria-label="Dismiss error"');
    expect(markup).not.toMatch(/<button[^>]* disabled=""/);
    expect(markup).not.toContain("title=");
  });

  it("explains why its action cannot run yet", () => {
    const markup = renderToStaticMarkup(
      <ThreadErrorBanner
        error="Claude's context window is full."
        action={{
          label: "Compact conversation",
          onClick: () => {},
          disabledReason: "Wait for the current work to finish, then compact.",
        }}
      />,
    );
    expect(markup).toContain('title="Wait for the current work to finish, then compact."');
    expect(markup).toMatch(/<button[^>]* disabled=""/);
  });

  it("shows no action without one", () => {
    const markup = renderToStaticMarkup(<ThreadErrorBanner error="Something failed." />);
    expect(markup).not.toContain("<button");
  });
});
