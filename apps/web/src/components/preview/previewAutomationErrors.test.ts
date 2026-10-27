import { EnvironmentId, PreviewTabId, ThreadId } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import {
  isPreviewAutomationHostError,
  PreviewAutomationNavigationTimeoutError,
  PreviewAutomationOperationError,
  PreviewAutomationOverlayTimeoutError,
  PreviewAutomationRecordingNotActiveError,
  PreviewAutomationTargetNotEditableHostError,
  PreviewAutomationTargetUnavailableError,
  PreviewAutomationViewportTimeoutError,
  serializePreviewAutomationHostError,
  type PreviewAutomationHostError,
} from "./previewAutomationErrors";

const environmentId = EnvironmentId.make("environment-1");
const threadId = ThreadId.make("thread-1");
const tabId = PreviewTabId.make("tab-1");
const context = {
  requestId: "request-1",
  operation: "click" as const,
  environmentId,
  threadId,
  tabId,
};

describe("preview automation host errors", () => {
  it("formats every timeout and availability boundary", () => {
    const overlay = new PreviewAutomationOverlayTimeoutError({
      requestId: context.requestId,
      environmentId,
      threadId,
      timeoutMs: 100,
    });
    const navigation = new PreviewAutomationNavigationTimeoutError({
      requestId: context.requestId,
      environmentId,
      threadId,
      tabId,
      readiness: "load",
      timeoutMs: 200,
    });
    const viewport = new PreviewAutomationViewportTimeoutError({
      requestId: context.requestId,
      environmentId,
      threadId,
      tabId,
      timeoutMs: 300,
    });
    expect(overlay.message).toContain("did not register within 100ms");
    expect(navigation.message).toContain("load readiness within 200ms");
    expect(viewport.message).toContain("was not rendered within 300ms");
    expect([overlay.responseTag, navigation.responseTag, viewport.responseTag]).toEqual([
      "PreviewAutomationTimeoutError",
      "PreviewAutomationTimeoutError",
      "PreviewAutomationTimeoutError",
    ]);

    expect(
      new PreviewAutomationTargetUnavailableError({
        ...context,
        tabId: null,
        bridgeAvailable: true,
      }).message,
    ).toContain("tab unassigned, bridge available");
    expect(
      new PreviewAutomationTargetUnavailableError({
        ...context,
        bridgeAvailable: false,
      }).message,
    ).toContain("tab tab-1, bridge unavailable");
    expect(
      new PreviewAutomationRecordingNotActiveError({
        requestId: context.requestId,
        environmentId,
        threadId,
        tabId: null,
      }).message,
    ).toContain("tab unassigned");
  });

  it("passes tagged host errors through and maps editable-target diagnostics", () => {
    const tagged = new PreviewAutomationTargetUnavailableError({
      ...context,
      bridgeAvailable: false,
    });
    expect(PreviewAutomationOperationError.fromCause({ ...context, cause: tagged })).toBe(tagged);
    expect(isPreviewAutomationHostError(tagged)).toBe(true);
    expect(isPreviewAutomationHostError(new Error("plain"))).toBe(false);

    for (const selectorKind of ["focused-element", "locator", "selector"] as const) {
      const mapped = PreviewAutomationOperationError.fromCause({
        ...context,
        cause: {
          _tag: "PreviewAutomationTargetNotEditableError",
          selectorKind,
          selectorLength: 12,
        },
      });
      expect(mapped).toBeInstanceOf(PreviewAutomationTargetNotEditableHostError);
      expect(mapped).toMatchObject({ selectorKind, selectorLength: 12 });
      expect(mapped.message).toContain("requires an editable target in tab tab-1");
      expect(mapped.responseTag).toBe("PreviewAutomationTargetNotEditableError");
    }

    const withoutValidDiagnostics = PreviewAutomationOperationError.fromCause({
      ...context,
      tabId: null,
      cause: {
        _tag: "PreviewAutomationTargetNotEditableError",
        selectorKind: "invalid",
        selectorLength: -1,
      },
    });
    expect(withoutValidDiagnostics).toBeInstanceOf(PreviewAutomationTargetNotEditableHostError);
    expect(withoutValidDiagnostics).not.toHaveProperty("selectorKind");
    expect(withoutValidDiagnostics).not.toHaveProperty("selectorLength");
    expect(withoutValidDiagnostics.message).toContain("tab unassigned");
  });

  it("wraps unrelated defects and rejects malformed diagnostic lookalikes", () => {
    for (const cause of [
      null,
      "plain",
      {},
      { _tag: "DifferentError" },
      { _tag: "PreviewAutomationTargetNotEditableError", selectorLength: 1.5 },
      { _tag: "PreviewAutomationTargetNotEditableError", selectorLength: "12" },
    ]) {
      const error = PreviewAutomationOperationError.fromCause({ ...context, cause });
      if (
        typeof cause === "object" &&
        cause !== null &&
        "_tag" in cause &&
        cause._tag === "PreviewAutomationTargetNotEditableError"
      ) {
        expect(error).toBeInstanceOf(PreviewAutomationTargetNotEditableHostError);
      } else {
        expect(error).toBeInstanceOf(PreviewAutomationOperationError);
        expect(error.message).toContain("failed on environment environment-1");
        expect(error.responseTag).toBe("PreviewAutomationExecutionError");
      }
    }
  });

  it("serializes structured details and omits an empty detail payload", () => {
    const error = new PreviewAutomationTargetUnavailableError({
      ...context,
      bridgeAvailable: false,
    });
    expect(serializePreviewAutomationHostError(error)).toMatchObject({
      _tag: "PreviewAutomationTabNotFoundError",
      detail: {
        requestId: "request-1",
        operation: "click",
        tabId: "tab-1",
        bridgeAvailable: false,
      },
    });

    const empty = {} as PreviewAutomationHostError;
    Object.defineProperties(empty, {
      _tag: { enumerable: true, value: "PreviewAutomationOperationError" },
      responseTag: { enumerable: false, value: "PreviewAutomationExecutionError" },
      message: { enumerable: false, value: "failed" },
    });
    expect(serializePreviewAutomationHostError(empty)).toEqual({
      _tag: "PreviewAutomationExecutionError",
      message: "failed",
    });
  });
});
