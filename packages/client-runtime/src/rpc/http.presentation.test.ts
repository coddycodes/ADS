import { expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import { EnvironmentAuthInvalidError } from "@coddy/contracts";
import { executeEnvironmentHttpRequest, RemoteEnvironmentAuthFetchError } from "./http.ts";
import { mapRemoteEnvironmentError } from "../connection/errors.ts";
import { connectionStatusText } from "../connection/presentation.ts";

it.effect(
  "gives actionable connection guidance while preserving the original transport cause",
  () =>
    Effect.gen(function* () {
      const cause = new Error("Inert transport implementation diagnostic.");
      const error = yield* executeEnvironmentHttpRequest(
        "https://owned.example.test/.well-known/coddy-studio/environment?private-test=value",
        1000,
        Effect.fail(cause),
      ).pipe(Effect.flip);
      expect(error).toBeInstanceOf(RemoteEnvironmentAuthFetchError);
      if (error._tag !== "RemoteEnvironmentAuthFetchError")
        throw new Error("Expected transport failure.");
      expect(error.cause).toBe(cause);
      expect(error.message).toBe(
        "Could not reach the server. Check that it is running and that this device can reach its network.",
      );
      const mapped = mapRemoteEnvironmentError(error);
      expect(mapped._tag).toBe("ConnectionTransientError");
      expect(
        connectionStatusText({ phase: "reconnecting", error: mapped.message, traceId: null }),
      ).toBe(
        "Could not reach the server. Check that it is running and that this device can reach its network. Reconnecting…",
      );
      expect(error.message).not.toContain("owned.example.test");
      expect(error.message).not.toContain("private-test");
      expect(error.message).not.toContain("implementation");
    }),
);

it.effect(
  "preserves declared authentication failures rather than disguising them as network failures",
  () =>
    Effect.gen(function* () {
      const cause = new EnvironmentAuthInvalidError({
        code: "auth_invalid",
        reason: "invalid_credential",
        traceId: "owned-trace",
      });
      const error = yield* executeEnvironmentHttpRequest(
        "https://owned.example.test/.well-known/coddy-studio/environment",
        1000,
        Effect.fail(cause),
      ).pipe(Effect.flip);
      expect(error).toBe(cause);
      expect(mapRemoteEnvironmentError(error)._tag).toBe("ConnectionBlockedError");
    }),
);
