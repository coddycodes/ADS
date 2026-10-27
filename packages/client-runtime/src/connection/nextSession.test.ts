import { GitCloneOperationError } from "@coddy/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Cause from "effect/Cause";
import { RpcClientError } from "effect/rpc";

import { EnvironmentRpcUnavailableError } from "../rpc/client.ts";
import { isSessionTransportLoss } from "./nextSession.ts";

describe("isSessionTransportLoss", () => {
  it("recognizes a lost transport without retrying typed server answers", () => {
    const lost = new RpcClientError.RpcClientError({
      reason: new RpcClientError.RpcClientDefect({ message: "socket closed", cause: new Error() }),
    });
    expect(isSessionTransportLoss(Cause.fail(lost))).toBe(true);
    expect(
      isSessionTransportLoss(
        Cause.fail(
          new EnvironmentRpcUnavailableError({
            environmentId: "host",
            message: "Host is disconnected",
          }),
        ),
      ),
    ).toBe(true);
    expect(
      isSessionTransportLoss(
        Cause.fail(
          new GitCloneOperationError({
            reason: "busy",
            destination: "/tmp/repository",
            message: "repository does not exist",
          }),
        ),
      ),
    ).toBe(false);
    expect(isSessionTransportLoss(Cause.die(new Error("unexpected defect")))).toBe(false);
  });
});
