import { describe, expect, it } from "@effect/vitest";

import {
  avcCodecString,
  configSps,
  MAX_DEVICE_MEDIA_FRAME_BYTES,
  MediaFrameError,
  parseMediaFrame,
} from "./mediaFrames.ts";

describe("device media frames", () => {
  it("parses kind, pts and payload", () => {
    const bytes = new Uint8Array([1, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 1, 0x65]);
    expect(parseMediaFrame(bytes)).toEqual({
      kind: "key",
      ptsUs: 7n,
      data: new Uint8Array([0, 0, 0, 1, 0x65]),
    });
  });

  it("maps the server's kind bytes", () => {
    const frame = (kind: number) => parseMediaFrame(new Uint8Array([kind, 0, 0, 0, 0, 0, 0, 1, 0]));
    expect(frame(0).kind).toBe("config");
    expect(frame(1).kind).toBe("key");
    expect(frame(2).kind).toBe("delta");
    expect(frame(2).ptsUs).toBe(256n);
  });

  it("rejects truncated frames", () => {
    expect(() => parseMediaFrame(new Uint8Array([1, 0]))).toThrow(MediaFrameError);
  });

  it("rejects an unknown kind", () => {
    expect(() => parseMediaFrame(new Uint8Array([3, 0, 0, 0, 0, 0, 0, 0, 0]))).toThrow(
      MediaFrameError,
    );
  });

  it("caps a frame at the server's largest packet plus its header", () => {
    expect(MAX_DEVICE_MEDIA_FRAME_BYTES).toBe(8 * 1024 * 1024 + 9);
  });

  it("derives the avc1 codec string from an SPS", () => {
    expect(avcCodecString(new Uint8Array([0x67, 0x42, 0xc0, 0x1f]))).toBe("avc1.42c01f");
  });

  it("refuses a NAL unit that is not an SPS", () => {
    expect(() => avcCodecString(new Uint8Array([0x68, 0x42, 0xc0, 0x1f]))).toThrow(MediaFrameError);
    expect(() => avcCodecString(new Uint8Array([0x67, 0x42]))).toThrow(MediaFrameError);
  });

  it("finds the SPS among a config unit's Annex-B NAL units", () => {
    const config = new Uint8Array([
      0, 0, 0, 1, 0x67, 0x64, 0x00, 0x28, 0xac, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80,
    ]);
    expect(configSps(config)).toEqual(new Uint8Array([0x67, 0x64, 0x00, 0x28, 0xac]));
    expect(avcCodecString(configSps(config))).toBe("avc1.640028");
  });

  it("refuses a config unit without an SPS", () => {
    expect(() => configSps(new Uint8Array([0, 0, 0, 1, 0x68, 0xee]))).toThrow(MediaFrameError);
  });
});
