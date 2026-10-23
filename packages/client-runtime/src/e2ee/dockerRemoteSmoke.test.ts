// Opt-in cross-container boundary test. The server and client are started by
// docs/testing/cross-platform-validation.md.
// @effect-diagnostics globalFetch:off - Every request targets the explicit Docker server URL.
// @effect-diagnostics nodeBuiltinImport:off - The opt-in smoke drives raw TCP and real-time waits outside Effect.
import {
  AuthAccessTokenResult,
  AuthAccessTokenType,
  AuthBrowserSessionResult,
  AuthClientSession,
  AuthClientSessionRevokeResult,
  AuthEnvironmentBootstrapTokenType,
  AuthPairingOfferResult,
  AuthPairingOfferCancellationResult,
  AuthSessionState,
  AuthShareStateResult,
  AuthTokenExchangeGrantType,
  AuthWebSocketTicketResult,
  E2eeAuthenticatedMessage,
  ExecutionEnvironmentDescriptor,
  RemoteUpdateInstallError,
  RemoteUpdateSnapshot,
} from "@bibcode/contracts";
import { parsePairingCode } from "@bibcode/shared/pairingCode";
import { describe, expect, it } from "@effect/vitest";
import * as Schema from "effect/Schema";
import * as NodeNet from "node:net";
import * as NodeTimersPromises from "node:timers/promises";

import { MAX_E2EE_CHUNK_BYTES, plaintextRecords } from "./frame.ts";
import { decodeBase64UrlKey } from "./noise.ts";
import {
  type EncryptedTestSocket,
  openEncryptedTestSocket,
  requestTestRpc,
} from "./testSupport.ts";

const serverUrl = process.env["BIBCODE_DOCKER_SERVER_URL"];
const adminCredential = process.env["BIBCODE_DOCKER_ADMIN_CREDENTIAL"];

const decodeAccessToken = Schema.decodeUnknownSync(AuthAccessTokenResult);
const decodeDescriptor = Schema.decodeUnknownSync(ExecutionEnvironmentDescriptor);
const decodeE2eeAuthenticated = Schema.decodeUnknownSync(E2eeAuthenticatedMessage);
const decodeRevoke = Schema.decodeUnknownSync(AuthClientSessionRevokeResult);
const decodeShareState = Schema.decodeUnknownSync(AuthShareStateResult);
const decodeUpdateInstallError = Schema.decodeUnknownSync(RemoteUpdateInstallError);
const decodeUpdateSnapshot = Schema.decodeUnknownSync(RemoteUpdateSnapshot);
const decodeBrowserSession = Schema.decodeUnknownSync(Schema.toCodecJson(AuthBrowserSessionResult));
const decodeWebSocketTicket = Schema.decodeUnknownSync(
  Schema.toCodecJson(AuthWebSocketTicketResult),
);
const decodeClientSessions = Schema.decodeUnknownSync(
  Schema.toCodecJson(Schema.Array(AuthClientSession)),
);
const decodePairingOffer = Schema.decodeUnknownSync(Schema.toCodecJson(AuthPairingOfferResult));
const decodePairingOfferCancellation = Schema.decodeUnknownSync(
  Schema.toCodecJson(AuthPairingOfferCancellationResult),
);
const decodeSession = Schema.decodeUnknownSync(Schema.toCodecJson(AuthSessionState));
const E2EE_RECORD_FLAG_CONTINUATION = 0x01;
const MAX_E2EE_RECORDS_PER_MESSAGE = 2_048;
const MAX_PLAIN_WEBSOCKET_FRAME_BYTES = 16 * 1024 * 1024;
const MAXIMUM_RECORD_PROGRESS_DELAY_MS = 5_250;

const RpcSuccess = Schema.TaggedStruct("Exit", {
  requestId: Schema.String,
  exit: Schema.TaggedStruct("Success", {
    value: Schema.Unknown,
  }),
});
const RpcFailure = Schema.TaggedStruct("Exit", {
  requestId: Schema.String,
  exit: Schema.TaggedStruct("Failure", {
    cause: Schema.Array(
      Schema.TaggedStruct("Fail", {
        error: Schema.Unknown,
      }),
    ),
  }),
});
const decodeRpcSuccess = Schema.decodeUnknownSync(RpcSuccess);
const decodeRpcFailure = Schema.decodeUnknownSync(RpcFailure);

function decodeSensitiveResponse<A>(
  label: string,
  decode: (input: unknown) => A,
  input: unknown,
): A {
  try {
    return decode(input);
  } catch {
    // The Docker runner must never print credentials, pairing codes, or
    // WebSocket tickets through a schema error's inspected input.
    throw new Error(`${label} response failed schema validation`);
  }
}

async function fetchJson(path: string, init?: RequestInit): Promise<unknown> {
  const response = await fetch(`${serverUrl!}${path}`, init);
  const body: unknown = await response.json();
  expect(response.ok, `${response.status} request failed for ${path}`).toBe(true);
  return body;
}

async function exchangeAdministrativeCredential(): Promise<string> {
  const access = decodeSensitiveResponse(
    "administrative token exchange",
    decodeAccessToken,
    await fetchJson("/oauth/token", {
      method: "POST",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({
        grant_type: AuthTokenExchangeGrantType,
        subject_token: adminCredential!,
        subject_token_type: AuthEnvironmentBootstrapTokenType,
        requested_token_type: AuthAccessTokenType,
        client_label: "docker-administrator",
      }),
    }),
  );
  expect(access.scope).toContain("access:write");

  const session = decodeSession(
    await fetchJson("/api/auth/session", {
      headers: { authorization: `Bearer ${access.access_token}` },
    }),
  );
  expect(session).toMatchObject({
    authenticated: true,
    sessionMethod: "bearer-access-token",
  });
  return access.access_token;
}

async function createOffHostOffer(administrator: string, label: string) {
  const offer = decodeSensitiveResponse(
    "pairing offer",
    decodePairingOffer,
    await fetchJson("/api/auth/pairing-offer", {
      method: "POST",
      headers: {
        authorization: `Bearer ${administrator}`,
        "content-type": "application/json",
      },
      body: JSON.stringify({
        name: label,
        label,
        endpoint: serverUrl,
        reach: "another-device",
      }),
    }),
  );
  let payload: ReturnType<typeof parsePairingCode>;
  try {
    payload = parsePairingCode(offer.code);
  } catch {
    throw new Error("pairing offer contained an invalid pairing code");
  }
  expect(payload.endpoint).toBe(serverUrl);
  expect(payload.name).toBe(label);
  expect(payload.reach).toBe("another-device");
  return payload;
}

async function assertDescriptor(): Promise<void> {
  const descriptor = decodeDescriptor(await fetchJson("/.well-known/bibcode/environment"));
  expect(descriptor.capabilities.remoteUpdateControl).toBe(true);
  expect(descriptor.remoteProtocolVersion).toBeGreaterThanOrEqual(1);
  expect(descriptor.remoteUpdateSupport).toEqual({
    installMode: "manual",
    reason: "manual-update-required",
    installKind: "unknown",
  });
}

async function assertRemoteUpdateRpc(channel: EncryptedTestSocket): Promise<void> {
  for (const [requestId, tag] of [
    ["1", "updater.status"],
    ["2", "updater.check"],
  ] as const) {
    const response = decodeRpcSuccess(await requestTestRpc(channel, requestId, tag));
    expect(response.requestId).toBe(requestId);
    expect(decodeUpdateSnapshot(response.exit.value)).toMatchObject({
      state: "idle",
      support: { installMode: "manual", reason: "manual-update-required", installKind: "unknown" },
      downloadPercent: null,
      targetVersion: null,
      installStage: null,
    });
  }

  const install = decodeRpcFailure(await requestTestRpc(channel, "3", "updater.install"));
  expect(install.requestId).toBe("3");
  expect(install.exit.cause).toHaveLength(1);
  expect(decodeUpdateInstallError(install.exit.cause[0]!.error)).toMatchObject({
    _tag: "RemoteUpdateInstallError",
    code: "remote_update_manual_required",
  });

  const statusAfterFailure = decodeRpcSuccess(await requestTestRpc(channel, "4", "updater.status"));
  expect(decodeUpdateSnapshot(statusAfterFailure.exit.value)).toMatchObject({
    state: "idle",
    support: { installMode: "manual", reason: "manual-update-required", installKind: "unknown" },
    downloadPercent: null,
    targetVersion: null,
    installStage: null,
  });
}

async function openAuthenticatedBearer(
  hostKey: Uint8Array,
  credential: string,
): Promise<EncryptedTestSocket> {
  const channel = await openEncryptedTestSocket(serverUrl!, hostKey);
  channel.sendMessage(JSON.stringify({ type: "e2ee_auth", bearer: credential }));
  const authenticated = decodeSensitiveResponse(
    "bearer authentication",
    decodeE2eeAuthenticated,
    JSON.parse(await channel.nextMessage()),
  );
  expect(authenticated.type).toBe("e2ee_authenticated");
  expect("credential" in authenticated).toBe(false);
  return channel;
}

async function assertPendingPairingCannotReconnect(
  hostKey: Uint8Array,
  credential: string,
): Promise<void> {
  const channel = await openEncryptedTestSocket(serverUrl!, hostKey);
  try {
    channel.sendMessage(JSON.stringify({ type: "e2ee_auth", bearer: credential }));
    expect(JSON.parse(await channel.nextMessage())).toEqual({
      type: "e2ee_error",
      code: "unauthorized",
    });
  } finally {
    channel.close();
  }
}

async function assertMaximumRecordProgress(channel: EncryptedTestSocket): Promise<void> {
  const requestId = "5";
  const request = new TextEncoder().encode(
    JSON.stringify({
      _tag: "Request",
      id: requestId,
      tag: "server.getConfig",
      payload: { ignored: "x".repeat(MAX_E2EE_CHUNK_BYTES) },
      headers: [],
    }),
  );
  const records = [...plaintextRecords(request)];
  expect(records).toHaveLength(2);
  expect(records[0]).toHaveLength(MAX_E2EE_CHUNK_BYTES + 1);

  channel.sendRecords([records[0]!]);
  await NodeTimersPromises.setTimeout(MAXIMUM_RECORD_PROGRESS_DELAY_MS);
  channel.sendRecords([records[1]!]);

  const response = decodeRpcSuccess(await channel.nextMessage().then(JSON.parse));
  expect(response.requestId).toBe(requestId);
}

interface SilentPreauthSocket {
  readonly socket: WebSocket;
  readonly closed: Promise<CloseEvent>;
}

async function openSilentPreauthSocket(): Promise<SilentPreauthSocket> {
  const socket = new WebSocket(`${serverUrl!.replace(/^http/, "ws")}/ws-e2ee`);
  const closed = new Promise<CloseEvent>((resolve) => {
    socket.addEventListener("close", (event) => resolve(event), { once: true });
  });
  await new Promise<void>((resolve, reject) => {
    socket.addEventListener("open", () => resolve(), { once: true });
    socket.addEventListener("error", () => reject(new Error("pre-auth socket failed to open")), {
      once: true,
    });
  });
  return { socket, closed };
}

async function waitForClose(socket: SilentPreauthSocket): Promise<CloseEvent> {
  return await Promise.race([
    socket.closed,
    NodeTimersPromises.setTimeout(5_000).then(() => {
      throw new Error("timed out waiting for pre-auth socket close");
    }),
  ]);
}

async function assertPreauthPeerOverflowIsRejected(): Promise<void> {
  const admitted: SilentPreauthSocket[] = [];
  try {
    for (let index = 0; index < 4; index += 1) {
      admitted.push(await openSilentPreauthSocket());
      // The WebSocket open event can precede the server task acquiring its
      // peer lease. Give the local cross-container hop time to publish each
      // admission before the fifth socket probes the per-peer limit.
      await NodeTimersPromises.setTimeout(25);
    }
    const overflow = await openSilentPreauthSocket();
    const close = await waitForClose(overflow);
    expect(close.code).toBe(1013);
    expect(close.reason).toBe("busy");
  } finally {
    const closes = admitted.map((socket) => waitForClose(socket));
    for (const { socket } of admitted) socket.close();
    await Promise.all(closes);
    // Let the server observe every close before the next smoke-test phase
    // opens its authenticated socket.
    await NodeTimersPromises.setTimeout(25);
  }
}

async function assertPlainRouteFrameCap(administrator: string): Promise<void> {
  const ticket = decodeSensitiveResponse(
    "WebSocket ticket",
    decodeWebSocketTicket,
    await fetchJson("/api/auth/websocket-ticket", {
      method: "POST",
      headers: { authorization: `Bearer ${administrator}` },
    }),
  );
  const url = new URL("/ws", serverUrl!);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  url.searchParams.set("wsTicket", ticket.ticket);

  const socket = new WebSocket(url);
  const closed = new Promise<CloseEvent>((resolve) => {
    socket.addEventListener("close", (event) => resolve(event), { once: true });
  });
  try {
    await new Promise<void>((resolve, reject) => {
      socket.addEventListener("open", () => resolve(), { once: true });
      socket.addEventListener("error", () => reject(new Error("plain WebSocket failed to open")), {
        once: true,
      });
    });
    socket.send(new Uint8Array(MAX_PLAIN_WEBSOCKET_FRAME_BYTES + 1));
    const close = await Promise.race([
      closed,
      NodeTimersPromises.setTimeout(5_000).then(() => {
        throw new Error("timed out waiting for oversized plain frame rejection");
      }),
    ]);
    expect(close.code).not.toBe(1000);
  } finally {
    if (socket.readyState < WebSocket.CLOSING) socket.close();
  }
}

async function assertRecordCountOverflowCloses(channel: EncryptedTestSocket): Promise<void> {
  try {
    const continuation = Uint8Array.of(E2EE_RECORD_FLAG_CONTINUATION, 0x78);
    channel.sendRecords(
      Array.from({ length: MAX_E2EE_RECORDS_PER_MESSAGE + 1 }, () => continuation),
    );
    await expect(channel.nextMessage()).rejects.toThrow("E2EE WebSocket closed");
  } finally {
    channel.close();
  }
}

async function shareState(administrator: string) {
  return decodeShareState(
    await fetchJson("/api/auth/share-state", {
      headers: { authorization: `Bearer ${administrator}` },
    }),
  );
}

async function revokeClient(administrator: string, sessionId: string): Promise<void> {
  const revoked = decodeRevoke(
    await fetchJson("/api/auth/clients/revoke", {
      method: "POST",
      headers: {
        authorization: `Bearer ${administrator}`,
        "content-type": "application/json",
      },
      body: JSON.stringify({ sessionId }),
    }),
  );
  expect(revoked.revoked).toBe(true);
}

async function assertBrowserPairingRetainsAndRevokesExposure(
  administrator: string,
  e2eeLabel: string,
  liveE2eeChannel: EncryptedTestSocket,
): Promise<void> {
  const browserLabel = "docker-browser";
  const browserPayload = await createOffHostOffer(administrator, browserLabel);
  const browser = decodeSensitiveResponse(
    "browser pairing",
    decodeBrowserSession,
    await fetchJson("/api/auth/browser-session", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ credential: browserPayload.token }),
    }),
  );
  expect(browser.sessionMethod).toBe("browser-session-cookie");
  expect(await shareState(administrator)).toMatchObject({
    desiredExposure: "wide",
    offHostGrantCount: 2,
  });

  const sessions = decodeClientSessions(
    await fetchJson("/api/auth/clients", {
      headers: { authorization: `Bearer ${administrator}` },
    }),
  );
  const e2eeSession = sessions.find((session) => session.client.label === e2eeLabel);
  const browserSession = sessions.find(
    (session) =>
      session.client.label === browserLabel && session.method === "browser-session-cookie",
  );
  expect(e2eeSession).toBeDefined();
  expect(browserSession).toBeDefined();

  await revokeClient(administrator, e2eeSession!.sessionId);
  await expect(liveE2eeChannel.nextMessage()).rejects.toThrow("E2EE WebSocket closed");
  expect(await shareState(administrator)).toMatchObject({
    desiredExposure: "wide",
    offHostGrantCount: 1,
  });

  await revokeClient(administrator, browserSession!.sessionId);
  expect(await shareState(administrator)).toEqual({
    desiredExposure: "loopback",
    offHostGrantCount: 0,
    legacyGrantCount: 0,
  });
}

async function assertAmbiguousOfferCancellation(administrator: string): Promise<void> {
  const idempotencyKey = "docker-lost-response";
  const create = () =>
    fetch(`${serverUrl!}/api/auth/pairing-offer`, {
      method: "POST",
      headers: {
        authorization: `Bearer ${administrator}`,
        "content-type": "application/json",
        "idempotency-key": idempotencyKey,
      },
      body: JSON.stringify({
        name: "docker-cancelled",
        endpoint: serverUrl,
        reach: "another-device",
      }),
    });
  const abandonedRequest = await dispatchPairingOfferWithoutReadingResponse({
    administrator,
    idempotencyKey,
  });
  try {
    let observedCommittedGrant = false;
    for (let attempt = 0; attempt < 120; attempt += 1) {
      const state = await shareState(administrator);
      if (state.desiredExposure === "wide" && state.offHostGrantCount === 1) {
        observedCommittedGrant = true;
        break;
      }
      await NodeTimersPromises.setTimeout(25);
    }
    expect(observedCommittedGrant).toBe(true);
  } finally {
    abandonedRequest.destroy();
  }

  const cancelled = decodePairingOfferCancellation(
    await fetchJson("/api/auth/pairing-offer/cancel", {
      method: "POST",
      headers: {
        authorization: `Bearer ${administrator}`,
        "content-type": "application/json",
      },
      body: JSON.stringify({ idempotencyKey }),
    }),
  );
  expect(cancelled.cancelled).toBe(true);
  expect(await shareState(administrator)).toEqual({
    desiredExposure: "loopback",
    offHostGrantCount: 0,
    legacyGrantCount: 0,
  });

  const delayed = await create();
  expect(delayed.status).toBe(400);
  expect(await delayed.json()).toMatchObject({ reason: "invalid_pairing_offer" });
}

async function dispatchPairingOfferWithoutReadingResponse(input: {
  administrator: string;
  idempotencyKey: string;
}): Promise<NodeNet.Socket> {
  const url = new URL("/api/auth/pairing-offer", serverUrl!);
  expect(url.protocol).toBe("http:");
  const body = JSON.stringify({
    name: "docker-cancelled",
    endpoint: serverUrl,
    reach: "another-device",
  });
  const request = [
    `POST ${url.pathname} HTTP/1.1`,
    `Host: ${url.host}`,
    `Authorization: Bearer ${input.administrator}`,
    "Content-Type: application/json",
    `Content-Length: ${String(Buffer.byteLength(body))}`,
    `Idempotency-Key: ${input.idempotencyKey}`,
    "Connection: close",
    "",
    body,
  ].join("\r\n");

  return await new Promise<NodeNet.Socket>((resolve, reject) => {
    let connected = false;
    const socket = NodeNet.connect(
      {
        host: url.hostname,
        port: url.port === "" ? 80 : Number(url.port),
      },
      () => {
        socket.setNoDelay(true);
        socket.write(request, () => {
          connected = true;
          resolve(socket);
        });
      },
    );
    socket.on("error", (error) => {
      if (!connected) reject(error);
    });
    // Intentionally do not attach a data/readable listener: the caller abandons
    // the response and must recover solely from the idempotency key.
  });
}

describe.skipIf(serverUrl === undefined || adminCredential === undefined)(
  "remote server Docker boundary",
  () => {
    it("confirms pairing, enforces transport caps, isolates updates, and converges exposure", async () => {
      await assertDescriptor();
      const administrator = await exchangeAdministrativeCredential();
      await assertPreauthPeerOverflowIsRejected();
      const e2eeLabel = "docker-e2ee";
      const payload = await createOffHostOffer(administrator, e2eeLabel);
      const hostKey = decodeBase64UrlKey(payload.hostKey);
      const pairingChannel = await openEncryptedTestSocket(serverUrl!, hostKey);
      let reconnect: EncryptedTestSocket | null = null;
      try {
        pairingChannel.sendMessage(
          JSON.stringify({
            type: "e2ee_auth",
            pairing: payload.token,
          }),
        );
        const authenticated = decodeSensitiveResponse(
          "in-channel pairing",
          decodeE2eeAuthenticated,
          JSON.parse(await pairingChannel.nextMessage()),
        );
        expect(authenticated.type).toBe("e2ee_authenticated");
        expect(authenticated.environmentId).toBeTypeOf("string");
        expect(authenticated.storageInstanceId).toBe(payload.storageInstanceId);
        if (!("credential" in authenticated) || typeof authenticated.credential !== "string") {
          throw new Error("in-channel pairing response omitted its credential");
        }
        const credential = authenticated.credential;

        expect(await shareState(administrator)).toMatchObject({
          desiredExposure: "wide",
          offHostGrantCount: 1,
        });
        expect(
          decodeRpcSuccess(await requestTestRpc(pairingChannel, "10", "server.getConfig"))
            .requestId,
        ).toBe("10");
        await assertPendingPairingCannotReconnect(hostKey, credential);

        const confirmed = decodeRpcSuccess(
          await requestTestRpc(pairingChannel, "11", "auth.confirmPairing"),
        );
        expect(confirmed).toEqual({
          _tag: "Exit",
          requestId: "11",
          exit: { _tag: "Success", value: {} },
        });
        expect(
          decodeRpcSuccess(await requestTestRpc(pairingChannel, "12", "auth.confirmPairing")).exit
            .value,
        ).toEqual({});

        await assertRemoteUpdateRpc(pairingChannel);
        await assertMaximumRecordProgress(pairingChannel);
        pairingChannel.close();
        await NodeTimersPromises.setTimeout(50);

        reconnect = await openAuthenticatedBearer(hostKey, credential);
        expect(
          decodeRpcSuccess(await requestTestRpc(reconnect, "6", "server.getConfig")).requestId,
        ).toBe("6");

        const recordOverflow = await openAuthenticatedBearer(hostKey, credential);
        await assertRecordCountOverflowCloses(recordOverflow);
        await assertPlainRouteFrameCap(administrator);
        await assertBrowserPairingRetainsAndRevokesExposure(administrator, e2eeLabel, reconnect);
        await assertAmbiguousOfferCancellation(administrator);
        expect(await shareState(administrator)).toEqual({
          desiredExposure: "loopback",
          offHostGrantCount: 0,
          legacyGrantCount: 0,
        });
      } finally {
        pairingChannel.close();
        reconnect?.close();
      }
    }, 60_000);
  },
);
