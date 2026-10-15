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

  it("sends nothing after dispose, even from a flush already scheduled", () => {
    const sent: Array<ReadonlyArray<DeviceInputEvent>> = [];
    const flushes: Array<() => void> = [];
    const queue = new DeviceInputQueue(
      (events) => {
        sent.push(events);
        return Promise.resolve();
      },
      (flush) => flushes.push(flush),
    );
    queue.push(key(0));
    queue.dispose();
    flushes.shift()?.();
    queue.push(key(1));
    expect(sent).toEqual([]);
    expect(flushes).toHaveLength(0);
  });
});
