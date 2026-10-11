import { EnvironmentId, ThreadId } from "@bibcode/contracts";
import { AsyncResult } from "effect/unstable/reactivity";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const resolvePreviewTarget = vi.fn();
const showPreviewUnreachableNotice = vi.fn();
const openUrlInPreview = vi.fn();
const openExternal = vi.fn();
let previewSupported = true;
let anyUrlSupported = true;
let setting: "app" | "system" = "app";

vi.mock("./browserTargetResolver", () => ({ resolvePreviewTarget }));
const showLinkOpenFailedNotice = vi.fn();
const showPreviewUnreachableMessage = vi.fn();
vi.mock("./linkNotices", () => ({
  showPreviewUnreachableNotice,
  showLinkOpenFailedNotice,
  showPreviewUnreachableMessage,
}));
vi.mock("./openFileInPreview", () => ({ openUrlInPreview }));
vi.mock("~/previewStateStore", () => ({
  isPreviewSupportedInRuntime: () => previewSupported,
  canPreviewAnyUrlInRuntime: () => anyUrlSupported,
}));
vi.mock("~/localApi", () => ({ readLocalApi: () => ({ shell: { openExternal } }) }));
vi.mock("~/hooks/useSettings", () => ({
  getClientSettings: () => ({ browserLinkTarget: setting }),
}));
const resolveForNavigation = vi.fn();
vi.mock("./previewGateway", () => ({ resolveForNavigation }));
const enqueueOpenPrompt = vi.fn();
vi.mock("./openPromptQueue", () => ({ enqueueOpenPrompt }));
vi.mock("~/rpc/atomRegistry", () => ({ appAtomRegistry: {} }));
vi.mock("~/state/preview", () => ({ previewEnvironment: { gatewayOpen: {} } }));

const threadRef = {
  environmentId: EnvironmentId.make("env-1"),
  threadId: ThreadId.make("thread-1"),
};
const openPreview = vi.fn();

describe("chooseLinkDestination", () => {
  it.each([
    ["app", false, true, "app"],
    ["app", true, true, "system"],
    ["system", false, true, "system"],
    ["system", true, true, "app"],
    ["app", false, false, "system"],
    ["system", true, false, "system"],
  ] as const)("setting %s invert %s canUseApp %s -> %s", async (s, invert, canUseApp, want) => {
    const { chooseLinkDestination } = await import("./openLink");
    expect(chooseLinkDestination({ setting: s, invert, canUseApp })).toBe(want);
  });
});

describe("openLink", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    previewSupported = true;
    anyUrlSupported = true;
    setting = "app";
    openUrlInPreview.mockResolvedValue(AsyncResult.success(undefined));
    openExternal.mockResolvedValue(undefined);
  });

  it("opens resolved loopback URLs in the app", async () => {
    resolvePreviewTarget.mockReturnValue({ kind: "reachable", url: "http://10.0.0.2:5173/" });
    const { openLink } = await import("./openLink");
    expect(openLink({ url: "http://localhost:5173/", threadRef, invert: false, openPreview })).toBe(
      "app",
    );
    expect(openUrlInPreview).toHaveBeenCalledWith({
      threadRef,
      url: "http://10.0.0.2:5173/",
      openPreview,
    });
  });

  it("shows the notice and opens nothing for unreachable targets, even when inverted", async () => {
    const unreachable = { kind: "unreachable", reason: "ssh", environmentLabel: "Box" };
    resolvePreviewTarget.mockReturnValue(unreachable);
    const { openLink } = await import("./openLink");
    expect(openLink({ url: "http://localhost:5173/", threadRef, invert: true, openPreview })).toBe(
      "unreachable",
    );
    expect(showPreviewUnreachableNotice).toHaveBeenCalledWith(unreachable);
    expect(openExternal).not.toHaveBeenCalled();
    expect(openUrlInPreview).not.toHaveBeenCalled();
  });

  it("opens external without awaiting first", async () => {
    resolvePreviewTarget.mockReturnValue({ kind: "reachable", url: "https://example.com/" });
    setting = "system";
    const { openLink } = await import("./openLink");
    openLink({ url: "https://example.com/", threadRef, invert: false, openPreview });
    expect(openExternal).toHaveBeenCalledWith("https://example.com/");
  });

  it("falls back to the system browser without a thread", async () => {
    const { openLink } = await import("./openLink");
    expect(
      openLink({ url: "https://example.com/", threadRef: null, invert: false, openPreview }),
    ).toBe("system");
    expect(resolvePreviewTarget).not.toHaveBeenCalled();
    expect(openExternal).toHaveBeenCalledWith("https://example.com/");
  });

  it("falls back to the system browser when preview is unsupported", async () => {
    previewSupported = false;
    anyUrlSupported = false;
    resolvePreviewTarget.mockReturnValue({ kind: "reachable", url: "https://example.com/" });
    const { openLink } = await import("./openLink");
    expect(openLink({ url: "https://example.com/", threadRef, invert: false, openPreview })).toBe(
      "system",
    );
  });

  it("shows a copyable notice when the system browser fails, without reporting it twice", async () => {
    resolvePreviewTarget.mockReturnValue({ kind: "reachable", url: "https://example.com/" });
    setting = "system";
    openExternal.mockRejectedValue(new Error("no handler"));
    const onError = vi.fn();
    const { openLink } = await import("./openLink");
    openLink({ url: "https://example.com/", threadRef, invert: false, openPreview, onError });
    await vi.waitFor(() =>
      expect(showLinkOpenFailedNotice).toHaveBeenCalledWith("https://example.com/"),
    );
    expect(onError).not.toHaveBeenCalled();
  });

  it("resolves through the environment when there is no thread", async () => {
    const unreachable = { kind: "unreachable", reason: "ssh", environmentLabel: "Box" };
    resolvePreviewTarget.mockReturnValue(unreachable);
    const { openLink } = await import("./openLink");
    const environmentId = EnvironmentId.make("env-2");
    expect(
      openLink({
        url: "http://localhost:5173/",
        threadRef: null,
        environmentId,
        invert: false,
        openPreview,
      }),
    ).toBe("unreachable");
    expect(resolvePreviewTarget).toHaveBeenCalledWith(environmentId, "http://localhost:5173/");
    expect(showPreviewUnreachableNotice).toHaveBeenCalledWith(unreachable);
    expect(openExternal).not.toHaveBeenCalled();

    resolvePreviewTarget.mockReturnValue({ kind: "reachable", url: "http://10.0.0.2:5173/" });
    expect(
      openLink({
        url: "http://localhost:5173/",
        threadRef: null,
        environmentId,
        invert: false,
        openPreview,
      }),
    ).toBe("system");
    expect(openExternal).toHaveBeenCalledWith("http://10.0.0.2:5173/");
  });

  it("hands the canonical URL of a gateway address to the internal browser", async () => {
    resolvePreviewTarget.mockReturnValue({
      kind: "gateway",
      via: "ssh",
      url: "http://localhost:5173/",
    });
    const { openLink } = await import("./openLink");
    expect(openLink({ url: "http://0.0.0.0:5173/", threadRef, invert: false, openPreview })).toBe(
      "app",
    );
    expect(openUrlInPreview).toHaveBeenCalledWith({
      threadRef,
      url: "http://localhost:5173/",
      openPreview,
    });
  });

  it("in browser mode frames only gateway addresses on this page's host in the panel", async () => {
    anyUrlSupported = false;
    vi.stubGlobal("location", { hostname: "10.0.0.2" });
    const { openLink } = await import("./openLink");
    resolvePreviewTarget.mockReturnValue({
      kind: "gateway",
      via: "host",
      host: "10.0.0.2",
      url: "http://localhost:5173/",
    });
    expect(openLink({ url: "http://localhost:5173/", threadRef, invert: false, openPreview })).toBe(
      "app",
    );
    expect(openUrlInPreview).toHaveBeenCalledTimes(1);

    resolvePreviewTarget.mockReturnValue({ kind: "reachable", url: "https://example.com/" });
    expect(openLink({ url: "https://example.com/", threadRef, invert: false, openPreview })).toBe(
      "system",
    );
    // Another host's gateway refuses to be framed by this page.
    resolvePreviewTarget.mockReturnValue({
      kind: "gateway",
      via: "host",
      host: "10.0.0.3",
      url: "http://localhost:5173/",
    });
    vi.stubGlobal("window", { open: () => null });
    expect(openLink({ url: "http://localhost:5173/", threadRef, invert: false, openPreview })).toBe(
      "system",
    );
    expect(openUrlInPreview).toHaveBeenCalledTimes(1);
    vi.unstubAllGlobals();
  });

  it("opens a same-host page directly when it leaves the panel", async () => {
    anyUrlSupported = false;
    vi.stubGlobal("location", { hostname: "localhost" });
    resolvePreviewTarget.mockReturnValue({
      kind: "gateway",
      via: "host",
      host: "localhost",
      direct: true,
      url: "http://localhost:5173/",
    });
    const { openLink } = await import("./openLink");
    // No thread to admit a gateway target, and a modifier-click for a new tab.
    expect(
      openLink({ url: "http://localhost:5173/", threadRef: null, invert: false, openPreview }),
    ).toBe("system");
    expect(openLink({ url: "http://localhost:5173/", threadRef, invert: true, openPreview })).toBe(
      "system",
    );
    expect(openExternal.mock.calls).toEqual([
      ["http://localhost:5173/"],
      ["http://localhost:5173/"],
    ]);
    expect(resolveForNavigation).not.toHaveBeenCalled();
    expect(showPreviewUnreachableMessage).not.toHaveBeenCalled();
    vi.unstubAllGlobals();
  });

  describe("gateway address in the system browser", () => {
    const lanGateway = {
      kind: "gateway",
      via: "host",
      host: "10.0.0.2",
      url: "http://localhost:5173/",
    };
    const bootstrapUrl = "http://10.0.0.2:41000/__bibcode/bootstrap?cap=CAP&to=%2F";

    function stubBrowserTab(tab: unknown) {
      const open = vi.fn(() => tab);
      vi.stubGlobal("window", { open });
      return open;
    }

    beforeEach(() => {
      resolvePreviewTarget.mockReturnValue(lanGateway);
      setting = "system";
    });

    afterEach(() => {
      vi.unstubAllGlobals();
    });

    it("opens a blank tab synchronously then navigates", async () => {
      const replace = vi.fn();
      const tab = { opener: {}, location: { replace }, close: vi.fn() };
      const open = stubBrowserTab(tab);
      let finish!: (value: unknown) => void;
      resolveForNavigation.mockReturnValue(new Promise((resolve) => (finish = resolve)));
      const { openLink } = await import("./openLink");

      expect(
        openLink({ url: "http://localhost:5173/", threadRef, invert: false, openPreview }),
      ).toBe("system");
      // The tab exists before any await, so the click's activation still counts.
      expect(open).toHaveBeenCalledWith("about:blank", "_blank");
      expect(tab.opener).toBeNull();
      expect(replace).not.toHaveBeenCalled();
      expect(resolveForNavigation).toHaveBeenCalledWith(
        expect.objectContaining({
          environmentId: threadRef.environmentId,
          threadId: threadRef.threadId,
          canonicalUrl: "http://localhost:5173/",
        }),
      );

      finish({ kind: "ok", url: bootstrapUrl });
      await vi.waitFor(() => expect(replace).toHaveBeenCalledWith(bootstrapUrl));
      expect(openExternal).not.toHaveBeenCalled();
    });

    it("closes the blank tab and notifies on unreachable", async () => {
      const tab = { opener: {}, location: { replace: vi.fn() }, close: vi.fn() };
      stubBrowserTab(tab);
      resolveForNavigation.mockResolvedValue({
        kind: "unreachable",
        message: "Nothing is listening on port 5173 on Box.",
      });
      const { openLink } = await import("./openLink");

      openLink({ url: "http://localhost:5173/", threadRef, invert: false, openPreview });

      await vi.waitFor(() =>
        expect(showPreviewUnreachableMessage).toHaveBeenCalledWith(
          "Nothing is listening on port 5173 on Box.",
          "http://localhost:5173/",
        ),
      );
      expect(tab.close).toHaveBeenCalled();
      expect(tab.location.replace).not.toHaveBeenCalled();
    });

    it("does not offer a permanent gateway refusal again", async () => {
      stubBrowserTab({ opener: {}, location: { replace: vi.fn() }, close: vi.fn() });
      resolveForNavigation.mockResolvedValue({ kind: "unreachable", message: "No." });
      const onUnopened = vi.fn();
      const { openLink } = await import("./openLink");

      openLink({
        url: "http://localhost:5173/",
        threadRef,
        invert: false,
        openPreview,
        onUnopened,
      });

      await vi.waitFor(() => expect(showPreviewUnreachableMessage).toHaveBeenCalled());
      expect(onUnopened).not.toHaveBeenCalled();
    });

    it("tells the caller when a retryable gateway failure follows the opened tab", async () => {
      stubBrowserTab({ opener: {}, location: { replace: vi.fn() }, close: vi.fn() });
      resolveForNavigation.mockResolvedValue({
        kind: "unreachable",
        message: "No.",
        retryable: true,
      });
      const onUnopened = vi.fn();
      const { openLink } = await import("./openLink");

      openLink({
        url: "http://localhost:5173/",
        threadRef,
        invert: false,
        openPreview,
        onUnopened,
      });

      await vi.waitFor(() => expect(onUnopened).toHaveBeenCalledTimes(1));
    });

    it("falls back to the prompt when the popup is blocked", async () => {
      stubBrowserTab(null);
      const { openLink } = await import("./openLink");

      openLink({ url: "http://localhost:5173/", threadRef, invert: false, openPreview });

      expect(enqueueOpenPrompt).toHaveBeenCalledWith({
        source: "link",
        blocked: true,
        url: "http://localhost:5173/",
        threadRef,
      });
      expect(resolveForNavigation).not.toHaveBeenCalled();
    });

    it("lets a caller handle a blocked popup itself", async () => {
      stubBrowserTab(null);
      const onPopupBlocked = vi.fn();
      const { openLink } = await import("./openLink");

      openLink({
        url: "http://localhost:5173/",
        threadRef,
        invert: false,
        openPreview,
        onPopupBlocked,
      });

      expect(onPopupBlocked).toHaveBeenCalledWith("http://localhost:5173/");
      expect(enqueueOpenPrompt).not.toHaveBeenCalled();
    });

    it("reports a blocked tab for a direct address when the caller handles it", async () => {
      resolvePreviewTarget.mockReturnValue({ kind: "reachable", url: "https://example.com/" });
      stubBrowserTab(null);
      const onPopupBlocked = vi.fn();
      const { openLink } = await import("./openLink");

      openLink({
        url: "https://example.com/",
        threadRef,
        invert: false,
        openPreview,
        onPopupBlocked,
      });

      expect(onPopupBlocked).toHaveBeenCalledWith("https://example.com/");
      expect(openExternal).not.toHaveBeenCalled();
    });

    it("desktop system destination resolves then calls openExternal without window.open", async () => {
      const open = vi.fn();
      vi.stubGlobal("window", { open, desktopBridge: { preview: {} } });
      resolveForNavigation.mockResolvedValue({ kind: "ok", url: bootstrapUrl });
      const { openLink } = await import("./openLink");

      expect(
        openLink({ url: "http://localhost:5173/", threadRef, invert: false, openPreview }),
      ).toBe("system");

      await vi.waitFor(() => expect(openExternal).toHaveBeenCalledWith(bootstrapUrl));
      expect(open).not.toHaveBeenCalled();
    });

    it("uses openExternal on a desktop host without preview support", async () => {
      const open = vi.fn();
      vi.stubGlobal("window", { open, desktopBridge: {} });
      resolveForNavigation.mockResolvedValue({ kind: "ok", url: bootstrapUrl });
      const { openLink } = await import("./openLink");

      openLink({ url: "http://localhost:5173/", threadRef, invert: false, openPreview });

      await vi.waitFor(() => expect(openExternal).toHaveBeenCalledWith(bootstrapUrl));
      expect(open).not.toHaveBeenCalled();
    });

    it("notifies instead of opening when desktop resolution is refused", async () => {
      vi.stubGlobal("window", { open: vi.fn(), desktopBridge: { preview: {} } });
      resolveForNavigation.mockResolvedValue({ kind: "unreachable", message: "No." });
      const { openLink } = await import("./openLink");

      openLink({ url: "http://localhost:5173/", threadRef, invert: false, openPreview });

      await vi.waitFor(() =>
        expect(showPreviewUnreachableMessage).toHaveBeenCalledWith("No.", "http://localhost:5173/"),
      );
      expect(openExternal).not.toHaveBeenCalled();
    });

    it("refuses a gateway address without a thread, never opening this computer's localhost", async () => {
      const open = stubBrowserTab({ opener: {}, location: { replace: vi.fn() }, close: vi.fn() });
      const { openLink } = await import("./openLink");

      expect(
        openLink({
          url: "http://localhost:5173/",
          threadRef: null,
          environmentId: threadRef.environmentId,
          invert: false,
          openPreview,
        }),
      ).toBe("unreachable");
      expect(open).not.toHaveBeenCalled();
      expect(openExternal).not.toHaveBeenCalled();
      expect(showPreviewUnreachableMessage).toHaveBeenCalledWith(
        expect.stringContaining("thread"),
        "http://localhost:5173/",
      );
    });
  });

  it("reports async preview failures through onError", async () => {
    resolvePreviewTarget.mockReturnValue({ kind: "reachable", url: "https://example.com/" });
    const failure = new Error("boom");
    openUrlInPreview.mockRejectedValue(failure);
    const onError = vi.fn();
    const { openLink } = await import("./openLink");
    openLink({ url: "https://example.com/", threadRef, invert: false, openPreview, onError });
    await vi.waitFor(() => expect(onError).toHaveBeenCalledWith(failure));
  });
});
