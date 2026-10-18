import { afterEach, describe, expect, it, vi } from "vite-plus/test";

import {
  buildHostedChannelSelectionUrl,
  buildHostedPairingUrl,
  hasHostedPairingRequest,
  isHostedPairingApp,
  isHostedStaticApp,
  isServedByThisBackend,
  isServedPrimarySuppressed,
  onServedPrimarySuppressionChange,
  resetServedPrimarySuppressionForTests,
  setServedPrimarySuppressed,
  readHostedPairingRequest,
} from "./hostedPairing";

describe("hostedPairing", () => {
  afterEach(() => {
    vi.unstubAllEnvs();
    vi.unstubAllGlobals();
  });

  it("reads hosted pairing host and query token parameters", () => {
    const url = new URL("https://app.example.test/pair?host=100.64.1.2:3773&token=ABCD1234");

    expect(readHostedPairingRequest(url)).toEqual({
      httpBaseUrl: "https://100.64.1.2:3773/",
      displayHost: "100.64.1.2:3773",
      token: "ABCD1234",
      label: "",
    });
    expect(hasHostedPairingRequest(url)).toBe(true);
  });

  it("rejects misleading userinfo and normalizes IDNs before display", () => {
    for (const host of [
      "https://trusted.example@attacker.example:8080",
      "https://user:password@example.test",
    ]) {
      const url = new URL("https://app.example.test/pair#token=ABCD1234");
      url.searchParams.set("host", host);
      expect(readHostedPairingRequest(url), host).toBeNull();
      expect(hasHostedPairingRequest(url), host).toBe(false);
    }

    const url = new URL("https://app.example.test/pair#token=ABCD1234");
    url.searchParams.set("host", "https://аpple.com");
    expect(readHostedPairingRequest(url)).toEqual({
      httpBaseUrl: "https://xn--pple-43d.com/",
      displayHost: "xn--pple-43d.com",
      token: "ABCD1234",
      label: "",
    });
  });

  it("prefers hash tokens so generated hosted links do not put credentials in search params", () => {
    vi.stubEnv("VITE_HOSTED_APP_URL", "https://preview.example.test");

    const url = new URL(
      buildHostedPairingUrl({
        host: "https://backend.example.com:3773",
        token: "pairing-token",
        label: "Workstation",
      }),
    );

    expect(url.origin).toBe("https://preview.example.test");
    expect(url.pathname).toBe("/pair");
    expect(url.searchParams.get("host")).toBe("https://backend.example.com:3773");
    expect(url.searchParams.get("label")).toBe("Workstation");
    expect(url.searchParams.has("token")).toBe(false);
    expect(url.hash).toBe("#token=pairing-token");
  });

  it("builds hosted channel selection URLs through the configured router origin", () => {
    vi.stubEnv("VITE_HOSTED_APP_URL", "https://app.example.test");

    const url = new URL(
      buildHostedChannelSelectionUrl({
        channel: "nightly",
      }),
    );

    expect(url.origin).toBe("https://app.example.test");
    expect(url.pathname).toBe("/__bibcode/channel");
    expect(url.searchParams.get("channel")).toBe("nightly");
    expect(url.searchParams.has("next")).toBe(false);
  });

  it("ignores incomplete hosted pairing requests", () => {
    expect(
      hasHostedPairingRequest(new URL("https://app.example.test/pair?host=backend.example.com")),
    ).toBe(false);
    expect(hasHostedPairingRequest(new URL("https://app.example.test/pair?token=ABCD1234"))).toBe(
      false,
    );
  });

  it("detects the hosted static app only when no backend URL is configured", () => {
    vi.stubEnv("VITE_HOSTED_APP_URL", "https://preview.example.test");
    vi.stubEnv("VITE_HTTP_URL", "");
    vi.stubEnv("VITE_WS_URL", "");

    expect(isHostedStaticApp(new URL("https://preview.example.test/"))).toBe(true);
    expect(isHostedStaticApp(new URL("https://preview.example.test/pair"))).toBe(true);
    expect(isHostedStaticApp(new URL("https://backend.example.com/"))).toBe(false);

    vi.stubEnv("VITE_HTTP_URL", "https://backend.example.com");
    expect(isHostedStaticApp(new URL("https://preview.example.test/"))).toBe(false);
  });

  describe("on a page a server served", () => {
    const servedPage = (marker: string | null) => {
      vi.stubEnv("VITE_HOSTED_APP_URL", "");
      vi.stubEnv("VITE_HOSTED_APP_CHANNEL", "");
      vi.stubEnv("VITE_HTTP_URL", "");
      vi.stubEnv("VITE_WS_URL", "");
      vi.stubGlobal("window", { location: { origin: "http://box.lan:3773" } });
      vi.stubGlobal("document", {
        querySelector: (selector: string) =>
          selector === 'meta[name="bibcode-served-host"]' && marker !== null
            ? { getAttribute: (name: string) => (name === "content" ? marker : null) }
            : null,
      });
    };
    const url = new URL("http://box.lan:3773/pair");

    // `bibcode serve --mode web` marks the page it serves; that page pairs with its server.
    it("is not the hosted app when the marker names this host", () => {
      servedPage("box.lan:3773");
      expect(isServedByThisBackend(url)).toBe(true);
      expect(isHostedStaticApp(url)).toBe(false);
      // Its own host-based pairing links still land here.
      expect(isHostedPairingApp(url)).toBe(true);
    });

    // A copy of a served document deployed elsewhere names another host.
    it("ignores a marker for another host", () => {
      servedPage("other.lan:3773");
      expect(isServedByThisBackend(url)).toBe(false);
      expect(isHostedStaticApp(url)).toBe(true);
    });

    // Its server is withheld until the root route confirms it is paired, so a primary discovered
    // before that check cannot be offered unpaired.
    it("withholds this server until the root route decides", () => {
      servedPage("box.lan:3773");
      vi.stubGlobal("window", {
        location: { origin: "http://box.lan:3773", href: "http://box.lan:3773/" },
      });
      resetServedPrimarySuppressionForTests();
      expect(isServedPrimarySuppressed()).toBe(true);
      const changes: boolean[] = [];
      const stop = onServedPrimarySuppressionChange(() =>
        changes.push(isServedPrimarySuppressed()),
      );
      setServedPrimarySuppressed(false);
      setServedPrimarySuppressed(false);
      stop();
      setServedPrimarySuppressed(true);
      expect(changes).toEqual([false]);
    });

    // A browser-normalized default port still matches a forwarded `host:443` marker.
    it("matches a marker that names the default port", () => {
      servedPage("box.example:443");
      expect(isServedByThisBackend(new URL("https://box.example/pair"))).toBe(true);
      servedPage("box.example:80");
      expect(isServedByThisBackend(new URL("http://box.example/pair"))).toBe(true);
      expect(isServedByThisBackend(new URL("https://box.example/pair"))).toBe(false);
    });

    it("keeps the remote-client fallback without a marker", () => {
      servedPage(null);
      expect(isServedByThisBackend(url)).toBe(false);
      expect(isHostedStaticApp(url)).toBe(true);
    });
  });

  it("does not classify a desktop bridge host as the hosted static app", () => {
    vi.stubEnv("VITE_HOSTED_APP_URL", "http://tauri.localhost");
    vi.stubEnv("VITE_HTTP_URL", "");
    vi.stubEnv("VITE_WS_URL", "");
    vi.stubGlobal("window", { desktopBridge: {} });

    expect(isHostedStaticApp(new URL("http://tauri.localhost/"))).toBe(false);
  });

  it("detects hosted channel aliases as static apps", () => {
    vi.stubEnv("VITE_HOSTED_APP_URL", "https://app.example.test");
    vi.stubEnv("VITE_HOSTED_APP_CHANNEL", "nightly");
    vi.stubEnv("VITE_HTTP_URL", "");
    vi.stubEnv("VITE_WS_URL", "");

    expect(isHostedStaticApp(new URL("https://nightly.app.example.test/"))).toBe(true);

    vi.stubEnv("VITE_HTTP_URL", "https://backend.example.com");
    expect(isHostedStaticApp(new URL("https://nightly.app.example.test/"))).toBe(false);
  });
});
