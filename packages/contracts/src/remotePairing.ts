import * as Schema from "effect/Schema";

import { EnvironmentId, TrimmedNonEmptyString } from "./baseSchemas.ts";

export const REMOTE_PAIRING_CODE_VERSION = 1;

/** Spec section 4.2: pairing intent recorded per grant. */
export const RemotePairingReach = Schema.Literals(["another-device", "this-computer", "custom"]);
export type RemotePairingReach = typeof RemotePairingReach.Type;

/** JSON payload carried by `coddy-studio://pair?code=<base64url(JSON)>`. */
export const RemotePairingCodePayload = Schema.Struct({
  v: Schema.Literal(REMOTE_PAIRING_CODE_VERSION),
  endpoint: TrimmedNonEmptyString,
  name: TrimmedNonEmptyString,
  token: TrimmedNonEmptyString,
  hostKey: TrimmedNonEmptyString,
  reach: RemotePairingReach,
  storageInstanceId: TrimmedNonEmptyString,
});
export type RemotePairingCodePayload = typeof RemotePairingCodePayload.Type;

/** Optional transport features a client offers and the server confirms inside the channel. */
export const E2eeChannelFeatures = Schema.Array(TrimmedNonEmptyString);
export type E2eeChannelFeatures = typeof E2eeChannelFeatures.Type;

/** First-connect form: exchange the one-time credential inside the E2EE channel. */
export const E2eeAuthPairingMessage = Schema.Struct({
  type: Schema.Literal("e2ee_auth"),
  pairing: TrimmedNonEmptyString,
  features: Schema.optionalKey(E2eeChannelFeatures),
});
export type E2eeAuthPairingMessage = typeof E2eeAuthPairingMessage.Type;

/** Reconnect form: authenticate the stored bearer inside the E2EE channel. */
export const E2eeAuthBearerMessage = Schema.Struct({
  type: Schema.Literal("e2ee_auth"),
  bearer: TrimmedNonEmptyString,
  features: Schema.optionalKey(E2eeChannelFeatures),
});
export type E2eeAuthBearerMessage = typeof E2eeAuthBearerMessage.Type;

export const E2eeAuthMessage = Schema.Union([E2eeAuthPairingMessage, E2eeAuthBearerMessage]);
export type E2eeAuthMessage = typeof E2eeAuthMessage.Type;

/** Pairing success includes the minted credential and re-verifiable identity. */
export const E2eeAuthenticatedMessage = Schema.Struct({
  type: Schema.Literal("e2ee_authenticated"),
  credential: Schema.optionalKey(TrimmedNonEmptyString),
  environmentId: Schema.optionalKey(EnvironmentId),
  storageInstanceId: Schema.optionalKey(TrimmedNonEmptyString),
  pairingConfirmationRequired: Schema.optionalKey(Schema.Literal(true)),
  features: Schema.optionalKey(E2eeChannelFeatures),
});
export type E2eeAuthenticatedMessage = typeof E2eeAuthenticatedMessage.Type;

export const E2eeErrorCode = Schema.Literals(["unauthorized", "protocol"]);
export type E2eeErrorCode = typeof E2eeErrorCode.Type;

export const E2eeErrorMessage = Schema.Struct({
  type: Schema.Literal("e2ee_error"),
  code: E2eeErrorCode,
});
export type E2eeErrorMessage = typeof E2eeErrorMessage.Type;
