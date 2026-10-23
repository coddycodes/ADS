import type { AtomCommandResult } from "@bibcode/client-runtime/state/runtime";
import type { DiffLineAnnotation, FileContents, LineAnnotation } from "@pierre/diffs";
import { Editor, type EditorOptions } from "@pierre/diffs/edit";

import {
  FileSaveCoordinator,
  type FileSaveFlushResult,
  type FileSaveSettleResult,
  type FileSaveSnapshot,
} from "./fileSaveCoordinator";

export type FileEditorChangeHandler<LAnnotation> = (
  file: FileContents,
  lineAnnotations?: LineAnnotation<LAnnotation>[] | DiffLineAnnotation<LAnnotation>[],
) => void;

export interface FileEditingSessionSnapshot {
  readonly save: FileSaveSnapshot;
  readonly canUndo: boolean;
  readonly canRedo: boolean;
}

export interface FileEditingSessionOptions<A, E> {
  readonly cwd: string;
  readonly relativePath: string;
  readonly debounceMs: number;
  readonly persist: (relativePath: string, contents: string) => Promise<AtomCommandResult<A, E>>;
  readonly onPendingChange: (relativePath: string, pending: boolean) => void;
  readonly onConfirmed: (relativePath: string, contents: string) => void;
}

let nextSessionCacheKey = 0;

function createSessionCacheKey(cwd: string): string {
  nextSessionCacheKey += 1;
  return `${cwd}:file-editing-session:${nextSessionCacheKey}`;
}

export class FileEditingSession<LAnnotation, A = unknown, E = unknown> {
  private readonly cwd: string;
  private currentRelativePath: string;
  private currentCacheKey: string;
  private readonly coordinator: FileSaveCoordinator<A, E>;
  private readonly unsubscribeCoordinator: () => void;
  private readonly listeners = new Set<() => void>();
  private editorChangeHandler: FileEditorChangeHandler<LAnnotation> | null = null;
  private editorInstance: Editor<LAnnotation> | null = null;
  private editorChangedDocument = false;
  private disposed = false;
  private snapshot!: FileEditingSessionSnapshot;

  constructor(private readonly options: FileEditingSessionOptions<A, E>) {
    this.cwd = options.cwd;
    this.currentRelativePath = options.relativePath;
    this.currentCacheKey = createSessionCacheKey(options.cwd);
    this.coordinator = new FileSaveCoordinator({
      debounceMs: options.debounceMs,
      persist: (contents) => options.persist(this.currentRelativePath, contents),
      onPendingChange: (pending) => options.onPendingChange(this.currentRelativePath, pending),
      onConfirmed: (contents) => options.onConfirmed(this.currentRelativePath, contents),
    });
    this.snapshot = this.readSnapshot();
    this.unsubscribeCoordinator = this.coordinator.subscribe(() => this.publish());
  }

  get relativePath(): string {
    return this.currentRelativePath;
  }

  get cacheKey(): string {
    return this.currentCacheKey;
  }

  /**
   * Contents a surface must show instead of the file query: the text the editor
   * document holds once the editor has changed it, or a change not saved yet.
   * Null when the file query should seed the document. Pierre shares rendered
   * lines between files with the same `cacheKey`, so pairing `cacheKey` with
   * contents read before the save landed can paint them over the edited document.
   */
  get retainedContents(): string | null {
    if (!this.editorChangedDocument && !this.coordinator.hasPendingWork()) return null;
    return this.coordinator.getLatestContents();
  }

  /**
   * The text this session's document holds while part of it is not yet saved
   * (pending, saving, failed, or paused), or null once the file holds all of it.
   * A view that shows the file must prefer this text: the file query still holds
   * what was read before these edits, and changing the file from it drops them.
   */
  get unsavedContents(): string | null {
    return this.coordinator.hasPendingWork() ? this.coordinator.getLatestContents() : null;
  }

  get editor(): Editor<LAnnotation> {
    if (this.editorInstance === null) {
      this.editorInstance = this.createEditor();
      this.snapshot = this.readSnapshot();
    }
    return this.editorInstance;
  }

  private createEditor(): Editor<LAnnotation> {
    const editorOptions: EditorOptions<LAnnotation> = {
      persistState: true,
      onAttach: () => {
        if (!this.disposed) this.publish();
      },
      onChange: (file, lineAnnotations) => {
        if (this.disposed) return;
        this.editorChangedDocument = true;
        this.coordinator.change(file.contents);
        this.editorChangeHandler?.(file, lineAnnotations);
        this.publish();
      },
    };
    return new Editor<LAnnotation>(editorOptions);
  }

  private readSnapshot(): FileEditingSessionSnapshot {
    return {
      save: this.coordinator.getSnapshot(),
      canUndo: this.editorInstance?.canUndo ?? false,
      canRedo: this.editorInstance?.canRedo ?? false,
    };
  }

  private publish(): void {
    const next = this.readSnapshot();
    if (
      next.save === this.snapshot.save &&
      next.canUndo === this.snapshot.canUndo &&
      next.canRedo === this.snapshot.canRedo
    ) {
      return;
    }
    this.snapshot = next;
    for (const listener of this.listeners) listener();
  }

  readonly subscribe = (listener: () => void): (() => void) => {
    if (this.disposed) return () => {};
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  readonly getSnapshot = (): FileEditingSessionSnapshot => this.snapshot;

  setEditorChangeHandler(handler: FileEditorChangeHandler<LAnnotation> | null): void {
    if (this.disposed) return;
    this.editorChangeHandler = handler;
  }

  async flush(): Promise<FileSaveFlushResult> {
    return this.coordinator.flush();
  }

  async settle(): Promise<FileSaveSettleResult> {
    return this.coordinator.settle();
  }

  setAutosaveEnabled(enabled: boolean): void {
    this.coordinator.setAutosaveEnabled(enabled);
  }

  pauseSaving(): void {
    this.coordinator.pauseSaving();
  }

  resumeSaving(): void {
    this.coordinator.resumeSaving();
  }

  discardPendingSave(): void {
    this.coordinator.discardPendingSave();
  }

  undo(): void {
    if (!this.editorInstance?.canUndo) return;
    this.editorInstance.undo();
    this.publish();
  }

  redo(): void {
    if (!this.editorInstance?.canRedo) return;
    this.editorInstance.redo();
    this.publish();
  }

  rename(relativePath: string): void {
    if (relativePath === this.currentRelativePath) return;
    const previousRelativePath = this.currentRelativePath;
    const pending = this.coordinator.hasPendingWork();
    if (pending) this.options.onPendingChange(previousRelativePath, false);
    this.currentRelativePath = relativePath;
    if (pending) this.options.onPendingChange(relativePath, true);
  }

  changeOutsideEditor(contents: string): void {
    if (this.disposed) return;
    this.editorInstance?.cleanUp();
    this.editorInstance = null;
    this.editorChangedDocument = false;
    this.currentCacheKey = createSessionCacheKey(this.cwd);
    this.coordinator.change(contents);
    this.publish();
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.unsubscribeCoordinator();
    this.coordinator.dispose();
    const editor = this.editorInstance;
    this.editorInstance = null;
    this.editorChangeHandler = null;
    this.listeners.clear();
    editor?.cleanUp();
  }
}
