import { describe, expect, it, vi } from "vite-plus/test";

import { createMinuteClock, type MinuteClockEnvironment } from "./minuteClock";

function fakeEnvironment(start: number) {
  let now = start;
  let nextHandle = 1;
  const timers = new Map<number, { callback: () => void; at: number }>();
  const visibilityListeners = new Set<() => void>();
  const environment: MinuteClockEnvironment = {
    now: () => now,
    setTimeout: (callback, delayMs) => {
      const handle = nextHandle++;
      timers.set(handle, { callback, at: now + delayMs });
      return handle;
    },
    clearTimeout: (handle) => {
      timers.delete(handle as number);
    },
    onVisibilityChange: (listener) => {
      visibilityListeners.add(listener);
      return () => {
        visibilityListeners.delete(listener);
      };
    },
  };
  return {
    environment,
    setNow(time: number) {
      now = time;
    },
    advanceTo(time: number) {
      now = time;
      for (const [handle, timer] of Array.from(timers)) {
        if (timer.at <= now) {
          timers.delete(handle);
          timer.callback();
        }
      }
    },
    pendingTimers: () => timers.size,
    visibilityListenerCount: () => visibilityListeners.size,
    becomeVisible() {
      for (const listener of Array.from(visibilityListeners)) listener();
    },
  };
}

const at = (time: string) => Date.parse(`2026-09-24T${time}Z`);

describe("createMinuteClock", () => {
  it("keeps one minute-aligned timer for every subscriber", () => {
    const fake = fakeEnvironment(at("10:00:30.000"));
    const clock = createMinuteClock(fake.environment);
    const first = vi.fn();
    const second = vi.fn();
    clock.subscribe(first);
    clock.subscribe(second);
    expect(fake.pendingTimers()).toBe(1);

    fake.advanceTo(at("10:00:59.999"));
    expect(first).not.toHaveBeenCalled();

    fake.advanceTo(at("10:01:00.000"));
    expect(first).toHaveBeenCalledOnce();
    expect(second).toHaveBeenCalledOnce();
    expect(clock.getSnapshot()).toBe(at("10:01:00.000"));
    expect(fake.pendingTimers()).toBe(1);

    fake.advanceTo(at("10:02:00.000"));
    expect(first).toHaveBeenCalledTimes(2);
  });

  it("stops its timer and visibility listener at zero subscribers", () => {
    const fake = fakeEnvironment(at("10:00:00.000"));
    const clock = createMinuteClock(fake.environment);
    const listener = vi.fn();
    const unsubscribeA = clock.subscribe(listener);
    const unsubscribeB = clock.subscribe(vi.fn());
    unsubscribeA();
    expect(fake.pendingTimers()).toBe(1);
    unsubscribeB();
    unsubscribeB();
    expect(fake.pendingTimers()).toBe(0);
    expect(fake.visibilityListenerCount()).toBe(0);

    fake.advanceTo(at("10:05:00.000"));
    expect(listener).not.toHaveBeenCalled();
  });

  it("re-reads the time when the page becomes visible", () => {
    const fake = fakeEnvironment(at("10:00:00.000"));
    const clock = createMinuteClock(fake.environment);
    const listener = vi.fn();
    clock.subscribe(listener);
    fake.setNow(at("10:00:45.000"));
    fake.becomeVisible();
    expect(listener).toHaveBeenCalledOnce();
    expect(clock.getSnapshot()).toBe(at("10:00:45.000"));
    expect(fake.pendingTimers()).toBe(1);
  });

  it("leaves no timer when the last subscriber leaves during visibility notification", () => {
    const fake = fakeEnvironment(at("10:00:00.000"));
    const clock = createMinuteClock(fake.environment);
    const notify = vi.fn(() => unsubscribe());
    const unsubscribe = clock.subscribe(notify);
    fake.setNow(at("10:00:45.000"));
    fake.becomeVisible();
    expect(notify).toHaveBeenCalledOnce();
    expect(fake.pendingTimers()).toBe(0);
    expect(fake.visibilityListenerCount()).toBe(0);
    fake.advanceTo(at("10:02:00.000"));
    expect(notify).toHaveBeenCalledOnce();
  });

  it("refreshes a stale snapshot for the first subscriber", () => {
    const fake = fakeEnvironment(at("10:00:00.000"));
    const clock = createMinuteClock(fake.environment);
    fake.setNow(at("10:05:00.000"));
    clock.subscribe(vi.fn());
    expect(clock.getSnapshot()).toBe(at("10:05:00.000"));
  });
});
