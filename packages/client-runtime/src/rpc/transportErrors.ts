import { RpcResponseTooLargeError } from "@bibcode/contracts";
import * as RpcMiddleware from "effect/rpc/RpcMiddleware";

/**
 * Declares the transport failures any RPC may end with, so every method's
 * exit schema decodes them. The client never runs this middleware
 * (`requiredForClient` is false); the Rust server produces the failure.
 */
export class RpcTransportErrors extends RpcMiddleware.Service<RpcTransportErrors>()(
  "@bibcode/client-runtime/rpc/RpcTransportErrors",
  { error: RpcResponseTooLargeError },
) {}
