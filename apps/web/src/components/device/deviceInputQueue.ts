import type { DeviceInputEvent } from "@bibcode/contracts";

/** The most events one `device.input` call accepts. */
export const MAX_INPUT_BATCH = 64;

const nextAnimationFrame = (flush: () => void) => {
  requestAnimationFrame(flush);
};

/**
 * Orders input for one device: the server may run unary calls concurrently, so exactly one
 * `device.input` is in flight. While it is, events wait here and pointer moves collapse to the
 * latest position per pointer, flushed once per animation frame.
 */
export class DeviceInputQueue {
  private pending: DeviceInputEvent[] = [];
  private sending = false;
  private scheduled = false;
  private disposed = false;

  constructor(
    private readonly send: (events: ReadonlyArray<DeviceInputEvent>) => Promise<void>,
    private readonly schedule: (flush: () => void) => void = nextAnimationFrame,
  ) {}

  get inFlight(): boolean {
    return this.sending;
  }

  /** Queues an event; a move replaces the waiting move of the same pointer. */
  push(event: DeviceInputEvent): void {
    if (this.disposed) return;
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

  /** Drops waiting events; nothing is sent after this. */
  dispose(): void {
    this.disposed = true;
    this.pending = [];
  }

  private requestFlush(): void {
    if (this.disposed || this.sending || this.scheduled || this.pending.length === 0) return;
    this.scheduled = true;
    this.schedule(() => {
      this.scheduled = false;
      this.flush();
    });
  }

  private flush(): void {
    if (this.disposed || this.sending || this.pending.length === 0) return;
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
      this.requestFlush();
    };
    void sent.then(settle, settle);
  }
}
