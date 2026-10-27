import { AuthSessionId } from "@coddy/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as DateTime from "effect/DateTime";

import {
  applyAuthAccessStreamEvent,
  EMPTY_AUTH_ACCESS_SNAPSHOT,
  projectAuthAccessSnapshot,
} from "./auth.ts";

function desktopSession(sessionId: string, current: boolean) {
  return {
    sessionId: AuthSessionId.make(sessionId),
    subject: "desktop-bootstrap",
    scopes: ["orchestration:read"],
    method: "bearer-access-token",
    client: {
      label: "Coddy Studio Tauri Desktop",
      deviceType: "desktop",
    },
    issuedAt: DateTime.makeUnsafe("2036-04-07T00:00:00.000Z"),
    expiresAt: DateTime.makeUnsafe("2036-05-07T00:00:00.000Z"),
    lastConnectedAt: null,
    connected: current,
    current,
  } as const;
}

describe("projectAuthAccessSnapshot", () => {
  it("keeps one row per session across duplicate delivery, snapshot replacement, and removal", () => {
    const stale = desktopSession("session-stale", false);
    const live = desktopSession("session-live", true);
    const upsert = (revision: number, payload: typeof live) =>
      ({ version: 1, revision, type: "clientUpserted", payload }) as const;

    // Duplicate delivery of the same upsert is idempotent.
    const [afterFirst] = projectAuthAccessSnapshot(EMPTY_AUTH_ACCESS_SNAPSHOT, upsert(1, live));
    const [afterDuplicate, projected] = projectAuthAccessSnapshot(afterFirst, upsert(1, live));
    expect(afterDuplicate.clientSessions).toEqual([live]);
    expect(projected).toEqual([
      { version: 1, revision: 1, type: "snapshot", payload: afterDuplicate },
    ]);

    // A reconnect re-delivers an authoritative snapshot that replaces, never appends.
    const [afterReconnect] = projectAuthAccessSnapshot(afterDuplicate, {
      version: 1,
      revision: 2,
      type: "snapshot",
      payload: { pairingLinks: [], clientSessions: [stale, live] },
    });
    expect(afterReconnect.clientSessions).toEqual([stale, live]);
    const [afterSecondSnapshot] = projectAuthAccessSnapshot(afterReconnect, {
      version: 1,
      revision: 3,
      type: "snapshot",
      payload: { pairingLinks: [], clientSessions: [live] },
    });
    expect(afterSecondSnapshot.clientSessions).toEqual([live]);

    // Supersession removes the stale row and re-upserting the live row does not duplicate it.
    const [afterRemoval] = projectAuthAccessSnapshot(afterReconnect, {
      version: 1,
      revision: 4,
      type: "clientRemoved",
      payload: { sessionId: stale.sessionId },
    });
    const [afterReupsert] = projectAuthAccessSnapshot(afterRemoval, upsert(5, live));
    expect(afterReupsert.clientSessions).toEqual([live]);
  });
});

describe("applyAuthAccessStreamEvent", () => {
  it("accumulates rapid pairing-link and client updates into one snapshot", () => {
    const pairingLink = {
      id: "pairing-link",
      credential: "credential",
      scopes: ["orchestration:read"],
      subject: "subject",
      label: "Phone",
      createdAt: DateTime.makeUnsafe("2036-04-07T00:00:00.000Z"),
      expiresAt: DateTime.makeUnsafe("2036-04-07T00:05:00.000Z"),
    } as const;
    const clientSession = {
      sessionId: AuthSessionId.make("session-client"),
      subject: "subject",
      scopes: ["orchestration:read"],
      method: "browser-session-cookie",
      client: {
        label: "Phone",
        deviceType: "mobile",
      },
      issuedAt: DateTime.makeUnsafe("2036-04-07T00:00:00.000Z"),
      expiresAt: DateTime.makeUnsafe("2036-05-07T00:00:00.000Z"),
      lastConnectedAt: null,
      connected: true,
      current: false,
    } as const;

    const withPairingLink = applyAuthAccessStreamEvent(EMPTY_AUTH_ACCESS_SNAPSHOT, {
      version: 1,
      revision: 1,
      type: "pairingLinkUpserted",
      payload: pairingLink,
    });
    const withClient = applyAuthAccessStreamEvent(withPairingLink, {
      version: 1,
      revision: 2,
      type: "clientUpserted",
      payload: clientSession,
    });

    expect(withClient).toEqual({
      pairingLinks: [pairingLink],
      clientSessions: [clientSession],
    });
  });

  it("applies removals without disturbing unrelated access state", () => {
    const snapshot = applyAuthAccessStreamEvent(
      {
        pairingLinks: [
          {
            id: "pairing-link",
            credential: "credential",
            scopes: ["orchestration:read"],
            subject: "subject",
            label: "Phone",
            createdAt: DateTime.makeUnsafe("2036-04-07T00:00:00.000Z"),
            expiresAt: DateTime.makeUnsafe("2036-04-07T00:05:00.000Z"),
          },
        ],
        clientSessions: [],
      },
      {
        version: 1,
        revision: 2,
        type: "pairingLinkRemoved",
        payload: { id: "pairing-link" },
      },
    );

    expect(snapshot).toEqual(EMPTY_AUTH_ACCESS_SNAPSHOT);
  });
});
