import type { DeviceInputEvent, DeviceKey } from "@bibcode/contracts";

/** The most events one `device.input` call accepts. */
export const MAX_INPUT_BATCH = 64;

const nextAnimationFrame = (flush: () => void) => {
  requestAnimationFrame(flush);
};

/**
 * Orders input for one device: the server may run unary calls concurrently, so exactly one
 * `device.input` is in flight. While it is, events wait here and pointer moves collapse to the
 * latest position per pointer, flushed once per animation frame.
 *
 * It also tracks what is held down, so keys and touches can be released when focus leaves the
 * screen or the queue closes, instead of staying pressed on the device. A key release is
 * accepted only for a key held down: the release of a press that was never forwarded, such as
 * Ctrl+Backspace's, is dropped, so the device never gets a spurious one.
 *
 * A batch that `send` rejects may have left a key or a touch down on the device. Its releases
 * go out once more, at once and ahead of later input, a touch's as a cancel where it was
 * lifted; one that fails again is held, so the next blur, hide, or unmount lifts it. After
 * `dispose` there is no next one (see there).
 */
export class DeviceInputQueue {
  private pending: DeviceInputEvent[] = [];
  private sending = false;
  private scheduled = false;
  /** No new input is accepted; what is queued (and the releases) still goes out. */
  private closed = false;
  /** Releases are waiting: drain without waiting for animation frames. */
  private urgent = false;
  private readonly heldKeys = new Set<DeviceKey>();
  private readonly heldTouches = new Map<number, { readonly x: number; readonly y: number }>();
  /** Releases sent again after their batch failed; none is sent a third time. */
  private readonly retries = new WeakSet<DeviceInputEvent>();

  constructor(
    private readonly send: (events: ReadonlyArray<DeviceInputEvent>) => Promise<void>,
    private readonly schedule: (flush: () => void) => void = nextAnimationFrame,
  ) {}

  get inFlight(): boolean {
    return this.sending;
  }

  /**
   * Queues an event; a move replaces the waiting move of the same pointer, and a key release
   * for a key not held is dropped.
   */
  push(event: DeviceInputEvent): void {
    if (this.closed) return;
    if (event.type === "key" && event.action === "up" && !this.heldKeys.has(event.key)) return;
    this.enqueue(event);
  }

  /**
   * Lifts every key still held down, such as when focus leaves the screen. Sent at once, not on
   * the next animation frame: a hidden or occluded window may not run one until it is back.
   */
  releaseKeys(): void {
    if (this.closed) return;
    this.queueKeyReleases();
    this.flushUrgently();
  }

  /** Lifts every held key and cancels every touch still down, sent at once. */
  releaseAll(): void {
    if (this.closed) return;
    this.queueKeyReleases();
    for (const [pointerId, position] of this.heldTouches) {
      this.enqueue({ type: "touch", action: "cancel", pointerId, ...position });
    }
    this.flushUrgently();
  }

  /**
   * Releases what is held, sends what is queued without waiting for a frame, and then accepts
   * nothing more.
   *
   * A release that fails twice after this is held again in a queue that nothing flushes any
   * more, so it is dropped with the queue: no later blur, hide, or unmount reaches it. On iOS
   * the server also releases the keys a failed send may have left down, but a release that
   * fails there waits for the device's next input.
   */
  dispose(): void {
    if (this.closed) return;
    this.releaseAll();
    this.closed = true;
  }

  private queueKeyReleases(): void {
    // Each release deletes the entry being visited, which Set iteration allows.
    for (const key of this.heldKeys) this.enqueue({ type: "key", key, action: "up" });
  }

  /** Sends what is queued now, and keeps draining without frames until it is empty. */
  private flushUrgently(): void {
    if (this.pending.length === 0) return;
    this.urgent = true;
    this.flush();
  }

  private enqueue(event: DeviceInputEvent): void {
    this.track(event);
    if (event.type === "touch" && event.action === "move") {
      for (let index = this.pending.length - 1; index >= 0; index -= 1) {
        const waiting = this.pending[index]!;
        if (waiting.type !== "touch" || waiting.pointerId !== event.pointerId) continue;
        // A down or up of the same pointer must stay between the moves around it.
        if (waiting.action === "move") {
          this.pending[index] = event;
          this.requestFlush();
          return;
        }
        break;
      }
    }
    this.pending.push(event);
    this.requestFlush();
  }

  private track(event: DeviceInputEvent): void {
    if (event.type === "key") {
      if (event.action === "down") this.heldKeys.add(event.key);
      else this.heldKeys.delete(event.key);
    } else if (event.type === "touch") {
      if (event.action === "down" || event.action === "move") {
        this.heldTouches.set(event.pointerId, { x: event.x, y: event.y });
      } else {
        this.heldTouches.delete(event.pointerId);
      }
    }
  }

  private requestFlush(): void {
    if (this.closed || this.sending || this.scheduled || this.pending.length === 0) return;
    this.scheduled = true;
    this.schedule(() => {
      this.scheduled = false;
      this.flush();
    });
  }

  private flush(): void {
    if (this.sending) return;
    if (this.pending.length === 0) {
      this.urgent = false;
      return;
    }
    const batch = this.pending.splice(0, MAX_INPUT_BATCH);
    this.sending = true;
    let sent: Promise<void>;
    try {
      sent = this.send(batch);
    } catch (error) {
      sent = Promise.reject(error);
    }
    // A failed batch is dropped but for its releases: the caller reports it, and later
    // input still goes out.
    const settle = () => {
      this.sending = false;
      if (this.closed || this.urgent) this.flush();
      else this.requestFlush();
    };
    void sent.then(settle, () => {
      this.recover(batch);
      settle();
    });
  }

  /**
   * Sends again, once and at once, the release of each key or touch that a failed batch
   * left up last, ahead of what was queued since; one already sent again is held instead,
   * unless later input for it is queued. Presses are not sent again: they stay held.
   */
  private recover(batch: ReadonlyArray<DeviceInputEvent>): void {
    const last = new Map<string, DeviceInputEvent>();
    for (const event of batch) {
      const target = targetOf(event);
      if (target !== null) last.set(target, event);
    }
    const resend: DeviceInputEvent[] = [];
    for (const [target, event] of last) {
      const release = releaseOf(event);
      if (release === null) continue;
      if (!this.retries.has(event)) {
        this.retries.add(release);
        resend.push(release);
      } else if (!this.pending.some((waiting) => targetOf(waiting) === target)) {
        this.hold(event);
      }
    }
    if (resend.length === 0) return;
    this.pending.unshift(...resend);
    this.urgent = true;
  }

  /** Tracks a release that never reached the device as held again. */
  private hold(release: DeviceInputEvent): void {
    if (release.type === "key") this.heldKeys.add(release.key);
    else if (release.type === "touch") {
      this.heldTouches.set(release.pointerId, { x: release.x, y: release.y });
    }
  }
}

/** The key or touch an event presses or releases; `null` for text. */
function targetOf(event: DeviceInputEvent): string | null {
  if (event.type === "key") return `key:${event.key}`;
  if (event.type === "touch") return `touch:${event.pointerId}`;
  return null;
}

/**
 * What lifts again what `event` released: the same key release, or a cancel where a touch was
 * lifted. `null` for a press or text.
 */
function releaseOf(event: DeviceInputEvent): DeviceInputEvent | null {
  if (event.type === "key") return event.action === "up" ? { ...event } : null;
  if (event.type === "touch" && (event.action === "up" || event.action === "cancel")) {
    return { ...event, action: "cancel" };
  }
  return null;
}
