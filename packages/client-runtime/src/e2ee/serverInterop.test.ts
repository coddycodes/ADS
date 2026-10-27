// Cross-language E2EE interop: TypeScript Noise NK initiator against the real
// Rust responder. Opt-in: requires a freshly built server binary.
//
//   cargo build -p coddy-studio-server
//   CODDY_STUDIO_E2EE_SERVER_BIN=$PWD/target/debug/coddy-studio vp test run src/e2ee/serverInterop.test.ts
//
// @effect-diagnostics nodeBuiltinImport:off
// @effect-diagnostics globalFetch:off - This opt-in harness probes a real loopback server process.
// @effect-diagnostics globalTimers:off - The process and WebSocket harness owns bounded watchdogs.
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";

import {
  AuthAccessTokenType,
  AuthEnvironmentBootstrapTokenType,
  AuthTokenExchangeGrantType,
  type RemotePairingCodePayload,
} from "@coddy/contracts";
import { parsePairingCode } from "@coddy/shared/pairingCode";
import { afterAll, beforeAll, describe, expect, it } from "@effect/vitest";

import { decodeBase64UrlKey } from "./noise.ts";
import {
  type EncryptedTestSocket,
  openEncryptedTestSocket,
  requestTestRpc,
} from "./testSupport.ts";
import {
  readHostPublicKey,
  type RunningServer,
  serverBinary,
  startServer,
  stopServer,
} from "./testServer.ts";

async function adminAccessToken(server: RunningServer): Promise<string> {
  if (server.adminAccessToken !== undefined) return server.adminAccessToken;
  const response = await fetch(`${server.httpBaseUrl}/oauth/token`, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: AuthTokenExchangeGrantType,
      subject_token: server.token,
      subject_token_type: AuthEnvironmentBootstrapTokenType,
      requested_token_type: AuthAccessTokenType,
    }),
  });
  const body = (await response.json()) as { access_token?: string };
  expect(response.ok, JSON.stringify(body)).toBe(true);
  expect(body.access_token).toBeTruthy();
  server.adminAccessToken = body.access_token!;
  return body.access_token!;
}

async function mintedPairing(server: RunningServer): Promise<{
  payload: RemotePairingCodePayload;
  hostKey: Uint8Array;
}> {
  const response = await fetch(`${server.httpBaseUrl}/api/auth/pairing-offer`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${await adminAccessToken(server)}`,
      "content-type": "application/json",
    },
    body: JSON.stringify({
      name: "Interop",
      // An off-host reach requires an off-host-classified endpoint; the test
      // channel still connects to the loopback server address.
      endpoint: "http://192.168.1.20:3773",
      reach: "another-device",
    }),
  });
  const offer = (await response.json()) as { code?: string };
  expect(response.ok, JSON.stringify(offer)).toBe(true);
  expect(offer.code).toBeTruthy();
  const payload = parsePairingCode(offer.code!);
  const hostKey = decodeBase64UrlKey(payload.hostKey);
  expect(hostKey).toEqual(readHostPublicKey(server.dataRoot));
  return { payload, hostKey };
}

const openEncrypted = (server: RunningServer, hostKey: Uint8Array): Promise<EncryptedTestSocket> =>
  openEncryptedTestSocket(server.httpBaseUrl, hostKey);

describe.skipIf(serverBinary === undefined)(
  "TypeScript initiator against the Rust responder",
  () => {
    let server: RunningServer;

    beforeAll(async () => {
      server = await startServer();
    }, 60_000);

    afterAll(async () => {
      if (server !== undefined) await stopServer(server);
    });

    it("mints, pairs in-channel, round-trips RPC, and reconnects with the minted bearer", async () => {
      const { payload, hostKey } = await mintedPairing(server);
      const channel = await openEncrypted(server, hostKey);
      // No confirmation flag is sent: the server decides delivery from the
      // grant, and the reply's pairingConfirmationRequired is the client's
      // only signal. Servers that predate the confirmation flow omit the
      // field and deliver immediately, so this suite gates both generations.
      channel.sendMessage(
        JSON.stringify({
          type: "e2ee_auth",
          pairing: payload.token,
        }),
      );
      const authenticated = JSON.parse(await channel.nextMessage()) as {
        type: string;
        credential?: string;
        storageInstanceId?: string;
        pairingConfirmationRequired?: boolean;
      };
      expect(authenticated.type).toBe("e2ee_authenticated");
      expect(authenticated.credential).toBeTruthy();
      expect(authenticated.storageInstanceId).toBe(payload.storageInstanceId);

      let nextRequestId = 1;
      if (authenticated.pairingConfirmationRequired === true) {
        const pending = await openEncrypted(server, hostKey);
        pending.sendMessage(
          JSON.stringify({ type: "e2ee_auth", bearer: authenticated.credential }),
        );
        expect(JSON.parse(await pending.nextMessage())).toEqual({
          type: "e2ee_error",
          code: "unauthorized",
        });
        pending.close();

        expect(
          await requestTestRpc(channel, String(nextRequestId), "auth.confirmPairing"),
        ).toMatchObject({
          _tag: "Exit",
          requestId: String(nextRequestId),
          exit: { _tag: "Success", value: {} },
        });
        nextRequestId += 1;
      }
      expect(
        await requestTestRpc(channel, String(nextRequestId), "server.getConfig"),
      ).toMatchObject({
        _tag: "Exit",
        requestId: String(nextRequestId),
      });
      channel.close();

      const second = await openEncrypted(server, hostKey);
      second.sendMessage(JSON.stringify({ type: "e2ee_auth", bearer: authenticated.credential }));
      expect(JSON.parse(await second.nextMessage())).toEqual({ type: "e2ee_authenticated" });
      second.close();
    }, 30_000);

    it("delivers ordered native PTY input over E2EE and rejects another socket's lease", async () => {
      const { payload, hostKey } = await mintedPairing(server);
      const channel = await openEncrypted(server, hostKey);
      let nextRequestId = 1;
      const nextId = () => String(nextRequestId++);
      const scope = { threadId: "interop-ordered-input", terminalId: "term-ordered" };
      const cwd = NodePath.join(server.dataRoot, "ordered-input-fixture");
      NodeFS.mkdirSync(cwd, { recursive: true });
      let opened = false;
      try {
        channel.sendMessage(JSON.stringify({ type: "e2ee_auth", pairing: payload.token }));
        const authenticated = JSON.parse(await channel.nextMessage()) as {
          type: string;
          credential: string;
          pairingConfirmationRequired?: boolean;
        };
        expect(authenticated.type).toBe("e2ee_authenticated");
        if (authenticated.pairingConfirmationRequired) {
          expect(await requestTestRpc(channel, nextId(), "auth.confirmPairing")).toMatchObject({
            exit: { _tag: "Success" },
          });
        }
        const config = (await requestTestRpc(channel, nextId(), "server.getConfig")) as {
          exit: { value: { environment: { platform: { os: string } } } };
        };
        expect(config).toMatchObject({
          exit: {
            _tag: "Success",
            value: { environment: { capabilities: { terminalOrderedInput: true } } },
          },
        });
        const isWindows = config.exit.value.environment.platform.os === "windows";
        const command = isWindows
          ? { executable: "cmd.exe", args: ["/D", "/Q", "/K"] }
          : { executable: "/bin/sh", args: ["-s"] };
        expect(
          await requestTestRpc(channel, nextId(), "terminal.open", {
            ...scope,
            cwd,
            cols: 80,
            rows: 24,
            command,
          }),
        ).toMatchObject({ exit: { _tag: "Success" } });
        opened = true;
        const begun = (await requestTestRpc(channel, nextId(), "terminal.beginInput", {
          ...scope,
          attachmentSequence: 0,
        })) as { exit: { _tag: string; value: { inputId: string } } };
        expect(begun.exit._tag).toBe("Success");
        const inputId = begun.exit.value.inputId;
        const frames = ["echo ordered-", "input-ok > ordered-input.txt", isWindows ? "\r" : "\n"];
        const expected = new Map<string, number>();
        for (const sequence of [2, 1, 0]) {
          const id = nextId();
          expected.set(id, sequence);
          channel.sendMessage(
            JSON.stringify({
              _tag: "Request",
              id,
              tag: "terminal.writeInput",
              payload: { ...scope, inputId, sequence, data: frames[sequence] },
              headers: [],
            }),
          );
        }
        for (let index = 0; index < frames.length; index++) {
          const response = JSON.parse(await channel.nextMessage()) as { requestId: string };
          expect(response).toMatchObject({
            _tag: "Exit",
            exit: {
              _tag: "Success",
              value: { inputId, sequence: expected.get(response.requestId) },
            },
          });
          expect(expected.delete(response.requestId)).toBe(true);
        }
        expect(expected.size).toBe(0);
        const output = NodePath.join(cwd, "ordered-input.txt");
        await expect
          .poll(
            () => (NodeFS.existsSync(output) ? NodeFS.readFileSync(output, "utf8").trim() : ""),
            { timeout: 5_000 },
          )
          .toBe("ordered-input-ok");
        const other = await openEncrypted(server, hostKey);
        try {
          other.sendMessage(
            JSON.stringify({ type: "e2ee_auth", bearer: authenticated.credential }),
          );
          expect(JSON.parse(await other.nextMessage())).toEqual({ type: "e2ee_authenticated" });
          expect(
            await requestTestRpc(other, "1", "terminal.writeInput", {
              ...scope,
              inputId,
              sequence: 3,
              data: "foreign\n",
            }),
          ).toMatchObject({
            exit: {
              _tag: "Failure",
              cause: [{ _tag: "Fail", error: { _tag: "TerminalInputError", code: "closed" } }],
            },
          });
        } finally {
          other.close();
        }
      } finally {
        if (opened)
          await requestTestRpc(channel, nextId(), "terminal.close", scope).catch(() => undefined);
        channel.close();
      }
    }, 30_000);

    it("reassembles a fragmented client message across the language boundary", async () => {
      const { payload, hostKey } = await mintedPairing(server);
      const channel = await openEncrypted(server, hostKey);
      channel.sendMessage(JSON.stringify({ type: "e2ee_auth", pairing: payload.token }));
      await channel.nextMessage();
      expect(
        await requestTestRpc(channel, "2", "server.getConfig", {
          ignored: "x".repeat(200_000),
        }),
      ).toMatchObject({ requestId: "2" });
      channel.close();
    }, 30_000);

    it("rejects a bad pairing token inside the encrypted channel", async () => {
      const { hostKey } = await mintedPairing(server);
      const channel = await openEncrypted(server, hostKey);
      channel.sendMessage(JSON.stringify({ type: "e2ee_auth", pairing: "bogus" }));
      expect(JSON.parse(await channel.nextMessage())).toEqual({
        type: "e2ee_error",
        code: "unauthorized",
      });
      channel.close();
    }, 30_000);
  },
);
