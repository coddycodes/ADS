import { afterEach, describe, expect, it, vi } from "vite-plus/test";

import {
  MAX_DIAGNOSTIC_REQUEST_BYTES,
  downloadDiagnosticLogs,
  type DiagnosticLogsRequest,
  type DiagnosticLogsTransportResponse,
} from "./downloadDiagnosticLogs";

function zipResponse(
  overrides: Partial<DiagnosticLogsTransportResponse> = {},
): DiagnosticLogsTransportResponse {
  return {
    status: 200,
    contentType: "application/zip",
    contentDisposition: 'attachment; filename="coddy-studio-diagnostics-20260715T123456Z.zip"',
    bytes: new Uint8Array([0x50, 0x4b, 1, 2]),
    ...overrides,
  };
}

function harness(
  response: DiagnosticLogsTransportResponse = zipResponse(),
  saveArchive?: (filename: string, bytes: Uint8Array) => Promise<string | null>,
) {
  const requests: Array<DiagnosticLogsRequest> = [];
  const anchor = {
    href: "",
    download: "",
    click: vi.fn(),
    remove: vi.fn(),
  };
  const createObjectURL = vi.fn((_blob: Blob) => "blob:diagnostics");
  const revokeObjectURL = vi.fn();
  const appendAnchor = vi.fn();
  return {
    requests,
    anchor,
    createObjectURL,
    revokeObjectURL,
    appendAnchor,
    dependencies: {
      execute: async (request: DiagnosticLogsRequest) => {
        requests.push(request);
        return response;
      },
      resolveUrl: (path: string) => `http://primary.test${path}`,
      createAnchor: () => anchor,
      appendAnchor,
      createObjectURL,
      revokeObjectURL,
      now: () => new Date("2026-07-15T12:34:56.000Z"),
      saveArchive,
    },
  };
}

describe("downloadDiagnosticLogs", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("posts the frontend snapshot and downloads the returned ZIP", async () => {
    const h = harness();

    const filename = await downloadDiagnosticLogs("frontend warning\n", {}, h.dependencies);

    expect(h.requests).toEqual([
      {
        url: "http://primary.test/api/diagnostics/logs.zip",
        contentType: "application/json",
        body: JSON.stringify({ frontendLog: "frontend warning\n" }),
      },
    ]);
    expect(filename).toEqual({
      status: "downloaded",
      filename: "coddy-studio-diagnostics-20260715T123456Z.zip",
    });
    expect(h.createObjectURL).toHaveBeenCalledTimes(1);
    const blob = h.createObjectURL.mock.calls[0]?.[0];
    expect(blob).toBeInstanceOf(Blob);
    expect(blob).toMatchObject({ size: 4, type: "application/zip" });
    expect(h.anchor).toMatchObject({
      href: "blob:diagnostics",
      download: "coddy-studio-diagnostics-20260715T123456Z.zip",
    });
    expect(h.appendAnchor).toHaveBeenCalledWith(h.anchor);
    expect(h.anchor.click).toHaveBeenCalledTimes(1);
    expect(h.anchor.remove).toHaveBeenCalledTimes(1);
    expect(h.revokeObjectURL).toHaveBeenCalledWith("blob:diagnostics");
  });

  it("rejects non-success and non-ZIP responses before creating a download", async () => {
    const failed = harness(zipResponse({ status: 500 }));
    await expect(downloadDiagnosticLogs("warning", {}, failed.dependencies)).rejects.toThrow(
      "server returned 500",
    );
    expect(failed.createObjectURL).not.toHaveBeenCalled();

    const wrongType = harness(zipResponse({ contentType: "application/json" }));
    await expect(downloadDiagnosticLogs("warning", {}, wrongType.dependencies)).rejects.toThrow(
      "did not return a ZIP archive",
    );
    expect(wrongType.createObjectURL).not.toHaveBeenCalled();
  });

  it("uses a timestamped fallback when the server filename is unsafe", async () => {
    const h = harness(
      zipResponse({ contentDisposition: 'attachment; filename="../../private.zip"' }),
    );

    const filename = await downloadDiagnosticLogs("warning", {}, h.dependencies);

    expect(filename).toEqual({
      status: "downloaded",
      filename: "coddy-studio-diagnostics-20260715T123456Z.zip",
    });
    expect(h.anchor.download).toBe("coddy-studio-diagnostics-20260715T123456Z.zip");
  });

  it("uses the native archive saver without triggering a WebView download", async () => {
    const saveArchive = vi.fn(async () => "C:\\Users\\test\\Downloads\\diagnostics.zip");
    const h = harness(zipResponse(), saveArchive);

    const result = await downloadDiagnosticLogs("warning", {}, h.dependencies);

    expect(saveArchive).toHaveBeenCalledWith(
      "coddy-studio-diagnostics-20260715T123456Z.zip",
      new Uint8Array([0x50, 0x4b, 1, 2]),
    );
    expect(result).toEqual({
      status: "saved",
      filename: "coddy-studio-diagnostics-20260715T123456Z.zip",
      path: "C:\\Users\\test\\Downloads\\diagnostics.zip",
    });
    expect(h.createObjectURL).not.toHaveBeenCalled();
    expect(h.anchor.click).not.toHaveBeenCalled();
  });

  it("reports a cancelled native save without triggering a WebView download", async () => {
    const h = harness(zipResponse(), async () => null);

    await expect(downloadDiagnosticLogs("warning", {}, h.dependencies)).resolves.toEqual({
      status: "cancelled",
      filename: "coddy-studio-diagnostics-20260715T123456Z.zip",
    });
    expect(h.createObjectURL).not.toHaveBeenCalled();
  });

  it("asks for crash dumps only when the caller opts in", async () => {
    const optedIn = harness();
    await downloadDiagnosticLogs("warning", { includeCrashDumps: true }, optedIn.dependencies);
    expect(JSON.parse(optedIn.requests[0]?.body ?? "")).toEqual({
      frontendLog: "warning",
      includeCrashDumps: true,
    });

    const optedOut = harness();
    await downloadDiagnosticLogs("warning", { includeCrashDumps: false }, optedOut.dependencies);
    expect(optedOut.requests[0]?.body).toBe(JSON.stringify({ frontendLog: "warning" }));
  });

  it("keeps the request within the server limit when crash dumps are requested", async () => {
    const h = harness();
    const frontendLog = `oldest-only\n${"middle-line\n".repeat(60_000)}newest-record\n`;

    await downloadDiagnosticLogs(frontendLog, { includeCrashDumps: true }, h.dependencies);

    const body = h.requests[0]?.body ?? "";
    expect(new TextEncoder().encode(body).byteLength).toBeLessThanOrEqual(
      MAX_DIAGNOSTIC_REQUEST_BYTES,
    );
    const decoded = JSON.parse(body) as { frontendLog: string; includeCrashDumps?: boolean };
    expect(decoded.includeCrashDumps).toBe(true);
    expect(decoded.frontendLog).toContain("newest-record");
  });

  it("keeps the JSON request within the server limit while retaining newest records", async () => {
    const h = harness();
    const frontendLog = `oldest-only\n${"middle-line\n".repeat(60_000)}newest-record\n`;

    await downloadDiagnosticLogs(frontendLog, {}, h.dependencies);

    const body = h.requests[0]?.body ?? "";
    expect(new TextEncoder().encode(body).byteLength).toBeLessThanOrEqual(
      MAX_DIAGNOSTIC_REQUEST_BYTES,
    );
    const decoded = JSON.parse(body) as { frontendLog: string };
    expect(decoded.frontendLog).toContain("[oldest frontend records omitted]");
    expect(decoded.frontendLog).toContain("newest-record");
    expect(decoded.frontendLog).not.toContain("oldest-only");
  });
});
