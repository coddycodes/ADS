import type { AtomCommandResult } from "@bibcode/client-runtime/state/runtime";

export interface FileSaveCoordinatorOptions<A, E> {
  readonly debounceMs: number;
  readonly persist: (contents: string) => Promise<AtomCommandResult<A, E>>;
  readonly onPendingChange: (pending: boolean) => void;
  readonly onConfirmed: (contents: string) => void;
}

export type FileSaveFlushResult = "saved" | "unchanged" | "saving" | "failed";
export type FileSaveSettleResult = "saved" | "unchanged" | "failed";

export type FileSavePhase = "clean" | "pending" | "saving" | "failed";

export interface FileSaveSnapshot {
  readonly phase: FileSavePhase;
  readonly canSave: boolean;
  readonly confirmedRevision: number;
}

const CLEAN_FILE_SAVE_SNAPSHOT: FileSaveSnapshot = {
  phase: "clean",
  canSave: false,
  confirmedRevision: 0,
};

interface QueuedImmediateFlush {
  readonly promise: Promise<FileSaveFlushResult>;
  readonly resolve: (result: FileSaveFlushResult) => void;
  readonly reject: (error: unknown) => void;
}

export class FileSaveCoordinator<A = unknown, E = unknown> {
  private timer: ReturnType<typeof setTimeout> | null = null;
  private latestContents = "";
  private changed = false;
  private latestRevision = 0;
  private persistedRevision = 0;
  private lastChangeAt = 0;
  private inFlight: Promise<boolean> | null = null;
  private savingPaused = false;
  private autosaveEnabled = true;
  private queuedImmediateFlush: QueuedImmediateFlush | null = null;
  private disposed = false;
  private snapshot: FileSaveSnapshot = CLEAN_FILE_SAVE_SNAPSHOT;
  private readonly listeners = new Set<() => void>();

  constructor(private readonly options: FileSaveCoordinatorOptions<A, E>) {}

  readonly getSnapshot = (): FileSaveSnapshot => this.snapshot;

  readonly subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  change(contents: string): void {
    if (this.disposed) return;
    this.latestContents = contents;
    this.changed = true;
    this.latestRevision += 1;
    this.lastChangeAt = Date.now();
    this.options.onPendingChange(true);
    this.publish(this.inFlight === null ? "pending" : "saving");
    if (this.autosaveEnabled && !this.savingPaused) {
      this.schedule(this.options.debounceMs);
    }
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.clearTimer();
    this.settleQueuedImmediateFlush("unchanged");
    // Only unsaved edits get a farewell write. An unconditional write here
    // resurrects the old path when the surface unmounts because the file was
    // just renamed or deleted out from under it.
    if (!this.savingPaused && this.latestRevision > this.persistedRevision) {
      void this.persistLatest();
    }
  }

  /** The contents of the latest change, saved, unsaved, or discarded; null before the first. */
  getLatestContents(): string | null {
    return this.changed ? this.latestContents : null;
  }

  /** True while a debounced write is scheduled or an immediate write is in flight. */
  hasPendingWork(): boolean {
    return this.latestRevision !== this.persistedRevision || this.inFlight !== null;
  }

  setAutosaveEnabled(enabled: boolean): void {
    if (this.autosaveEnabled === enabled) return;
    this.autosaveEnabled = enabled;
    if (!enabled) {
      this.clearTimer();
      this.settleQueuedImmediateFlush("saving");
      return;
    }
    if (
      !this.savingPaused &&
      this.inFlight === null &&
      this.latestRevision !== this.persistedRevision
    ) {
      this.schedule(this.options.debounceMs);
    }
  }

  pauseSaving(): void {
    if (this.savingPaused) return;
    this.savingPaused = true;
    this.clearTimer();
    this.publish(this.inFlight === null ? this.snapshot.phase : "saving");
  }

  resumeSaving(): void {
    if (!this.savingPaused) return;
    this.savingPaused = false;
    const queuedFlush = this.queuedImmediateFlush;
    this.queuedImmediateFlush = null;
    this.publish(this.inFlight === null ? this.snapshot.phase : "saving");
    if (
      queuedFlush !== null &&
      this.autosaveEnabled &&
      this.latestRevision !== this.persistedRevision
    ) {
      void this.persistLatest().then(
        (succeeded) => queuedFlush.resolve(succeeded ? "saved" : "failed"),
        (error: unknown) => queuedFlush.reject(error),
      );
    } else if (this.autosaveEnabled && this.latestRevision !== this.persistedRevision) {
      queuedFlush?.resolve("saving");
      this.schedule(this.options.debounceMs);
    } else {
      queuedFlush?.resolve("unchanged");
    }
  }

  discardPendingSave(): void {
    this.clearTimer();
    this.settleQueuedImmediateFlush("unchanged");
    this.latestRevision = this.persistedRevision;
    this.options.onPendingChange(false);
    this.publish("clean");
  }

  /**
   * Persist any pending debounced edit immediately (explicit save). Cancels the
   * outstanding debounce timer and writes now. No-op ("unchanged") when nothing
   * is unsaved; returns "saving" when a write is already in flight — that write
   * (and its reschedule) settles the remaining edits on its own.
   */
  flush(): Promise<FileSaveFlushResult> {
    if (this.savingPaused) {
      if (this.autosaveEnabled && this.latestRevision !== this.persistedRevision) {
        return this.queueImmediateFlush();
      }
      return Promise.resolve("saving");
    }
    if (this.inFlight !== null) return Promise.resolve("saving");
    if (this.latestRevision === this.persistedRevision) return Promise.resolve("unchanged");
    this.clearTimer();
    return this.persistLatest().then((succeeded) => (succeeded ? "saved" : "failed"));
  }

  async settle(): Promise<FileSaveSettleResult> {
    if (this.savingPaused) {
      return this.latestRevision === this.persistedRevision ? "unchanged" : "failed";
    }
    const hadUnsavedChanges = this.latestRevision !== this.persistedRevision;
    this.clearTimer();

    while (this.inFlight !== null) {
      await this.inFlight;
      this.clearTimer();
    }

    while (this.latestRevision !== this.persistedRevision) {
      const succeeded = await this.persistLatest();
      this.clearTimer();
      if (!succeeded && this.latestRevision !== this.persistedRevision) return "failed";
      while (this.inFlight !== null) await this.inFlight;
    }

    return hadUnsavedChanges ? "saved" : "unchanged";
  }

  private schedule(delay: number): void {
    this.clearTimer();
    this.timer = setTimeout(() => {
      this.timer = null;
      void this.persistLatest();
    }, delay);
  }

  private clearTimer(): void {
    if (this.timer === null) return;
    clearTimeout(this.timer);
    this.timer = null;
  }

  private queueImmediateFlush(): Promise<FileSaveFlushResult> {
    if (this.queuedImmediateFlush !== null) return this.queuedImmediateFlush.promise;
    let resolve!: (result: FileSaveFlushResult) => void;
    let reject!: (error: unknown) => void;
    const promise = new Promise<FileSaveFlushResult>((resolvePromise, rejectPromise) => {
      resolve = resolvePromise;
      reject = rejectPromise;
    });
    this.queuedImmediateFlush = { promise, resolve, reject };
    return promise;
  }

  private settleQueuedImmediateFlush(result: FileSaveFlushResult): void {
    const queuedFlush = this.queuedImmediateFlush;
    if (queuedFlush === null) return;
    this.queuedImmediateFlush = null;
    queuedFlush.resolve(result);
  }

  private persistLatest(): Promise<boolean> {
    if (
      this.savingPaused ||
      this.inFlight !== null ||
      this.latestRevision === this.persistedRevision
    ) {
      return Promise.resolve(false);
    }

    const contents = this.latestContents;
    const revision = this.latestRevision;
    const inFlight = Promise.resolve().then(() => this.persistRevision(contents, revision));
    this.inFlight = inFlight;
    return inFlight.finally(() => {
      if (this.inFlight === inFlight) {
        this.inFlight = null;
        this.publish(this.snapshot.phase);
      }
    });
  }

  private async persistRevision(contents: string, revision: number): Promise<boolean> {
    this.publish("saving");
    let result: AtomCommandResult<A, E>;
    try {
      result = await this.options.persist(contents);
    } catch (error) {
      this.publish(revision === this.latestRevision ? "failed" : "pending");
      throw error;
    }
    const succeeded = result._tag === "Success";
    if (succeeded) {
      this.persistedRevision = revision;
      this.options.onConfirmed(contents);
    }

    if (revision === this.latestRevision) {
      if (succeeded) {
        this.settleQueuedImmediateFlush("saved");
        this.options.onPendingChange(false);
        this.publish("clean");
      } else {
        this.publish("failed");
      }
      return succeeded;
    }

    this.publish("pending");

    const remainingDebounce = Math.max(
      0,
      this.options.debounceMs - (Date.now() - this.lastChangeAt),
    );
    if (this.disposed) {
      return this.persistRevision(this.latestContents, this.latestRevision);
    }
    if (this.autosaveEnabled && !this.savingPaused) {
      this.schedule(remainingDebounce);
    }
    return succeeded;
  }

  private publish(phase: FileSavePhase): void {
    const next: FileSaveSnapshot = {
      phase,
      canSave:
        this.latestRevision !== this.persistedRevision &&
        this.inFlight === null &&
        !this.savingPaused,
      confirmedRevision: this.persistedRevision,
    };
    if (
      next.phase === this.snapshot.phase &&
      next.canSave === this.snapshot.canSave &&
      next.confirmedRevision === this.snapshot.confirmedRevision
    ) {
      return;
    }
    this.snapshot = next;
    for (const listener of this.listeners) listener();
  }
}
