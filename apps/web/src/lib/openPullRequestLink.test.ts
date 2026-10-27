import { describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  api: null as { shell: { openExternal: ReturnType<typeof vi.fn> } } | null,
  addToast: vi.fn(),
  consoleError: vi.fn(),
}));

vi.mock("react", () => ({ useCallback: (callback: unknown) => callback }));
vi.mock("../localApi", () => ({ readLocalApi: () => h.api }));
vi.mock("../components/ui/toast", () => ({
  stackedThreadToast: (toast: unknown) => toast,
  toastManager: { add: h.addToast },
}));

import {
  openPullRequestLink,
  PullRequestLinkOpenError,
  useOpenPrLink,
} from "./openPullRequestLink";

describe("openPullRequestLink", () => {
  it("opens the requested pull request URL", async () => {
    const openExternal = vi.fn(async () => undefined);
    const targetUrl = "https://github.com/mubeda/CoddyStudio/pull/123";

    await openPullRequestLink({ openExternal }, targetUrl);

    expect(openExternal).toHaveBeenCalledExactlyOnceWith(targetUrl);
  });

  it("reports bridge failures with a safe target origin", async () => {
    const cause = new Error("desktop shell unavailable");
    const targetUrl = "https://github.com/mubeda/CoddyStudio/pull/123?token=secret";
    const openExternal = vi.fn(async () => Promise.reject(cause));

    const result = openPullRequestLink({ openExternal }, targetUrl);

    await expect(result).rejects.toEqual(
      new PullRequestLinkOpenError({
        targetOrigin: "https://github.com",
        cause,
      }),
    );
    await expect(result).rejects.not.toHaveProperty("message", expect.stringContaining("secret"));
  });

  it("keeps malformed targets out of error diagnostics", () => {
    const cause = new Error("open failed");
    const error = PullRequestLinkOpenError.fromCause("not a url?token=secret", cause);

    expect(error).toMatchObject({ targetOrigin: null, cause });
    expect(error.message).toBe("Unable to open pull request link.");
    expect(error.message).not.toContain("secret");
  });

  it("prevents row activation and reports an unavailable local API", () => {
    h.api = null;
    h.addToast.mockClear();
    const event = { preventDefault: vi.fn(), stopPropagation: vi.fn() };

    useOpenPrLink()(event as never, "https://github.com/org/repo/pull/1");

    expect(event.preventDefault).toHaveBeenCalledOnce();
    expect(event.stopPropagation).toHaveBeenCalledOnce();
    expect(h.addToast).toHaveBeenCalledWith({
      type: "error",
      title: "Link opening is unavailable.",
    });
  });

  it("opens links through the local API and toasts asynchronous failures", async () => {
    const openExternal = vi
      .fn()
      .mockResolvedValueOnce(undefined)
      .mockRejectedValueOnce(new Error("shell unavailable"));
    h.api = { shell: { openExternal } };
    h.addToast.mockClear();
    vi.spyOn(console, "error").mockImplementation(h.consoleError);
    const event = { preventDefault: vi.fn(), stopPropagation: vi.fn() };
    const open = useOpenPrLink();

    open(event as never, "https://github.com/org/repo/pull/1");
    await Promise.resolve();
    expect(openExternal).toHaveBeenCalledWith("https://github.com/org/repo/pull/1");

    open(event as never, "https://github.com/org/repo/pull/2");
    await Promise.resolve();
    await Promise.resolve();
    expect(h.consoleError).toHaveBeenCalled();
    expect(h.addToast).toHaveBeenCalledWith(
      expect.objectContaining({
        title: "Unable to open pull request link",
        description: "Unable to open pull request link at https://github.com.",
      }),
    );
  });
});
