export {
  getPrimaryKnownEnvironment,
  readPrimaryEnvironmentDescriptor,
  resetPrimaryEnvironmentDescriptorForTests,
  resolveInitialPrimaryEnvironmentDescriptor,
  writePrimaryEnvironmentDescriptor,
  __resetPrimaryEnvironmentBootstrapForTests,
  __resetPrimaryEnvironmentDescriptorBootstrapForTests,
} from "./context";

export {
  resolveInitialPrimaryEnvironmentDescriptor as ensurePrimaryEnvironmentReady,
  writePrimaryEnvironmentDescriptor as updatePrimaryEnvironmentDescriptor,
} from "./context";

export {
  cancelServerPairingOffer,
  createServerPairingCredential,
  createServerPairingOffer,
  fetchSessionState,
  isPrimaryEnvironmentPairingCredentialRejectedError,
  isPrimaryEnvironmentRequestError,
  getServerShareState,
  listServerClientSessions,
  listServerPairingLinks,
  peekPairingTokenFromUrl,
  PRIMARY_PAIRING_OFFER_REQUEST_TIMEOUT_MS,
  PrimaryEnvironmentPairingCredentialRejectedError,
  PrimaryEnvironmentRequestError,
  reauthenticatePrimaryEnvironment,
  resolveInitialServerAuthGateState,
  revokeOtherServerClientSessions,
  revokeServerClientSession,
  revokeServerPairingLink,
  stripPairingTokenFromUrl,
  submitServerAuthCredential,
  takePairingTokenFromUrl,
  type ServerClientSessionRecord,
  type ServerPairingLinkRecord,
  __resetServerAuthBootstrapForTests,
  abandonServerAuthBootstrap,
} from "./auth";

export { refreshPrimarySessionState, usePrimarySessionState } from "./sessionState";

export { PrimaryEnvironmentHttpClient } from "./httpClient";

export {
  DesktopEnvironmentBootstrapIncompleteError,
  isDesktopEnvironmentBootstrapIncompleteError,
  isPrimaryEnvironmentProtocolUnsupportedError,
  isPrimaryEnvironmentUrlInvalidError,
  PrimaryEnvironmentProtocolUnsupportedError,
  PrimaryEnvironmentUrlInvalidError,
  readPrimaryEnvironmentTarget,
  resolvePrimaryEnvironmentHttpUrl,
  isLoopbackHostname,
  type PrimaryEnvironmentTarget,
} from "./target";
