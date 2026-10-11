import {
  AuthAccessWriteScope,
  AuthSessionId,
  type AuthAccessStreamSnapshotEvent,
} from "@bibcode/contracts";
import type { DesktopNetworkAccessSnapshot } from "../../src/state/desktopNetworkAccess";
import * as DateTime from "effect/DateTime";

// Root's owned auth query and limited bridge read this value before mount.
// This data never enters appAtomRegistry or any running product instance.
export function shareChangedSitesData(
  id: "settings-share-current-unreachable" | "settings-share-empty-access",
): {
  readonly access: AuthAccessStreamSnapshotEvent;
  readonly network: DesktopNetworkAccessSnapshot;
} {
  const timestamp = DateTime.makeUnsafe("2026-10-10T12:00:00.000Z");
  const expiration = DateTime.makeUnsafe("2030-01-01T00:00:00.000Z");
  const empty = id === "settings-share-empty-access";
  return {
    access: {
      version: 1,
      revision: 1,
      type: "snapshot",
      payload: {
        pairingLinks: empty
          ? []
          : [
              {
                id: "fixture-unreachable-link",
                credential: "INERT_COMPONENT_FIXTURE_NEVER_A_VALID_PAIRING_CREDENTIAL",
                label: "Unreachable fixture pairing link",
                subject: "fixture-subject",
                scopes: [AuthAccessWriteScope],
                createdAt: timestamp,
                expiresAt: expiration,
              },
            ],
        clientSessions: empty
          ? []
          : [
              {
                sessionId: AuthSessionId.make("fixture-current-client"),
                subject: "fixture-subject",
                scopes: [AuthAccessWriteScope],
                method: "browser-session-cookie",
                client: { label: "Current fixture client", deviceType: "desktop" },
                issuedAt: timestamp,
                expiresAt: expiration,
                lastConnectedAt: timestamp,
                connected: true,
                current: true,
              },
            ],
      },
    },
    network: {
      advertisedEndpoints: [],
      serverExposureState: {
        configuredMode: "local-only",
        management: "native",
        mode: "local-only",
        endpointUrl: null,
        advertisedHost: null,
        tailscaleServeEnabled: false,
        tailscaleServePort: 443,
      },
    },
  };
}
