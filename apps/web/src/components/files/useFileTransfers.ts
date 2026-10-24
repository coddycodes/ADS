// The Files panel's Download and Upload flows.
//
// Both mint a short-lived signed URL (projects.createDownloadUrl / projects.createUploadUrl) and
// then move the bytes over plain HTTP against the same environment base URL asset previews use.
// The desktop host streams to and from native pickers so a remote workspace transfers
// host-to-host; the browser falls back to an anchor download and a hidden file input.
//
// This lives apart from FileBrowserPanel because it is the panel's only multi-step, awaitable
// flow: it owns a promise that a dialog settles, a per-file retry loop, and a hidden input's
// target. The panel keeps the dialog state itself, because rename, delete, and New File… share it.

import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@bibcode/client-runtime/state/runtime";
import type { EnvironmentId } from "@bibcode/contracts";
import type { ChangeEvent, RefObject } from "react";
import { useCallback, useEffect, useRef } from "react";

import { projectEnvironment } from "~/state/projects";
import { useAtomCommand } from "~/state/use-atom-command";

import { stackedThreadToast, toastManager } from "../ui/toast";
import type { FileEntryDialogRequest } from "./FileEntryDialog";
import { entryName } from "./FileTreeContextMenu.logic";
import {
  describeByteLimit,
  downloadWithBridge,
  interpretUploadResponse,
  resolveTransferUrl,
  sendBrowserUpload,
  triggerBrowserDownload,
  uploadUrlFor,
} from "./fileTransfers";

const NO_SERVER_MESSAGE = "Not connected to this environment's server. Reconnect and try again.";

/** How an upload destination reads in a message: a quoted folder, or the root it stands for. */
function uploadTargetLabel(relativeDirectory: string): string {
  return relativeDirectory ? `"${relativeDirectory}"` : "the workspace root";
}

/**
 * Whether a refused mint is about this file rather than the folder it was headed for.
 *
 * The server refuses a name it cannot store with `operation_failed`; `not_found`,
 * `outside_root`, and `not_configured` are all properties of the destination or the server. The
 * distinction decides which name the toast leads with, because "Failed to prepare an upload to
 * the workspace root" tells someone dragging in twenty files nothing about which one was wrong.
 */
function isFileLevelMintFailure(error: unknown): boolean {
  return (
    typeof error === "object" &&
    error !== null &&
    "failure" in error &&
    (error as { failure?: unknown }).failure === "operation_failed"
  );
}

export interface UseFileTransfersInput {
  readonly environmentId: EnvironmentId;
  readonly cwd: string;
  /** This environment's server, or null while there is no connection to it. */
  readonly httpBaseUrl: string | null;
  /** Non-null while the workspace is gone; a flow awaiting a prompt stops instead of resuming. */
  readonly workspaceUnavailable: string | null;
  readonly showMutationError: (error: unknown, title: string) => void;
  readonly setDialogRequest: (request: FileEntryDialogRequest | null) => void;
  /** Re-reads the entry list once a batch has finished, however it finished. */
  readonly refreshEntries: () => void;
  readonly toasts?: Pick<typeof toastManager, "add">;
}

export interface FileTransfers {
  /** Downloads a file, or a folder as a zip, to a destination the runtime chooses. */
  readonly downloadEntry: (relativePath: string) => void;
  /** Opens the file picker for `relativeDirectory` ("" is the workspace root). */
  readonly uploadTo: (relativeDirectory: string) => void;
  readonly handleUploadInputChange: (event: ChangeEvent<HTMLInputElement>) => void;
  /** Belongs on the panel's hidden `<input type="file">`. */
  readonly uploadInputRef: RefObject<HTMLInputElement | null>;
  /** Every dialog close path: clears the request and settles a flow waiting on an answer. */
  readonly closeDialog: () => void;
}

export function useFileTransfers({
  environmentId,
  cwd,
  httpBaseUrl,
  workspaceUnavailable,
  showMutationError,
  setDialogRequest,
  refreshEntries,
  toasts = toastManager,
}: UseFileTransfersInput): FileTransfers {
  const createDownloadUrl = useAtomCommand(projectEnvironment.createDownloadUrl, {
    reportFailure: false,
  });
  const createUploadUrl = useAtomCommand(projectEnvironment.createUploadUrl, {
    reportFailure: false,
  });

  // FileEntryDialog only reports "closed", so a flow that awaits an answer (the upload replace
  // prompt) leaves its "not confirmed" resolver here. Every close path — Cancel, Escape, or the
  // workspace going away — runs through closeDialog, so such a flow can never hang.
  const dialogCancelRef = useRef<(() => void) | null>(null);
  // Browser-mode upload picker: the panel's hidden input is clicked for the folder recorded here.
  const uploadInputRef = useRef<HTMLInputElement | null>(null);
  const uploadTargetRef = useRef<string>("");
  const workspaceUnavailableRef = useRef(workspaceUnavailable);
  useEffect(() => {
    workspaceUnavailableRef.current = workspaceUnavailable;
  }, [workspaceUnavailable]);

  const closeDialog = useCallback(() => {
    setDialogRequest(null);
    const cancel = dialogCancelRef.current;
    dialogCancelRef.current = null;
    cancel?.();
  }, [setDialogRequest]);

  const downloadEntry = useCallback(
    (relativePath: string) => {
      const name = entryName(relativePath);
      if (!httpBaseUrl) {
        showMutationError(new Error(NO_SERVER_MESSAGE), `Can’t download "${name}"`);
        return;
      }
      void (async () => {
        try {
          const outcome = await downloadWithBridge({
            bridge: typeof window === "undefined" ? undefined : window.desktopBridge,
            prepare: async () => {
              const minted = await createDownloadUrl({
                environmentId,
                input: { cwd, relativePath },
              });
              if (minted._tag === "Failure") {
                if (!isAtomCommandInterrupted(minted)) {
                  showMutationError(
                    squashAtomCommandFailure(minted),
                    `Failed to prepare a download for "${name}"`,
                  );
                }
                return null;
              }
              const url = resolveTransferUrl(httpBaseUrl, minted.value.relativeUrl);
              if (url === null) {
                showMutationError(
                  new Error("The server did not return a usable download URL."),
                  `Can’t download "${name}"`,
                );
                return null;
              }
              return { url, fileName: minted.value.fileName };
            },
          });
          if (outcome._tag === "BrowserDownload") {
            triggerBrowserDownload(outcome.url, outcome.fileName);
          } else if (outcome._tag === "Saved") {
            // The host never overwrites: it uniquifies the name, so show the path it actually wrote.
            toasts.add(
              stackedThreadToast({
                type: "success",
                title: "Download saved",
                description: outcome.path,
              }),
            );
          }
        } catch (error) {
          showMutationError(error, `Failed to download "${name}"`);
        }
      })();
    },
    [createDownloadUrl, cwd, environmentId, httpBaseUrl, showMutationError, toasts],
  );

  /** Resolves false unless the user confirms; see `closeDialog` for the cancel path. */
  const confirmReplace = useCallback(
    (fileName: string, relativeDirectory: string) =>
      new Promise<boolean>((resolve) => {
        // A prompt this one displaces must not strand the flow awaiting it.
        dialogCancelRef.current?.();
        let settled = false;
        const settle = (value: boolean) => {
          if (settled) return;
          settled = true;
          resolve(value);
        };
        // The dialog calls onConfirm and then onClose, so both paths settle and only the first wins.
        dialogCancelRef.current = () => settle(false);
        setDialogRequest({
          mode: "confirm",
          title: `Replace "${fileName}"?`,
          description: `"${fileName}" already exists in ${uploadTargetLabel(relativeDirectory)}. Replacing it can't be undone.`,
          confirmLabel: "Replace",
          destructive: true,
          onConfirm: () => settle(true),
        });
      }),
    [setDialogRequest],
  );

  // One file, one freshly minted upload token bound to that one file name. `send` is the transport
  // (desktop host or browser fetch) so the retry-after-replace loop is identical in both runtimes.
  // Every failure — a refused upload or a rejected transport (unreadable file, permission error) —
  // is reported against this file's own name and stops here, so one bad file never aborts the rest
  // of a batch.
  const uploadOne = useCallback(
    async (
      relativeDirectory: string,
      file: { name: string; send: (url: string) => Promise<{ status: number; body: string }> },
    ): Promise<void> => {
      const target = uploadTargetLabel(relativeDirectory);
      const refuse = (message: string) =>
        showMutationError(new Error(message), `Can’t upload "${file.name}"`);
      if (!httpBaseUrl) {
        refuse(NO_SERVER_MESSAGE);
        return;
      }
      try {
        const minted = await createUploadUrl({
          environmentId,
          input: { cwd, relativeDirectory, fileName: file.name },
        });
        if (minted._tag === "Failure") {
          if (!isAtomCommandInterrupted(minted)) {
            const error = squashAtomCommandFailure(minted);
            showMutationError(
              error,
              isFileLevelMintFailure(error)
                ? `Can’t upload "${file.name}"`
                : `Failed to prepare an upload to ${target}`,
            );
          }
          return;
        }
        const transferUrl = resolveTransferUrl(httpBaseUrl, minted.value.relativeUrl);
        if (transferUrl === null) {
          refuse("The server did not return a usable upload URL.");
          return;
        }
        let overwrite = false;
        for (;;) {
          // The same token is reused for the replace retry: it already names this file.
          const response = await file.send(uploadUrlFor(transferUrl, overwrite));
          const step = interpretUploadResponse(response.status, response.body);
          if (step._tag === "Uploaded") return;
          if (step._tag === "Exists" && !overwrite) {
            const replace = await confirmReplace(file.name, relativeDirectory);
            if (!replace || workspaceUnavailableRef.current) return;
            overwrite = true;
            continue;
          }
          refuse(
            step._tag === "TooLarge"
              ? `The file is larger than the ${describeByteLimit(step.limit)} the server accepts.`
              : step._tag === "Exists"
                ? `Something else now uses that name in ${target}. Refresh the tree and try again.`
                : step.message,
          );
          return;
        }
      } catch (error) {
        showMutationError(error, `Can’t upload "${file.name}"`);
      }
    },
    [confirmReplace, createUploadUrl, cwd, environmentId, httpBaseUrl, showMutationError],
  );

  const uploadTo = useCallback(
    (relativeDirectory: string) => {
      const bridge = typeof window === "undefined" ? undefined : window.desktopBridge;
      const pickFiles = bridge?.pickFiles;
      const uploadFile = bridge?.uploadFile;
      if (pickFiles === undefined || uploadFile === undefined) {
        uploadTargetRef.current = relativeDirectory;
        uploadInputRef.current?.click();
        return;
      }
      void (async () => {
        // `uploadOne` reports its own failures per file, so the batch runs to the end and the tree
        // resyncs even when some of the files were refused.
        let picked: readonly string[] = [];
        try {
          picked = await pickFiles({ title: "Select files to upload" });
          for (const path of picked) {
            const name = path.split(/[\\/]/).pop() ?? path;
            await uploadOne(relativeDirectory, {
              name,
              send: (url) => uploadFile({ url, path }),
            });
          }
        } catch (error) {
          showMutationError(error, "Failed to upload files");
        } finally {
          if (picked.length > 0) refreshEntries();
        }
      })();
    },
    // oxlint-disable-next-line react/memo-dependencies -- refreshEntries is a prop called in the batch finally block; oxlint 1.87 misses that use.
    [refreshEntries, showMutationError, uploadOne],
  );

  const handleUploadInputChange = useCallback(
    (event: ChangeEvent<HTMLInputElement>) => {
      const input = event.currentTarget;
      const files = Array.from(input.files ?? []);
      // Clear the picker so choosing the same file twice in a row fires another change event.
      input.value = "";
      if (files.length === 0) return;
      const relativeDirectory = uploadTargetRef.current;
      void (async () => {
        try {
          for (const file of files) {
            await uploadOne(relativeDirectory, {
              name: file.name,
              send: (url) => sendBrowserUpload(url, file),
            });
          }
        } catch (error) {
          showMutationError(error, "Failed to upload files");
        } finally {
          refreshEntries();
        }
      })();
    },
    // oxlint-disable-next-line react/memo-dependencies -- refreshEntries is a prop called in the batch finally block; oxlint 1.87 misses that use.
    [refreshEntries, showMutationError, uploadOne],
  );

  return { downloadEntry, uploadTo, handleUploadInputChange, uploadInputRef, closeDialog };
}
