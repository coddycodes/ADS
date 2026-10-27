import { RpcResponseTooLargeError, WS_METHODS, WsRpcGroup } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";
import * as Cause from "effect/Cause";
import * as Exit from "effect/Exit";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import * as Rpc from "effect/rpc/Rpc";

import { RpcTransportErrors } from "./transportErrors.ts";

const group = WsRpcGroup.middleware(RpcTransportErrors);
const isRpcResponseTooLargeError = Schema.is(RpcResponseTooLargeError);
const exitDecoders = {
  [WS_METHODS.gitManagerGetCommits]: Schema.decodeUnknownSync(
    Schema.toCodecJson(Rpc.exitSchema(group.requests.get(WS_METHODS.gitManagerGetCommits)!)),
  ),
  [WS_METHODS.subscribeServerConfig]: Schema.decodeUnknownSync(
    Schema.toCodecJson(Rpc.exitSchema(group.requests.get(WS_METHODS.subscribeServerConfig)!)),
  ),
};

describe("RpcTransportErrors", () => {
  it("lets every method decode an oversized-response failure", () => {
    for (const [tag, decodeExit] of Object.entries(exitDecoders)) {
      const exit = decodeExit({
        _tag: "Failure",
        cause: [
          {
            _tag: "Fail",
            error: {
              _tag: "RpcResponseTooLargeError",
              method: tag,
              bytes: 70_000_000,
              limitBytes: 67_108_864,
            },
          },
        ],
      });
      expect(Exit.isFailure(exit)).toBe(true);
      if (!Exit.isFailure(exit)) return;
      const error = Cause.findErrorOption(exit.cause);
      expect(Option.isSome(error) && isRpcResponseTooLargeError(error.value)).toBe(true);
    }
  });
});
