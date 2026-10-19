import { WS_METHODS } from "@bibcode/contracts";
import { Atom } from "effect/unstable/reactivity";

import type { EnvironmentRegistry } from "../connection/registry.ts";
import {
  createAtomCommandScheduler,
  createEnvironmentRpcCommand,
  createEnvironmentRpcSubscriptionAtomFamily,
} from "./runtime.ts";

/** Device commands are ordered per device of one environment, never across devices. */
export const deviceConcurrencyKey = ({
  environmentId,
  input,
}: {
  readonly environmentId: string;
  readonly input: { readonly deviceId: string };
}): string => JSON.stringify([environmentId, input.deviceId]);

export function createDeviceEnvironmentAtoms<R, E>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | R, E>,
) {
  const lifecycleScheduler = createAtomCommandScheduler();
  const inputScheduler = createAtomCommandScheduler();
  return {
    catalog: createEnvironmentRpcSubscriptionAtomFamily(runtime, {
      label: "environment-data:device:catalog",
      tag: WS_METHODS.deviceSubscribeCatalog,
    }),
    frames: createEnvironmentRpcSubscriptionAtomFamily(runtime, {
      label: "environment-data:device:frames",
      tag: WS_METHODS.deviceSubscribeFrames,
      // Frames are only worth their bandwidth while a screen shows them: the server stops
      // capturing when the last subscriber goes, so the stream ends with its view.
      idleTtlMs: 0,
    }),
    open: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:device:open",
      tag: WS_METHODS.deviceOpen,
      scheduler: lifecycleScheduler,
    }),
    close: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:device:close",
      tag: WS_METHODS.deviceClose,
      scheduler: lifecycleScheduler,
      concurrency: { mode: "singleFlight", key: deviceConcurrencyKey },
    }),
    input: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:device:input",
      tag: WS_METHODS.deviceInput,
      scheduler: inputScheduler,
      // Unary calls may run concurrently on the server; input order is kept here.
      concurrency: { mode: "serial", key: deviceConcurrencyKey },
    }),
    // A fresh single-use ticket per media socket connect; never cached.
    createMediaTicket: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:device:createMediaTicket",
      tag: WS_METHODS.deviceCreateMediaTicket,
      scheduler: lifecycleScheduler,
    }),
    rotate: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:device:rotate",
      tag: WS_METHODS.deviceRotate,
      scheduler: lifecycleScheduler,
      concurrency: { mode: "serial", key: deviceConcurrencyKey },
    }),
  };
}
