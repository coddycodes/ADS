/* oxlint-disable react/immutability -- the probe component captures the hook result for the test. */
import { EnvironmentId } from "@coddy/contracts";
import { renderToStaticMarkup } from "react-dom/server";
import type { ChangeEvent } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

/**
 * The hook is exercised through a probe component rendered with `renderToStaticMarkup`, the same
 * shape FileBrowserPanel.test.tsx uses. Only the flows the panel cannot easily reach are covered
 * here: the replace prompt is a promise the dialog settles, and every close path must settle it
 * exactly once.
 */
const testState = vi.hoisted(() => ({
  commandCalls: [] as Array<{ label: string; input: unknown }>,
  commandResults: {} as Record<string, unknown>,
}));

vi.mock("@coddy/client-runtime/state/runtime", () => ({
  isAtomCommandInterrupted: (result: { _tag: string; interrupted?: boolean }) =>
    result._tag === "Interrupted" || result.interrupted === true,
  squashAtomCommandFailure: (result: { error?: unknown }) => result.error,
}));

vi.mock("~/state/projects", () => ({
  projectEnvironment: {
    createDownloadUrl: { label: "createDownloadUrl" },
    createUploadUrl: { label: "createUploadUrl" },
  },
}));

vi.mock("~/state/use-atom-command", () => ({
  useAtomCommand: (command: { label?: string }) => (input: unknown) => {
    const label = command?.label ?? "unknown";
    testState.commandCalls.push({ label, input });
    return Promise.resolve(testState.commandResults[label] ?? { _tag: "Success", value: {} });
  },
}));

vi.mock("../ui/toast", () => ({
  stackedThreadToast: (options: Record<string, unknown>) => ({ stacked: true, ...options }),
  toastManager: { add: vi.fn() },
}));

import type { FileEntryDialogRequest } from "./FileEntryDialog";

/** The hook only ever pushes the confirm variant. */
type ConfirmRequest = Extract<FileEntryDialogRequest, { mode: "confirm" }>;
import { type FileTransfers, useFileTransfers } from "./useFileTransfers";

const environmentId = EnvironmentId.make("environment-1");

async function flushPromises(): Promise<void> {
  for (let turn = 0; turn < 6; turn += 1) await Promise.resolve();
}

interface Harness {
  readonly transfers: FileTransfers;
  readonly dialogRequests: Array<FileEntryDialogRequest | null>;
  readonly showMutationError: ReturnType<typeof vi.fn>;
  readonly refreshEntries: ReturnType<typeof vi.fn>;
  /** The most recent confirm prompt the hook pushed. */
  lastRequest: () => ConfirmRequest;
}

function renderTransfers(): Harness {
  const dialogRequests: Array<FileEntryDialogRequest | null> = [];
  const showMutationError = vi.fn();
  const refreshEntries = vi.fn();
  const captured: { transfers: FileTransfers | null } = { transfers: null };
  function Probe() {
    captured.transfers = useFileTransfers({
      environmentId,
      cwd: "/workspace/demo",
      httpBaseUrl: "http://127.0.0.1:4100",
      workspaceUnavailable: null,
      showMutationError,
      setDialogRequest: (request) => dialogRequests.push(request),
      refreshEntries,
      toasts: { add: vi.fn() },
    });
    return null;
  }
  renderToStaticMarkup(<Probe />);
  if (captured.transfers === null) throw new Error("the hook did not run");
  return {
    transfers: captured.transfers,
    dialogRequests,
    showMutationError,
    refreshEntries,
    lastRequest: () => {
      const request = dialogRequests.findLast((entry) => entry !== null);
      if (request?.mode !== "confirm") throw new Error("no confirm prompt was pushed");
      return request;
    },
  };
}

/** Feeds the browser-mode picker one file, as a change event on the hidden input would. */
function pickOne(transfers: FileTransfers, name: string): void {
  transfers.handleUploadInputChange({
    currentTarget: { files: [{ name }], value: name },
  } as unknown as ChangeEvent<HTMLInputElement>);
}

let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
  testState.commandCalls.length = 0;
  testState.commandResults = {
    createUploadUrl: {
      _tag: "Success",
      value: { relativeUrl: "/api/transfers/u.k", expiresAt: 1, maxBytes: 1024 },
    },
  };
  fetchMock = vi.fn();
  vi.stubGlobal("fetch", fetchMock);
  // No desktop bridge: the browser transport runs, which is the one the hidden input drives.
  vi.stubGlobal("window", {});
});

afterEach(() => {
  vi.unstubAllGlobals();
});

function respond(status: number, body: string) {
  return { status, text: async () => body };
}

describe("a refused mint", () => {
  it("names the file when the server refuses its name", async () => {
    // `operation_failed` is the code the server uses for a name it cannot store. Naming the
    // folder here would leave someone uploading a batch with no idea which file was wrong.
    testState.commandResults["createUploadUrl"] = {
      _tag: "Failure",
      error: {
        _tag: "ProjectTransferError",
        failure: "operation_failed",
        message: "Upload file name must be a plain file name.",
      },
    };
    const harness = renderTransfers();

    pickOne(harness.transfers, "report:v2.txt");
    await flushPromises();

    expect(fetchMock).not.toHaveBeenCalled();
    expect(harness.showMutationError).toHaveBeenCalledWith(
      expect.objectContaining({ failure: "operation_failed" }),
      'Can\u2019t upload "report:v2.txt"',
    );
  });

  it("names the folder when the destination is what failed", async () => {
    testState.commandResults["createUploadUrl"] = {
      _tag: "Failure",
      error: {
        _tag: "ProjectTransferError",
        failure: "not_found",
        message: "Upload target is not a folder.",
      },
    };
    const harness = renderTransfers();

    pickOne(harness.transfers, "a.txt");
    await flushPromises();

    expect(harness.showMutationError).toHaveBeenCalledWith(
      expect.objectContaining({ failure: "not_found" }),
      "Failed to prepare an upload to the workspace root",
    );
  });
});

describe("the replace prompt settles exactly once", () => {
  it("retries once when the prompt is confirmed and then closed", async () => {
    fetchMock
      .mockResolvedValueOnce(respond(409, '{"_tag":"TransferEntryExistsError"}'))
      .mockResolvedValueOnce(respond(201, '{"relativePath":"a.txt"}'));
    const harness = renderTransfers();

    pickOne(harness.transfers, "a.txt");
    await flushPromises();
    expect(fetchMock).toHaveBeenCalledTimes(1);

    // The dialog calls onConfirm and then onClose. The close must not resolve the promise a
    // second time and start another upload.
    harness.lastRequest().onConfirm();
    harness.transfers.closeDialog();
    await flushPromises();

    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(fetchMock.mock.calls[1]?.[0]).toBe(
      "http://127.0.0.1:4100/api/transfers/u.k?overwrite=1",
    );
    expect(harness.showMutationError).not.toHaveBeenCalled();
    expect(harness.refreshEntries).toHaveBeenCalledTimes(1);
  });

  it("keeps the existing file when the prompt is closed and only then confirmed", async () => {
    fetchMock.mockResolvedValue(respond(409, '{"_tag":"TransferEntryExistsError"}'));
    const harness = renderTransfers();

    pickOne(harness.transfers, "a.txt");
    await flushPromises();
    const request: ConfirmRequest = harness.lastRequest();

    harness.transfers.closeDialog();
    // A confirm that arrives after the close is the loser; it must not resurrect the upload.
    request.onConfirm();
    await flushPromises();

    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(harness.showMutationError).not.toHaveBeenCalled();
    expect(harness.refreshEntries).toHaveBeenCalledTimes(1);
  });

  it("declines a prompt that a later prompt displaces", async () => {
    fetchMock
      .mockResolvedValueOnce(respond(409, '{"_tag":"TransferEntryExistsError"}'))
      .mockResolvedValueOnce(respond(409, '{"_tag":"TransferEntryExistsError"}'))
      .mockResolvedValue(respond(201, '{"relativePath":"b.txt"}'));
    const harness = renderTransfers();

    // Two files in one batch: the second file's prompt replaces the first file's, which must
    // settle as declined rather than leaving that file's flow awaiting an answer forever.
    harness.transfers.handleUploadInputChange({
      currentTarget: { files: [{ name: "a.txt" }, { name: "b.txt" }], value: "a.txt" },
    } as unknown as ChangeEvent<HTMLInputElement>);
    await flushPromises();
    expect(fetchMock).toHaveBeenCalledTimes(1);

    // Declining the first file lets the batch move on to the second, which prompts in turn.
    harness.transfers.closeDialog();
    await flushPromises();
    expect(fetchMock).toHaveBeenCalledTimes(2);

    harness.lastRequest().onConfirm();
    await flushPromises();
    expect(fetchMock).toHaveBeenCalledTimes(3);
    expect(harness.refreshEntries).toHaveBeenCalledTimes(1);
  });
});
