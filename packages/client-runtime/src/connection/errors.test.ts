import { describe, expect, it } from "vite-plus/test";
import { EnvironmentId } from "@coddy/contracts";

import {
  credentialMissingError,
  environmentMismatchError,
  mapManagedRelayError,
  mapRemoteEnvironmentError,
  profileMissingError,
} from "./errors.ts";

describe("connection error mapping", () => {
  it("describes missing profiles, credentials, and mismatched environments", () => {
    expect(profileMissingError("profile-1")).toMatchObject({
      reason: "configuration",
      detail: "Connection profile profile-1 is unavailable.",
    });
    expect(credentialMissingError("credential-1")).toMatchObject({
      reason: "authentication",
      detail: "Connection credential credential-1 is unavailable.",
    });
    expect(
      environmentMismatchError({
        expected: EnvironmentId.make("environment-1"),
        actual: EnvironmentId.make("environment-2"),
      }),
    ).toMatchObject({
      reason: "configuration",
      detail: "Connected environment environment-2 does not match environment-1.",
    });
  });

  it("maps every protected relay error category", () => {
    const cases = [
      ["RelayAuthInvalidError", "authentication"],
      ["RelayEnvironmentLinkProofExpiredError", "authentication"],
      ["RelayEnvironmentConnectNotAuthorizedError", "permission"],
      ["RelayEnvironmentLinkProofInvalidError", "permission"],
      ["RelayEnvironmentEndpointTimedOutError", "timeout"],
      ["RelayEnvironmentEndpointUnavailableError", "endpoint-unavailable"],
      ["RelayEnvironmentLinkUnavailableError", "endpoint-unavailable"],
      ["RelayEnvironmentLinkFailedError", "relay-unavailable"],
      ["RelayInternalError", "relay-unavailable"],
    ] as const;

    for (const [tag, reason] of cases) {
      const mapped = mapManagedRelayError({
        _tag: "ManagedRelayRequestFailedError",
        message: "request failed",
        relayError: { _tag: tag, message: `detail:${tag}`, traceId: "trace-1" },
      } as never);
      expect(mapped).toMatchObject({ reason, detail: `detail:${tag}`, traceId: "trace-1" });
    }
  });

  it("maps unstructured managed relay failures with optional trace ids", () => {
    expect(
      mapManagedRelayError({
        _tag: "ManagedRelayRequestFailedError",
        message: "request failed",
        traceId: "trace-1",
      } as never),
    ).toMatchObject({ reason: "relay-unavailable", traceId: "trace-1" });
    expect(
      mapManagedRelayError({
        _tag: "ManagedRelayRequestFailedError",
        message: "request failed",
      } as never),
    ).not.toHaveProperty("traceId");
  });

  it("maps all remaining managed relay client failures", () => {
    const cases = [
      ["ManagedRelayRequestTimeoutError", "timeout"],
      ["ManagedRelayUrlInvalidError", "configuration"],
      ["ManagedRelayAccessTokenScopesUnexpectedError", "permission"],
      ["ManagedRelayDpopKeyLoadError", "authentication"],
      ["ManagedRelayTokenProofCreationError", "authentication"],
      ["ManagedRelayRequestProofCreationError", "authentication"],
    ] as const;

    for (const [tag, reason] of cases) {
      expect(mapManagedRelayError({ _tag: tag, message: `detail:${tag}` } as never)).toMatchObject({
        reason,
        detail: `detail:${tag}`,
      });
    }
  });

  it("maps every remote environment authorization failure", () => {
    const cases = [
      ["EnvironmentAuthInvalidError", "authentication"],
      ["EnvironmentScopeRequiredError", "permission"],
      ["EnvironmentOperationForbiddenError", "permission"],
      ["EnvironmentRequestInvalidError", "configuration"],
      ["RemoteEnvironmentAuthTimeoutError", "timeout"],
      ["RemoteEnvironmentAuthFetchError", "network"],
      ["EnvironmentInternalError", "remote-unavailable"],
      ["RemoteEnvironmentAuthInvalidJsonError", "remote-unavailable"],
      ["RemoteEnvironmentAuthUndeclaredStatusError", "remote-unavailable"],
    ] as const;

    for (const [tag, reason] of cases) {
      const mapped = mapRemoteEnvironmentError({
        _tag: tag,
        message: `detail:${tag}`,
        traceId: "trace-1",
      } as never);
      expect(mapped.reason).toBe(reason);
      if (tag.startsWith("RemoteEnvironmentAuth")) {
        expect(mapped.detail).toBe(`detail:${tag}`);
      }
    }
  });
});
