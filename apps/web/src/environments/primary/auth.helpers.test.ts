import {
  EnvironmentAuthInvalidError,
  EnvironmentInternalError,
  EnvironmentOperationForbiddenError,
  EnvironmentRequestInvalidError,
  EnvironmentScopeRequiredError,
} from "@coddy/contracts";
import { it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as TestClock from "effect/testing/TestClock";
import { afterEach, describe, expect, vi } from "vite-plus/test";

import {
  PrimaryEnvironmentRequestError,
  primaryEnvironmentAuthInternals,
  stripPairingTokenFromUrl,
  takePairingTokenFromUrl,
} from "./auth";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("primary environment authentication helpers", () => {
  it("maps every structured environment error to its HTTP status", () => {
    const errors = [
      new EnvironmentRequestInvalidError({
        code: "invalid_request",
        reason: "invalid_scope",
        traceId: "trace-400",
      }),
      new EnvironmentAuthInvalidError({
        code: "auth_invalid",
        reason: "missing_credential",
        traceId: "trace-401",
      }),
      new EnvironmentScopeRequiredError({
        code: "insufficient_scope",
        requiredScope: "terminal:operate",
        traceId: "trace-403-scope",
      }),
      new EnvironmentOperationForbiddenError({
        code: "operation_forbidden",
        reason: "current_session_revoke_not_allowed",
        traceId: "trace-403-operation",
      }),
      new EnvironmentInternalError({
        code: "internal_error",
        reason: "orchestration_dispatch_failed",
        traceId: "trace-500",
      }),
    ] as const;

    expect(errors.map(primaryEnvironmentAuthInternals.readEnvironmentHttpErrorStatus)).toEqual([
      400, 401, 403, 403, 500,
    ]);
    expect(primaryEnvironmentAuthInternals.readHttpApiStatus({})).toBeNull();
  });

  it("preserves optional request identifiers and inferred statuses", () => {
    const cause = new EnvironmentAuthInvalidError({
      code: "auth_invalid",
      reason: "missing_credential",
      traceId: "trace-auth",
    });
    const error = PrimaryEnvironmentRequestError.fromCause({
      operation: "revoke-client-session",
      pairingLinkId: "pairing-1",
      sessionId: "session-1",
      cause,
    });

    expect(error).toMatchObject({
      status: 401,
      pairingLinkId: "pairing-1",
      sessionId: "session-1",
    });
  });

  it("reads only a non-empty primary desktop bootstrap token", () => {
    vi.stubGlobal("window", { desktopBridge: undefined });
    expect(primaryEnvironmentAuthInternals.getDesktopBootstrapCredential()).toBeNull();

    vi.stubGlobal("window", {
      desktopBridge: {
        getLocalEnvironmentBootstraps: () => [
          { id: "secondary", bootstrapToken: "secondary-token" },
          { id: "primary", bootstrapToken: "" },
        ],
      },
    });
    expect(primaryEnvironmentAuthInternals.getDesktopBootstrapCredential()).toBeNull();

    vi.stubGlobal("window", {
      desktopBridge: {
        getLocalEnvironmentBootstraps: () => [{ id: "primary", bootstrapToken: "primary-token" }],
      },
    });
    expect(primaryEnvironmentAuthInternals.getDesktopBootstrapCredential()).toBe("primary-token");
  });

  it("classifies retryable bootstrap failures", () => {
    const requestError = (status: number) =>
      new PrimaryEnvironmentRequestError({
        operation: "fetch-session-state",
        status,
        cause: new Error("request failed"),
      });

    expect(primaryEnvironmentAuthInternals.isTransientBootstrapError(requestError(503))).toBe(true);
    expect(primaryEnvironmentAuthInternals.isTransientBootstrapError(requestError(400))).toBe(
      false,
    );
    expect(
      primaryEnvironmentAuthInternals.isTransientBootstrapError(new TypeError("network")),
    ).toBe(true);
    expect(
      primaryEnvironmentAuthInternals.isTransientBootstrapError(
        new DOMException("aborted", "AbortError"),
      ),
    ).toBe(true);
    expect(
      primaryEnvironmentAuthInternals.isTransientBootstrapError(
        new DOMException("denied", "SecurityError"),
      ),
    ).toBe(false);
    expect(primaryEnvironmentAuthInternals.isTransientBootstrapError(new Error("other"))).toBe(
      false,
    );
  });

  it.effect("interrupts a blackholed pairing-offer request at its deadline", () =>
    Effect.gen(function* () {
      const errorFiber = yield* primaryEnvironmentAuthInternals
        .withPrimaryRequestTimeout("create-pairing-offer", Effect.never, 5)
        .pipe(Effect.flip, Effect.forkChild);
      yield* Effect.yieldNow;
      yield* TestClock.adjust("5 millis");
      const error = yield* Fiber.join(errorFiber);
      expect(error).toMatchObject({
        _tag: "PrimaryEnvironmentRequestTimeoutError",
        operation: "create-pairing-offer",
        timeoutMs: 5,
      });
    }).pipe(Effect.provide(TestClock.layer())),
  );

  it("leaves URLs unchanged when no pairing token exists", () => {
    const replaceState = vi.fn();
    vi.stubGlobal("window", {
      location: new URL("https://example.test/path"),
      history: { replaceState },
    });
    vi.stubGlobal("document", { title: "Coddy Studio" });

    expect(takePairingTokenFromUrl()).toBeNull();
    stripPairingTokenFromUrl();
    expect(replaceState).not.toHaveBeenCalled();
  });
});
