import type { DiscoveredLocalServer, ScopedThreadRef } from "@coddy/contracts";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  resolution: { kind: "reachable", url: "http://192.168.1.25:5173/" } as Record<string, unknown>,
  notices: [] as unknown[],
  opened: [] as unknown[],
}));

vi.mock("~/browser/browserTargetResolver", () => ({ resolvePreviewTarget: () => h.resolution }));
vi.mock("~/browser/linkNotices", () => ({
  showPreviewUnreachableNotice: (resolution: unknown) => h.notices.push(resolution),
}));
vi.mock("~/rightPanelStore", () => ({
  useRightPanelStore: { getState: () => ({ openBrowser: vi.fn() }) },
}));
vi.mock("./openPreviewSession", () => ({
  openPreviewSession: async (input: unknown) => {
    h.opened.push(input);
    return { _tag: "Success", value: { tabId: "tab-1" } };
  },
}));

const threadRef = { environmentId: "environment-1", threadId: "thread-1" } as ScopedThreadRef;
const port = { url: "http://localhost:5173/" } as DiscoveredLocalServer;

describe("openDiscoveredPort", () => {
  beforeEach(() => {
    h.notices.length = 0;
    h.opened.length = 0;
  });

  it("opens the topology-resolved URL", async () => {
    h.resolution = { kind: "reachable", url: "http://192.168.1.25:5173/" };
    const { openDiscoveredPort } = await import("./openDiscoveredPort");
    await openDiscoveredPort({ threadRef, port, openPreview: vi.fn() });
    expect(h.opened).toMatchObject([{ url: "http://192.168.1.25:5173/" }]);
    expect(h.notices).toEqual([]);
  });

  it("explains an unreachable server instead of opening a preview", async () => {
    h.resolution = { kind: "unreachable", reason: "ssh", environmentLabel: "Box" };
    const { openDiscoveredPort } = await import("./openDiscoveredPort");
    const result = await openDiscoveredPort({ threadRef, port, openPreview: vi.fn() });
    expect(result._tag).toBe("Success");
    expect(h.notices).toEqual([h.resolution]);
    expect(h.opened).toEqual([]);
  });
});
