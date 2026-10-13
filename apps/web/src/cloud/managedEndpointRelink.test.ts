import { describe, expect, it, vi } from "vite-plus/test";

import type { CloudLinkTarget } from "./linkEnvironment";
import { relinkOncePerSession } from "./managedEndpointRelink";

const target: CloudLinkTarget = {
  environmentId: "environment-1",
  label: "Desktop",
  httpBaseUrl: "http://127.0.0.1:3000",
  wsBaseUrl: "ws://127.0.0.1:3000",
};

const succeeded = { _tag: "Success" } as const;

describe("relinkOncePerSession", () => {
  it("re-links once per environment per session, then refreshes the state", async () => {
    const attempted = new Set<string>();
    const relink = vi.fn(async () => succeeded);
    const refresh = vi.fn();
    const deps = {
      target,
      accountId: "user-1",
      readToken: async () => "clerk-token",
      relink,
      refresh,
      attempted,
    };

    await relinkOncePerSession(deps);
    await relinkOncePerSession(deps);

    expect(relink).toHaveBeenCalledTimes(1);
    expect(relink).toHaveBeenCalledWith({
      target,
      clerkToken: "clerk-token",
      accountId: "user-1",
    });
    expect(refresh).toHaveBeenCalledTimes(1);
  });

  it("dispatches one re-link for calls that overlap a pending token lookup", async () => {
    const attempted = new Set<string>();
    const relink = vi.fn(async () => succeeded);
    let releaseToken: (token: string) => void = () => undefined;
    const token = new Promise<string>((resolve) => {
      releaseToken = resolve;
    });
    const deps = {
      target,
      accountId: "user-1",
      readToken: () => token,
      relink,
      refresh: vi.fn(),
      attempted,
    };

    const first = relinkOncePerSession(deps);
    const second = relinkOncePerSession(deps);
    releaseToken("clerk-token");
    await Promise.all([first, second]);

    expect(relink).toHaveBeenCalledTimes(1);
  });

  it("gives the attempt back while signed out, so a later sign-in retries", async () => {
    const attempted = new Set<string>();
    const relink = vi.fn(async () => succeeded);
    const deps = { target, accountId: "user-1", relink, refresh: vi.fn(), attempted };

    await relinkOncePerSession({ ...deps, readToken: async () => null });
    await relinkOncePerSession({
      ...deps,
      readToken: () => Promise.reject(new Error("token unavailable")),
    });
    expect(relink).not.toHaveBeenCalled();
    expect(attempted.size).toBe(0);

    await relinkOncePerSession({ ...deps, readToken: async () => "clerk-token" });
    expect(relink).toHaveBeenCalledTimes(1);
  });

  it("keeps a failed re-link for the next session without refreshing", async () => {
    const attempted = new Set<string>();
    const relink = vi.fn(async () => ({ _tag: "Failure" }) as const);
    const refresh = vi.fn();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const deps = {
      target,
      accountId: "user-1",
      readToken: async () => "clerk-token",
      relink,
      refresh,
      attempted,
    };

    await relinkOncePerSession(deps);
    await relinkOncePerSession(deps);

    expect(relink).toHaveBeenCalledTimes(1);
    expect(refresh).not.toHaveBeenCalled();
    expect(warn).toHaveBeenCalledOnce();
    warn.mockRestore();
  });
});
