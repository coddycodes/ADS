import { describe, expect, it } from "vite-plus/test";

import { JpegRenderer, type DecodedFrame } from "./jpegRenderer";

interface PendingDecode {
  readonly bytes: string;
  readonly resolve: (frame: DecodedFrame) => void;
  readonly reject: (error: unknown) => void;
}

function fakeDecoder() {
  const pending: PendingDecode[] = [];
  const closed: string[] = [];
  const decode = async (blob: Blob): Promise<DecodedFrame> => {
    const bytes = await blob.text();
    return new Promise<DecodedFrame>((resolve, reject) => {
      pending.push({ bytes, resolve, reject });
    });
  };
  const frame = (bytes: string): DecodedFrame => ({
    width: 1,
    height: 1,
    close: () => closed.push(bytes),
  });
  return { pending, closed, decode, frame };
}

const flush = async () => {
  for (let tick = 0; tick < 10; tick += 1) await Promise.resolve();
};

describe("JpegRenderer", () => {
  it("decodes only the newest frame that arrived while one was decoding", async () => {
    const decoder = fakeDecoder();
    const drawn: DecodedFrame[] = [];
    const renderer = new JpegRenderer((frame) => drawn.push(frame), decoder.decode);
    renderer.push(btoa("one"));
    renderer.push(btoa("two"));
    renderer.push(btoa("three"));
    await flush();
    expect(decoder.pending.map((entry) => entry.bytes)).toEqual(["one"]);
    const first = decoder.frame("one");
    decoder.pending[0]!.resolve(first);
    await flush();
    expect(drawn).toEqual([first]);
    expect(decoder.closed).toEqual(["one"]);
    expect(decoder.pending.map((entry) => entry.bytes)).toEqual(["one", "three"]);
  });

  it("skips a frame that fails to decode and keeps drawing", async () => {
    const decoder = fakeDecoder();
    const drawn: DecodedFrame[] = [];
    const renderer = new JpegRenderer((frame) => drawn.push(frame), decoder.decode);
    renderer.push(btoa("bad"));
    await flush();
    decoder.pending[0]!.reject(new Error("corrupt"));
    await flush();
    renderer.push(btoa("good"));
    await flush();
    const good = decoder.frame("good");
    decoder.pending[1]!.resolve(good);
    await flush();
    expect(drawn).toEqual([good]);
  });

  it("redraws the last drawn frame, for a canvas that was resized", async () => {
    const decoder = fakeDecoder();
    const drawn: DecodedFrame[] = [];
    const renderer = new JpegRenderer((frame) => drawn.push(frame), decoder.decode);
    renderer.redraw();
    await flush();
    expect(decoder.pending).toHaveLength(0);
    renderer.push(btoa("still"));
    await flush();
    decoder.pending[0]!.resolve(decoder.frame("still"));
    await flush();
    renderer.redraw();
    await flush();
    expect(decoder.pending.map((entry) => entry.bytes)).toEqual(["still", "still"]);
    const again = decoder.frame("still");
    decoder.pending[1]!.resolve(again);
    await flush();
    expect(drawn.at(-1)).toBe(again);
  });

  it("draws nothing after dispose and still closes a late frame", async () => {
    const decoder = fakeDecoder();
    const drawn: DecodedFrame[] = [];
    const renderer = new JpegRenderer((frame) => drawn.push(frame), decoder.decode);
    renderer.push(btoa("late"));
    renderer.push(btoa("dropped"));
    await flush();
    renderer.dispose();
    decoder.pending[0]!.resolve(decoder.frame("late"));
    await flush();
    expect(drawn).toEqual([]);
    expect(decoder.closed).toEqual(["late"]);
    expect(decoder.pending).toHaveLength(1);
  });
});
