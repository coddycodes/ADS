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
 * screen or the queue closes, instead of staying pressed on the device.
 */
export class DeviceInputQueue {
  private pending: DeviceInputEvent[] = [];
  private sending = false;
  private scheduled = false;
  /** No new input is accepted; what is queued (and the releases) still goes out. */
  private closed = false;
  private readonly heldKeys = new Set<DeviceKey>();
  private readonly heldTouches = new Map<number, { readonly x: number; readonly y: number }>();

  constructor(
    private readonly send: (events: ReadonlyArray<DeviceInputEvent>) => Promise<void>,
    private readonly schedule: (flush: () => void) => void = nextAnimationFrame,
  ) {}

  get inFlight(): boolean {
    return this.sending;
  }

  /** Queues an event; a move replaces the waiting move of the same pointer. */
  push(event: DeviceInputEvent): void {
    if (this.closed) return;
    this.enqueue(event);
  }

  /** Lifts every key still held down, such as when focus leaves the screen. */
  releaseKeys(): void {
    if (this.closed) return;
    // Each release deletes the entry being visited, which Set iteration allows.
    for (const key of this.heldKeys) this.enqueue({ type: "key", key, action: "up" });
  }

  /** Lifts every held key and cancels every touch still down. */
  releaseAll(): void {
    if (this.closed) return;
    this.releaseKeys();
    for (const [pointerId, position] of this.heldTouches) {
      this.enqueue({ type: "touch", action: "cancel", pointerId, ...position });
    }
  }

  /**
   * Releases what is held, sends what is queued without waiting for a frame, and then accepts
   * nothing more.
   */
  dispose(): void {
    if (this.closed) return;
    this.releaseAll();
    this.closed = true;
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
    if (this.sending || this.pending.length === 0) return;
    const batch = this.pending.splice(0, MAX_INPUT_BATCH);
    this.sending = true;
    let sent: Promise<void>;
    try {
      sent = this.send(batch);
    } catch (error) {
      sent = Promise.reject(error);
    }
    // A failed batch is dropped: the caller reports it, and later input still goes out.
    const settle = () => {
      this.sending = false;
      if (this.closed) this.flush();
      else this.requestFlush();
    };
    void sent.then(settle, settle);
  }
}
