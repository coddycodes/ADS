import * as ManagedRuntime from "effect/ManagedRuntime";
import type * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import { HttpClient } from "effect/http";
import * as Socket from "effect/socket/Socket";

import { remoteHttpClientLayer } from "@bibcode/client-runtime/rpc";
import * as PrimaryEnvironmentHttpClient from "../environments/primary/httpClient";
import { primaryEnvironmentHttpLayer } from "../environments/primary/httpLayer";
import { tauriDesktopBridgeReady } from "../tauriDesktopBridge";

import { browserCryptoLayer } from "../cloud/dpop";
import { managedRelayClientLayer } from "../cloud/managedRelayLayer";
import { resolveCloudPublicConfig } from "../cloud/publicConfig";

function configuredRelayUrl(): string {
  return resolveCloudPublicConfig().relayUrl ?? "http://relay.invalid";
}

const httpClientLayer = remoteHttpClientLayer((input, init) => globalThis.fetch(input, init));

type RuntimeLayerSource =
  | typeof httpClientLayer
  | typeof browserCryptoLayer
  | typeof Socket.layerWebSocketConstructorGlobal
  | ReturnType<typeof managedRelayClientLayer>;

export const remoteHttpRuntime = ManagedRuntime.make(httpClientLayer);

const makePrimaryHttpRuntime = () =>
  ManagedRuntime.make(
    PrimaryEnvironmentHttpClient.layer.pipe(Layer.provide(primaryEnvironmentHttpLayer)),
  );

let primaryHttpRuntime: ReturnType<typeof makePrimaryHttpRuntime> | null = null;

const makePrimaryRawHttpRuntime = () => ManagedRuntime.make(primaryEnvironmentHttpLayer);

let primaryRawHttpRuntime: ReturnType<typeof makePrimaryRawHttpRuntime> | null = null;

async function getPrimaryHttpRuntime(): Promise<ReturnType<typeof makePrimaryHttpRuntime>> {
  await tauriDesktopBridgeReady.catch(() => undefined);
  primaryHttpRuntime ??= makePrimaryHttpRuntime();
  return primaryHttpRuntime;
}

async function getPrimaryRawHttpRuntime(): Promise<ReturnType<typeof makePrimaryRawHttpRuntime>> {
  await tauriDesktopBridgeReady.catch(() => undefined);
  primaryRawHttpRuntime ??= makePrimaryRawHttpRuntime();
  return primaryRawHttpRuntime;
}

export type PrimaryHttpEffectRunner = <A, E>(
  effect: Effect.Effect<A, E, PrimaryEnvironmentHttpClient.PrimaryEnvironmentHttpClient>,
) => Promise<A>;

const livePrimaryHttpRunner: PrimaryHttpEffectRunner = async (effect) =>
  (await getPrimaryHttpRuntime()).runPromise(effect);

let primaryHttpRunner = livePrimaryHttpRunner;

export const runPrimaryHttp = <A, E>(
  effect: Effect.Effect<A, E, PrimaryEnvironmentHttpClient.PrimaryEnvironmentHttpClient>,
) => primaryHttpRunner(effect);

export function __setPrimaryHttpRunnerForTests(runner?: PrimaryHttpEffectRunner): void {
  primaryHttpRunner = runner ?? livePrimaryHttpRunner;
}

export type PrimaryRawHttpEffectRunner = <A, E>(
  effect: Effect.Effect<A, E, HttpClient.HttpClient>,
) => Promise<A>;

const livePrimaryRawHttpRunner: PrimaryRawHttpEffectRunner = async (effect) =>
  (await getPrimaryRawHttpRuntime()).runPromise(effect);

let primaryRawHttpRunner = livePrimaryRawHttpRunner;

export const runPrimaryRawHttp = <A, E>(effect: Effect.Effect<A, E, HttpClient.HttpClient>) =>
  primaryRawHttpRunner(effect);

export function __setPrimaryRawHttpRunnerForTests(runner?: PrimaryRawHttpEffectRunner): void {
  primaryRawHttpRunner = runner ?? livePrimaryRawHttpRunner;
}

const runtimeLayer = Layer.mergeAll(
  httpClientLayer,
  browserCryptoLayer,
  Socket.layerWebSocketConstructorGlobal,
  managedRelayClientLayer(configuredRelayUrl()).pipe(
    Layer.provide(Layer.mergeAll(httpClientLayer, browserCryptoLayer)),
  ),
);

export const runtime: ManagedRuntime.ManagedRuntime<
  Layer.Success<RuntimeLayerSource>,
  Layer.Error<RuntimeLayerSource>
> = ManagedRuntime.make(runtimeLayer);

export const runtimeContextLayer: Layer.Layer<
  Layer.Success<RuntimeLayerSource>,
  Layer.Error<RuntimeLayerSource>
> = Layer.effectContext(runtime.contextEffect);
