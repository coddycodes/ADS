import type { ServerConfig } from "@bibcode/contracts";
import { describe, expect, it } from "vite-plus/test";

import { isProfilerUnsupported, profilerRecording, serverHasProfiler } from "./profilerSupport";

const config = (profiler: boolean | undefined, enableProfiler: boolean) =>
  ({
    environment: { capabilities: profiler === undefined ? {} : { profiler } },
    settings: { enableProfiler },
  }) as unknown as ServerConfig;

describe("profilerSupport", () => {
  it("records only on a server that has the profiler and has it on", () => {
    expect(profilerRecording(config(true, true))).toBe(true);
    expect(profilerRecording(config(true, false))).toBe(false);
    // An older server echoes back a stored `enableProfiler: true` it never acts on.
    expect(profilerRecording(config(false, true))).toBe(false);
    expect(profilerRecording(config(undefined, true))).toBe(false);
    expect(profilerRecording(null)).toBe(false);
    expect(serverHasProfiler(config(true, false))).toBe(true);
    expect(serverHasProfiler(undefined)).toBe(false);
  });

  it("recognizes an older server's answer to a profiler method", () => {
    expect(isProfilerUnsupported("Unknown request tag: server.getProfilerStatus")).toBe(true);
    expect(isProfilerUnsupported("Unknown request tag: server.reportProfilerClientBatch")).toBe(
      true,
    );
    expect(isProfilerUnsupported("Unknown request tag: auth.confirmPairing")).toBe(false);
    expect(isProfilerUnsupported(new Error("Unknown request tag: server.getProfilerStatus"))).toBe(
      false,
    );
  });
});
