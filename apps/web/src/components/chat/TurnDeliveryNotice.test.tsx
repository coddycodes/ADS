import { ProviderDriverKind, type TurnDelivery } from "@coddy/contracts";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vite-plus/test";

import { TurnDeliveryNotice, type TurnDeliveryNoticeProps } from "./TurnDeliveryNotice";

function delivery(
  state: TurnDelivery["state"],
  provider = "claudeAgent",
  detail: string | null = "connection closed before acknowledgement",
): TurnDelivery {
  return {
    state,
    provider: ProviderDriverKind.make(provider),
    ...(detail === null ? {} : { detail }),
  };
}

function renderNotice(
  value: TurnDelivery,
  props: Partial<Pick<TurnDeliveryNoticeProps, "providerLabel" | "waitingBehind">> = {},
): string {
  return renderToStaticMarkup(
    <TurnDeliveryNotice
      delivery={value}
      providerLabel="Claude"
      onRetry={vi.fn()}
      onDismiss={vi.fn()}
      disabled={false}
      {...props}
    />,
  );
}

/** The classes of the paragraph whose text starts with `text`. */
function paragraphClasses(markup: string, text: string): string[] | undefined {
  return markup
    .split("<p")
    .slice(1)
    .find((paragraph) => paragraph.slice(paragraph.indexOf(">") + 1).startsWith(text))
    ?.match(/^[^>]*class="([^"]*)"/u)?.[1]
    ?.split(" ");
}

// The server keeps the refusal as the delivery's detail (ProviderRuntimeError::InvalidOption).
const OPTION_REFUSAL =
  "claudeAgent provider operation failed: option fastMode is not supported by the selected model/session";

describe("TurnDeliveryNotice", () => {
  it("shows a quiet truthful notice after delivery in a new conversation", () => {
    const markup = renderNotice({
      ...delivery("delivered", "claudeAgent", null),
      reason: "startedNewConversation",
    });
    expect(markup).toContain("Sent in a new conversation with a summary of earlier messages.");
    expect(markup).toContain('role="status"');
    expect(markup).toContain("text-muted-foreground");
    expect(markup).not.toMatch(/text-destructive|text-warning|<button/u);
    expect(renderNotice(delivery("delivered", "claudeAgent", null))).toBe("");
  });

  it.each(["pending", "sending", "failed", "uncertain", "dismissed"] as const)(
    "does not claim a new conversation while delivery is %s",
    (state) => {
      expect(
        renderNotice({
          ...delivery(state),
          reason: "startedNewConversation",
        }),
      ).not.toContain("Sent in a new conversation");
    },
  );

  it("offers only Dismiss and explains how to resend a refused model selection", () => {
    const markup = renderNotice({
      ...delivery("failed", "claudeAgent", OPTION_REFUSAL),
      reason: "modelSelectionRefused",
    });

    expect(markup).toContain("Delivery failed");
    expect(markup).toContain(OPTION_REFUSAL);
    expect(markup).toContain(
      "Sending it again unchanged would fail, and later messages wait behind it. Dismiss it, then send it again with another model or without that option.",
    );
    expect(markup).not.toContain("Retry");
    expect(markup).toContain('aria-label="Dismiss and skip this message"');
    expect(markup.match(/<button[\s>]/gu)).toHaveLength(1);
  });

  it.each([
    ["failed", "Codex Personal did not receive this message"],
    ["uncertain", "Codex Personal may have received this message"],
  ] as const)("names the routed instance in a %s notice", (state, expected) => {
    expect(renderNotice(delivery(state, "codex"), { providerLabel: "Codex Personal" })).toContain(
      expected,
    );
  });

  it.each([
    [true, "Waiting for an earlier message. Retry or dismiss it to send this one."],
    [false, "Waiting for an earlier message. Dismiss it to send this one."],
  ] as const)("shows a muted waiting line when the blocker offersRetry=%s", (offersRetry, text) => {
    const markup = renderNotice(delivery("pending"), { waitingBehind: { offersRetry } });

    expect(markup).toContain(text);
    expect(markup).toContain('role="status"');
    expect(paragraphClasses(markup, text)).toContain("text-muted-foreground");
    expect(markup).not.toMatch(/<button|border-|text-warning|text-destructive/u);
    expect(markup).not.toContain("connection closed before acknowledgement");
  });

  it("shows why a delivery failed directly under the heading", () => {
    const markup = renderNotice(delivery("failed", "claudeAgent", OPTION_REFUSAL));

    expect(markup).toContain(OPTION_REFUSAL);
    expect(markup.indexOf("Delivery failed")).toBeLessThan(markup.indexOf(OPTION_REFUSAL));
    expect(markup.indexOf(OPTION_REFUSAL)).toBeLessThan(
      markup.indexOf("Claude did not receive this message"),
    );
  });

  it("says that later messages wait and what Retry and Dismiss do", () => {
    const markup = renderNotice(delivery("failed", "claudeAgent", OPTION_REFUSAL));

    // A failed delivery holds back the thread's later deliveries until the user acts, and Retry
    // replays the unchanged message, so a refused model option is refused again.
    expect(markup).not.toContain("Retry to send it again");
    expect(markup).toContain(
      "Claude did not receive this message, and later messages wait behind it. Retry sends it again unchanged; Dismiss skips it.",
    );
    expect(markup).toContain('aria-label="Retry message delivery"');
    expect(markup).toContain('aria-label="Dismiss and skip this message"');
  });

  it.each([
    ["failed", "Delivery failed", OPTION_REFUSAL],
    ["uncertain", "Delivery uncertain", "connection closed before acknowledgement"],
  ] as const)(
    "shows the detail muted under a heading that keeps its tone (%s)",
    (state, heading, detail) => {
      const markup = renderNotice(delivery(state, "claudeAgent", detail));

      expect(paragraphClasses(markup, heading)).toBeDefined();
      expect(paragraphClasses(markup, heading)).not.toContain("text-muted-foreground");
      expect(paragraphClasses(markup, detail)).toContain("text-muted-foreground");
    },
  );

  it("shows why a delivery is uncertain", () => {
    const markup = renderNotice(delivery("uncertain"));

    expect(markup).toContain("connection closed before acknowledgement");
    expect(markup.indexOf("Delivery uncertain")).toBeLessThan(
      markup.indexOf("connection closed before acknowledgement"),
    );
  });

  it.each(["failed", "uncertain"] as const)(
    "leaves no empty line for a %s delivery without a detail",
    (state) => {
      const markup = renderNotice(delivery(state, "claudeAgent", null));

      expect(markup).toContain(state === "failed" ? "Delivery failed" : "Delivery uncertain");
      expect(markup).not.toMatch(/<p[^>]*><\/p>/u);
      expect(markup.match(/<p[\s>]/gu)).toHaveLength(2);
    },
  );

  it("explains uncertain delivery with provider-specific safe actions", () => {
    const markup = renderToStaticMarkup(
      <TurnDeliveryNotice
        delivery={delivery("uncertain")}
        providerLabel="Claude"
        onRetry={vi.fn()}
        onDismiss={vi.fn()}
        disabled={false}
      />,
    );

    expect(markup).toContain("Delivery uncertain");
    expect(markup).toContain("Claude may have received this message");
    // An uncertain delivery holds back later ones too, so the copy says so.
    expect(markup).toContain("later messages wait behind it");
    expect(markup).toContain("Retrying could deliver a duplicate; Dismiss skips it.");
    expect(markup).toContain('aria-label="Retry message delivery"');
    expect(markup).toContain('aria-label="Dismiss and skip this message"');
  });

  it("explains failed delivery and disables both actions together", () => {
    const markup = renderToStaticMarkup(
      <TurnDeliveryNotice
        delivery={delivery("failed", "opencode")}
        providerLabel="OpenCode"
        onRetry={vi.fn()}
        onDismiss={vi.fn()}
        disabled
      />,
    );

    expect(markup).toContain("Delivery failed");
    expect(markup).toContain("OpenCode did not receive this message");
    expect(markup.match(/disabled=""/gu)).toHaveLength(2);
  });

  it.each(["pending", "sending", "delivered", "dismissed"] as const)(
    "renders nothing for %s delivery",
    (state) => {
      expect(
        renderToStaticMarkup(
          <TurnDeliveryNotice
            delivery={delivery(state)}
            providerLabel="Claude"
            onRetry={vi.fn()}
            onDismiss={vi.fn()}
            disabled={false}
          />,
        ),
      ).toBe("");
    },
  );
});
