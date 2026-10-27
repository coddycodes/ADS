import { WsRpcGroup } from "@coddy/contracts";
import * as Effect from "effect/Effect";
import { RpcClient } from "effect/rpc";
import * as RpcMessage from "effect/rpc/RpcMessage";

import { RpcTransportErrors } from "./transportErrors.ts";

let nextRequestId = 0n;

export const makeWsRpcProtocolClient = RpcClient.make(WsRpcGroup.middleware(RpcTransportErrors), {
  generateRequestId: () => RpcMessage.RequestId(String(nextRequestId++)),
});
type RpcClientFactory = typeof makeWsRpcProtocolClient;
export type WsRpcProtocolClient =
  RpcClientFactory extends Effect.Effect<infer Client, any, any> ? Client : never;
