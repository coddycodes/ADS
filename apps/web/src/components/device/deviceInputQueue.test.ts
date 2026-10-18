import type { DeviceInputEvent } from "@bibcode/contracts";
import { describe, expect, it } from "vite-plus/test";

import { DeviceInputQueue } from "./deviceInputQueue";

const touch = (
  action: "down" | "move" | "up" | "cancel",
  x: number,
  y: number,
  pointerId = 0,
): DeviceInputEvent => ({ type: "touch", action, pointerId, x, y });

const key = (index: number): DeviceInputEvent => ({
  type: "key",
  key: index % 2 === 0 ? "back" : "home",
  action: "down",
});

const settle = async () => {
  for (let tick = 0; tick < 5; tick += 1) await Promise.resolve();
};

describe("DeviceInputQueue", () => {
  it("keeps one request in flight and coalesces moves", async () => {
    const sent: Array<ReadonlyArray<DeviceInputEvent>> = [];
    let release!: () => void;
    const queue = new DeviceInputQueue(
      (events) => {
        sent.push(events);
        return new Promise<void>((resolve) => (release = resolve));
      },
      (flush) => flush(),
    );
    queue.push(touch("down", 0.1, 0.1));
    queue.push(touch("move", 0.2, 0.2));
    queue.push(touch("move", 0.3, 0.3));
    queue.push(touch("up", 0.3, 0.3));
    expect(sent).toHaveLength(1);
    expect(queue.inFlight).toBe(true);
    release();
    await Promise.resolve();
    await Promise.resolve();
    expect(sent[1]).toEqual([touch("move", 0.3, 0.3), touch("up", 0.3, 0.3)]);
  });

  it("coalesces moves per pointer without crossing a down or up", () => {
    const sent: Array<ReadonlyArray<DeviceInputEvent>> = [];
    const flushes: Array<() => void> = [];
    const queue = new DeviceInputQueue(
      (events) => {
        sent.push(events);
        return new Promise<void>(() => undefined);
      },
      (flush) => flushes.push(flush),
    );
    queue.push(touch("move", 0.1, 0.1, 0));
    queue.push(touch("move", 0.1, 0.1, 1));
    queue.push(touch("move", 0.2, 0.2, 0));
    queue.push(touch("move", 0.2, 0.2, 1));
    queue.push(touch("up", 0.2, 0.2, 0));
    queue.push(touch("move", 0.3, 0.3, 0));
    flushes.shift()?.();
    expect(sent[0]).toEqual([
      touch("move", 0.2, 0.2, 0),
      touch("move", 0.2, 0.2, 1),
      touch("up", 0.2, 0.2, 0),
      touch("move", 0.3, 0.3, 0),
    ]);
  });

  it("splits batches above 64 events", async () => {
    const sent: Array<ReadonlyArray<DeviceInputEvent>> = [];
    const flushes: Array<() => void> = [];
    let release!: () => void;
    const queue = new DeviceInputQueue(
      (events) => {
        sent.push(events);
        return new Promise<void>((resolve) => (release = resolve));
      },
      (flush) => flushes.push(flush),
    );
    for (let index = 0; index < 70; index += 1) queue.push(key(index));
    expect(flushes).toHaveLength(1);
    flushes.shift()?.();
    expect(sent.map((batch) => batch.length)).toEqual([64]);
    release();
    await settle();
    flushes.shift()?.();
    expect(sent.map((batch) => batch.length)).toEqual([64, 6]);
    expect(sent[1]?.[0]).toEqual(key(64));
  });

  it("keeps going after a failed send", async () => {
    const sent: Array<ReadonlyArray<DeviceInputEvent>> = [];
    let failures = 1;
    const queue = new DeviceInputQueue(
      (events) => {
        sent.push(events);
        if (failures > 0) {
          failures -= 1;
          return Promise.reject(new Error("not ready"));
        }
        return Promise.resolve();
      },
      (flush) => flush(),
    );
    queue.push(key(0));
    await settle();
    expect(queue.inFlight).toBe(false);
    queue.push(key(1));
    await settle();
    expect(sent).toEqual([[key(0)], [key(1)]]);
  });

  it("keeps going when send throws synchronously", async () => {
    const sent: Array<ReadonlyArray<DeviceInputEvent>> = [];
    let throws = true;
    const queue = new DeviceInputQueue(
      (events) => {
        sent.push(events);
        if (throws) {
          throws = false;
          throw new Error("boom");
        }
        return Promise.resolve();
      },
      (flush) => flush(),
    );
    queue.push(key(0));
    await settle();
    queue.push(key(1));
    await settle();
    expect(sent).toEqual([[key(0)], [key(1)]]);
  });

  it("lifts held keys on release, once", () => {
    const sent: Array<ReadonlyArray<DeviceInputEvent>> = [];
    const flushes: Array<() => void> = [];
    const queue = new DeviceInputQueue(
      (events) => {
        sent.push(events);
        return new Promise<void>(() => undefined);
      },
      (flush) => flushes.push(flush),
    );
    queue.push({ type: "key", key: "backspace", action: "down" });
    queue.push({ type: "key", key: "home", action: "down" });
    queue.push({ type: "key", key: "home", action: "up" });
    queue.releaseKeys();
    queue.releaseKeys();
    flushes.shift()?.();
    expect(sent[0]).toEqual([
      { type: "key", key: "backspace", action: "down" },
      { type: "key", key: "home", action: "down" },
      { type: "key", key: "home", action: "up" },
      { type: "key", key: "backspace", action: "up" },
    ]);
  });

  it("forwards a key release only for a key it holds", () => {
    const sent: Array<ReadonlyArray<DeviceInputEvent>> = [];
    const flushes: Array<() => void> = [];
    const queue = new DeviceInputQueue(
      (events) => {
        sent.push(events);
        return new Promise<void>(() => undefined);
      },
      (flush) => flushes.push(flush),
    );
    // Ctrl+Backspace: its press stayed with the app, so its release must not reach the device.
    queue.push({ type: "key", key: "backspace", action: "up" });
    expect(flushes).toHaveLength(0);
    // A held key is released whatever else changed since it went down.
    queue.push({ type: "key", key: "backspace", action: "down" });
    queue.push({ type: "key", key: "backspace", action: "up" });
    // A second release of the same press is a duplicate.
    queue.push({ type: "key", key: "backspace", action: "up" });
    queue.push({ type: "key", key: "enter", action: "up" });
    flushes.shift()?.();
    expect(sent).toEqual([
      [
        { type: "key", key: "backspace", action: "down" },
        { type: "key", key: "backspace", action: "up" },
      ],
    ]);
    expect(flushes).toHaveLength(0);
  });

  it("sends releases at once, without waiting for a frame", async () => {
    const sent: Array<ReadonlyArray<DeviceInputEvent>> = [];
    const flushes: Array<() => void> = [];
    let release!: () => void;
    const queue = new DeviceInputQueue(
      (events) => {
        sent.push(events);
        return new Promise<void>((resolve) => (release = resolve));
      },
      (flush) => flushes.push(flush),
    );
    queue.push({ type: "key", key: "backspace", action: "down" });
    queue.push(touch("down", 0.2, 0.2));
    // The frame that would flush these never runs, as in a hidden window.
    queue.releaseKeys();
    expect(sent).toEqual([
      [
        { type: "key", key: "backspace", action: "down" },
        touch("down", 0.2, 0.2),
        { type: "key", key: "backspace", action: "up" },
      ],
    ]);
    // A release while a batch is in flight follows as soon as it settles, still without a frame.
    queue.releaseAll();
    release();
    await settle();
    expect(sent[1]).toEqual([touch("cancel", 0.2, 0.2)]);
    release();
    await settle();
    // Once drained, ordinary input waits for frames again; the frame requested before the
    // releases finds nothing left to send.
    expect(flushes).toHaveLength(1);
    flushes.shift()?.();
    expect(sent).toHaveLength(2);
    queue.push(key(2));
    expect(sent).toHaveLength(2);
    expect(flushes).toHaveLength(1);
  });

  it("on dispose cancels a drag in progress and sends it without waiting for a frame", async () => {
    const sent: Array<ReadonlyArray<DeviceInputEvent>> = [];
    const flushes: Array<() => void> = [];
    let release!: () => void;
    const queue = new DeviceInputQueue(
      (events) => {
        sent.push(events);
        return new Promise<void>((resolve) => (release = resolve));
      },
      (flush) => flushes.push(flush),
    );
    queue.push(touch("down", 0.1, 0.1, 3));
    flushes.shift()?.();
    queue.push(touch("move", 0.4, 0.5, 3));
    queue.push({ type: "key", key: "back", action: "down" });
    queue.dispose();
    queue.push(key(1));
    // The first batch is still in flight; the rest follows as soon as it settles.
    release();
    await settle();
    expect(sent).toEqual([
      [touch("down", 0.1, 0.1, 3)],
      [
        touch("move", 0.4, 0.5, 3),
        { type: "key", key: "back", action: "down" },
        { type: "key", key: "back", action: "up" },
        touch("cancel", 0.4, 0.5, 3),
      ],
    ]);
    expect(flushes).toHaveLength(0);
  });

  /** A queue whose sends fail while `failing` holds, flushing at once on a frame request. */
  function scriptedQueue(failing: (call: number) => boolean) {
    const sent: Array<ReadonlyArray<DeviceInputEvent>> = [];
    const frames: Array<() => void> = [];
    const queue = new DeviceInputQueue(
      (events) => {
        sent.push(events);
        return failing(sent.length)
          ? Promise.reject(new Error("the emulator accepted nothing"))
          : Promise.resolve();
      },
      (flush) => frames.push(flush),
    );
    const frame = () => frames.shift()?.();
    return { frame, frames, queue, sent };
  }

  const keyEvent = (action: "down" | "up"): DeviceInputEvent => ({
    type: "key",
    key: "backspace",
    action,
  });

  it("re-sends a key release whose batch failed", async () => {
    const { frame, frames, queue, sent } = scriptedQueue((call) => call === 2);
    queue.push(keyEvent("down"));
    frame();
    await settle();
    queue.push(keyEvent("up"));
    frame();
    await settle();
    // The release went out once more, at once, without waiting for a frame.
    expect(sent).toEqual([[keyEvent("down")], [keyEvent("up")], [keyEvent("up")]]);
    expect(frames).toHaveLength(0);
    // It was accepted, so nothing is held: a blur sends nothing more.
    queue.releaseKeys();
    await settle();
    expect(sent).toHaveLength(3);
  });

  it("re-sends a failed release ahead of later input", async () => {
    let release!: () => void;
    let calls = 0;
    const sent: Array<ReadonlyArray<DeviceInputEvent>> = [];
    const queue = new DeviceInputQueue(
      (events) => {
        sent.push(events);
        calls += 1;
        if (calls === 2) {
          return new Promise<void>((_, reject) => {
            release = () => reject(new Error("refused"));
          });
        }
        return Promise.resolve();
      },
      (flush) => flush(),
    );
    queue.push(keyEvent("down"));
    await settle();
    queue.push(keyEvent("up"));
    // Pressed again while the release is in flight.
    queue.push(keyEvent("down"));
    release();
    await settle();
    expect(sent.slice(2)).toEqual([[keyEvent("up"), keyEvent("down")]]);
    // The new press is still held, so a blur lifts it.
    queue.releaseKeys();
    await settle();
    expect(sent.at(-1)).toEqual([keyEvent("up")]);
  });

  it("keeps a release that failed twice for the next blur", async () => {
    const { frame, queue, sent } = scriptedQueue((call) => call === 2 || call === 3);
    queue.push(keyEvent("down"));
    frame();
    await settle();
    queue.push(keyEvent("up"));
    frame();
    await settle();
    // Re-sent once only.
    expect(sent).toEqual([[keyEvent("down")], [keyEvent("up")], [keyEvent("up")]]);
    queue.releaseKeys();
    await settle();
    expect(sent.at(-1)).toEqual([keyEvent("up")]);
    expect(sent).toHaveLength(4);
  });

  it("cancels a touch whose up was in a failed batch", async () => {
    const { frame, queue, sent } = scriptedQueue((call) => call === 2);
    queue.push(touch("down", 0.1, 0.1));
    frame();
    await settle();
    queue.push(touch("move", 0.4, 0.5));
    queue.push(touch("up", 0.4, 0.5));
    frame();
    await settle();
    expect(sent).toEqual([
      [touch("down", 0.1, 0.1)],
      [touch("move", 0.4, 0.5), touch("up", 0.4, 0.5)],
      [touch("cancel", 0.4, 0.5)],
    ]);
    queue.releaseAll();
    await settle();
    expect(sent).toHaveLength(3);
  });

  it("cancels on releaseAll a touch whose cancel also failed", async () => {
    const { frame, queue, sent } = scriptedQueue((call) => call === 2 || call === 3);
    queue.push(touch("down", 0.1, 0.1, 2));
    frame();
    await settle();
    queue.push(touch("up", 0.3, 0.3, 2));
    frame();
    await settle();
    expect(sent.at(-1)).toEqual([touch("cancel", 0.3, 0.3, 2)]);
    queue.releaseAll();
    await settle();
    expect(sent.at(-1)).toEqual([touch("cancel", 0.3, 0.3, 2)]);
    expect(sent).toHaveLength(4);
  });

  it("re-sends no press of a failed batch", async () => {
    const { frame, queue, sent } = scriptedQueue((call) => call === 1);
    queue.push(keyEvent("down"));
    queue.push(touch("down", 0.2, 0.2));
    frame();
    await settle();
    expect(sent).toHaveLength(1);
    // The presses stay held, so a blur or unmount still lifts them.
    queue.releaseAll();
    await settle();
    expect(sent.at(-1)).toEqual([keyEvent("up"), touch("cancel", 0.2, 0.2)]);
  });

  const text = (value: string): DeviceInputEvent => ({ type: "text", text: value });

  it("sends nothing more for a refused batch of text", async () => {
    // The server refuses text the emulator would drop, as a whole: nothing of the batch was
    // sent, and text holds nothing down, so there is no release to send again.
    const { frame, queue, sent } = scriptedQueue((call) => call === 1);
    queue.push(text("héllo"));
    frame();
    await settle();
    expect(sent).toEqual([[text("héllo")]]);
    queue.releaseAll();
    await settle();
    expect(sent).toHaveLength(1);
    // Later input still goes out.
    queue.push(text("hello"));
    frame();
    await settle();
    expect(sent.at(-1)).toEqual([text("hello")]);
  });

  it("re-sends the releases of a refused batch that also carried text", async () => {
    const { frame, queue, sent } = scriptedQueue((call) => call === 2);
    queue.push(keyEvent("down"));
    queue.push(touch("down", 0.1, 0.1));
    frame();
    await settle();
    // Both lifts share a batch with text the server refuses, so neither reached the device.
    queue.push(keyEvent("up"));
    queue.push(text("héllo"));
    queue.push(touch("up", 0.3, 0.3));
    frame();
    await settle();
    expect(sent.slice(1)).toEqual([
      [keyEvent("up"), text("héllo"), touch("up", 0.3, 0.3)],
      // Once more, at once: the key release and a cancel where the touch was lifted, and
      // none of the text.
      [keyEvent("up"), touch("cancel", 0.3, 0.3)],
    ]);
    // They were accepted, so nothing is held: a blur sends nothing more.
    queue.releaseAll();
    await settle();
    expect(sent).toHaveLength(3);
  });

  it("forgets a touch once it is lifted", () => {
    const sent: Array<ReadonlyArray<DeviceInputEvent>> = [];
    const queue = new DeviceInputQueue(
      (events) => {
        sent.push(events);
        return new Promise<void>(() => undefined);
      },
      () => undefined,
    );
    queue.push(touch("down", 0.1, 0.1));
    queue.push(touch("up", 0.1, 0.1));
    queue.dispose();
    expect(sent).toEqual([[touch("down", 0.1, 0.1), touch("up", 0.1, 0.1)]]);
  });
});
