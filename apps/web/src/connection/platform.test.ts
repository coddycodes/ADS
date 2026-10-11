import {
  AuthStandardClientScopes,
  EnvironmentId,
  PRIMARY_LOCAL_ENVIRONMENT_ID,
  type DesktopBridge,
  type DesktopSshEnvironmentTarget,
} from "@bibcode/contracts";
import { describe, expect, it } from "@effect/vitest";
import { afterEach, vi } from "vite-plus/test";
import * as Data from "effect/Data";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Stream from "effect/Stream";
import * as TestClock from "effect/testing/TestClock";
import {
  ClientPresentation,
  CloudSession,
  EnvironmentOwnedDataCleanup,
  PlatformConnectionSource,
  PrimaryEnvironmentAuth,
  SshEnvironmentGateway,
} from "@bibcode/client-runtime/platform";
import {
  ConnectionBlockedError,
  ConnectionTransientError,
  Connectivity,
  Wakeups,
} from "@bibcode/client-runtime/connection";
import {
  EnvironmentRpcRequestObserver,
  type EnvironmentRpcRequestObservation,
} from "@bibcode/client-runtime/rpc";

// ── Controllable mock state ──────────────────────────────────────────
const pf = vi.hoisted(() => ({
  isHostedStatic: false,
  session: null as null | { readClerkToken: () => unknown },
  desktopPrimaryBearer: null as null | (() => Promise<string | null>),
  primaryTarget: null as unknown,
  secondaryRead: { _tag: "Success", bootstraps: [] as unknown[] } as unknown,
  descriptor: { environmentId: "environment-primary", label: "Primary" } as unknown,
  bearerAccess: { access_token: "secondary-token", expires_in: 3_600 } as unknown,
  descriptorCalls: [] as string[],
  bearerBootstrapCalls: [] as string[],
  clearCalls: [] as string[],
  previewForgetCalls: [] as string[],
  trackCalls: [] as Array<{ requestId: string; request: EnvironmentRpcRequestObservation }>,
  ackCalls: [] as string[],
}));

vi.mock("../hostedPairing", () => ({
  isHostedStaticApp: () => pf.isHostedStatic,
}));

vi.mock("../rpc/atomRegistry", () => ({
  appAtomRegistry: { get: () => pf.session },
}));

vi.mock("../rpc/requestLatencyState", () => ({
  trackRpcRequestSent: (requestId: string, request: EnvironmentRpcRequestObservation) => {
    pf.trackCalls.push({ requestId, request });
  },
  acknowledgeRpcRequest: (requestId: string) => {
    pf.ackCalls.push(requestId);
  },
}));

vi.mock("../browser/previewStorageCleanup", () => ({
  forgetPreviewStorage: (environmentId: string) => {
    pf.previewForgetCalls.push(environmentId);
    return Promise.resolve();
  },
}));

vi.mock("../composerDraftStore", () => ({
  clearComposerDraftsEnvironment: (environmentId: string) => {
    pf.clearCalls.push(environmentId);
  },
}));

vi.mock("../environments/primary/desktopAuth", () => ({
  readDesktopPrimaryBearerToken: () =>
    pf.desktopPrimaryBearer ? pf.desktopPrimaryBearer() : Promise.resolve(null),
}));

vi.mock("../environments/primary/httpLayer", async () => {
  const Layer = await import("effect/Layer");
  return { primaryEnvironmentHttpLayer: Layer.empty };
});

vi.mock("../environments/primary/target", async () => ({
  ...(await vi.importActual<typeof import("../environments/primary/target")>(
    "../environments/primary/target",
  )),
  readPrimaryEnvironmentTarget: () => {
    if (pf.primaryTarget instanceof Error) {
      throw pf.primaryTarget;
    }
    return pf.primaryTarget;
  },
}));

vi.mock("./desktopLocal", () => ({
  desktopLocalConnectionId: (backendId: string) => `local:${backendId}`,
  readDesktopSecondaryBootstrapsResult: () => pf.secondaryRead,
}));

vi.mock("./storage", async () => {
  const Layer = await import("effect/Layer");
  return { connectionStorageLayer: Layer.empty };
});

vi.mock("@bibcode/client-runtime/relay", async () => {
  const Stream = await import("effect/Stream");
  return {
    managedRelaySessionAtom: { _tag: "managedRelaySessionAtom" },
    managedRelayAccountChanges: () => Stream.empty,
  };
});

vi.mock("@bibcode/client-runtime/environment", () => ({
  fetchRemoteEnvironmentDescriptor: (input: { httpBaseUrl: string }) => {
    pf.descriptorCalls.push(input.httpBaseUrl);
    return Effect.succeed(pf.descriptor);
  },
}));

vi.mock("@bibcode/client-runtime/authorization", () => ({
  bootstrapRemoteBearerSession: (input: { httpBaseUrl: string }) => {
    pf.bearerBootstrapCalls.push(input.httpBaseUrl);
    return Effect.succeed(pf.bearerAccess);
  },
}));

import {
  canRetainCachedPlatformRegistrationAfterRefreshFailure,
  canReuseCachedPlatformRegistration,
  connectionPlatformLayer,
  primaryRegistrationToRetainAfterTopologyRead,
  provisionDesktopSshEnvironment,
  readPrimaryEnvironmentTargetResult,
  secondaryRegistrationsToRetainAfterTopologyRead,
  secondaryBearerExpiresAtEpochMs,
  secondaryBearerRefreshAtEpochMs,
} from "./platform.ts";
import { clientCollector, PROFILER_CORRELATION_HEADER } from "../profiler/clientCollector";

const TARGET: DesktopSshEnvironmentTarget = {
  alias: "devbox",
  hostname: "devbox.example.test",
  username: "developer",
  port: 22,
};

const PREPARE_INPUT = {
  connectionId: "ssh-connection",
  expectedEnvironmentId: EnvironmentId.make("environment-ssh"),
  target: TARGET,
};

class ReadClerkTokenError extends Data.TaggedError("ReadClerkTokenError")<{
  readonly message: string;
}> {}

interface BridgeOptions {
  readonly failDescriptor?: boolean;
  readonly failEnsure?: unknown;
  readonly pairingToken?: string | null;
  readonly failBearer?: boolean;
  readonly failDisconnect?: boolean;
  /** Receives the scopes of every exchange. */
  readonly exchangedScopes?: Array<ReadonlyArray<string> | undefined>;
}

/** The desktop's rejection of a consumed one-time token, verbatim. */
const CONSUMED_TOKEN_REJECTION =
  "[ssh_http:401] SSH remote API request failed during bootstrap-bearer-session.";

/**
 * Behaves like the desktop host: every `issuePairingToken: true` mints a new
 * one-time token, a cached tunnel never returns one, and the host refuses a
 * second exchange of the same token. Tauri rejects with a bare string.
 */
function makeBridge(calls: string[], options: BridgeOptions = {}): DesktopBridge {
  let minted = 0;
  const exchanged = new Set<string>();
  return {
    ensureSshEnvironment: async (
      target: DesktopSshEnvironmentTarget,
      ensureOptions?: { issuePairingToken?: boolean },
    ) => {
      const issue = ensureOptions?.issuePairingToken === true;
      calls.push(issue ? "ensure+token" : "ensure");
      if (options.failEnsure !== undefined) {
        throw options.failEnsure;
      }
      minted += issue ? 1 : 0;
      return {
        target,
        httpBaseUrl: "http://127.0.0.1:3201/",
        wsBaseUrl: "ws://127.0.0.1:3201/",
        pairingToken: !issue
          ? null
          : options.pairingToken === undefined
            ? `pairing-token-${minted}`
            : options.pairingToken,
      };
    },
    fetchSshEnvironmentDescriptor: async () => {
      calls.push("descriptor");
      if (options.failDescriptor === true) {
        throw new Error("descriptor unavailable");
      }
      return {
        environmentId: EnvironmentId.make("environment-ssh"),
        label: "SSH environment",
        platform: { os: "linux", arch: "x64" },
        serverVersion: "0.0.0-test",
        capabilities: { repositoryIdentity: true },
      };
    },
    bootstrapSshBearerSession: async (
      _httpBaseUrl: string,
      credential: string,
      scopes?: ReadonlyArray<string>,
    ) => {
      calls.push("token");
      options.exchangedScopes?.push(scopes);
      if (options.failBearer === true) {
        throw new Error("bearer denied");
      }
      if (exchanged.has(credential)) {
        throw CONSUMED_TOKEN_REJECTION;
      }
      exchanged.add(credential);
      return {
        access_token: `bearer-for-${credential}`,
        issued_token_type: "urn:ietf:params:oauth:token-type:access_token",
        token_type: "Bearer",
        expires_in: 3_600,
        scope: AuthStandardClientScopes.join(" "),
      };
    },
    disconnectSshEnvironment: async () => {
      calls.push("disconnect");
      if (options.failDisconnect === true) {
        throw new Error("disconnect failed");
      }
      return undefined;
    },
  } as unknown as DesktopBridge;
}

function stubBrowser(options: { desktopBridge?: DesktopBridge; platform?: string } = {}): void {
  vi.stubGlobal("window", options.desktopBridge ? { desktopBridge: options.desktopBridge } : {});
  vi.stubGlobal("navigator", {
    platform: options.platform ?? "Win32",
    onLine: true,
  });
  vi.stubGlobal("document", {
    visibilityState: "visible",
    hasFocus: () => true,
    addEventListener: () => undefined,
    removeEventListener: () => undefined,
  });
}

interface DomStubs {
  readonly windowListeners: (type: string) => Array<() => void>;
  readonly documentListeners: (type: string) => Array<() => void>;
  readonly fireWindow: (type: string) => void;
  readonly fireDocument: (type: string) => void;
}

function makeDomStubs(options: { desktopBridge?: DesktopBridge } = {}): DomStubs {
  const windowListeners = new Map<string, Array<() => void>>();
  const documentListeners = new Map<string, Array<() => void>>();
  const add = (map: Map<string, Array<() => void>>) => (type: string, handler: () => void) => {
    const bucket = map.get(type) ?? [];
    bucket.push(handler);
    map.set(type, bucket);
  };
  const remove = (map: Map<string, Array<() => void>>) => (type: string, handler: () => void) => {
    const bucket = map.get(type);
    if (!bucket) return;
    const index = bucket.indexOf(handler);
    if (index >= 0) bucket.splice(index, 1);
  };
  vi.stubGlobal("window", {
    desktopBridge: options.desktopBridge,
    addEventListener: add(windowListeners),
    removeEventListener: remove(windowListeners),
  });
  vi.stubGlobal("document", {
    visibilityState: "visible",
    hasFocus: () => true,
    addEventListener: add(documentListeners),
    removeEventListener: remove(documentListeners),
  });
  vi.stubGlobal("navigator", { platform: "Win32", onLine: true });
  return {
    windowListeners: (type) => windowListeners.get(type) ?? [],
    documentListeners: (type) => documentListeners.get(type) ?? [],
    fireWindow: (type) => {
      for (const handler of Array.from(windowListeners.get(type) ?? [])) handler();
    },
    fireDocument: (type) => {
      for (const handler of Array.from(documentListeners.get(type) ?? [])) handler();
    },
  };
}

function waitFor(check: () => boolean) {
  return Effect.gen(function* () {
    for (let index = 0; index < 2_000; index += 1) {
      if (check()) return;
      // Cooperative yield (clock-agnostic: it.effect runs on a frozen TestClock).
      yield* Effect.yieldNow;
    }
    throw new Error("Timed out waiting for a stubbed DOM listener to register.");
  });
}

function resetPf(): void {
  pf.isHostedStatic = false;
  pf.session = null;
  pf.desktopPrimaryBearer = null;
  pf.primaryTarget = null;
  pf.secondaryRead = { _tag: "Success", bootstraps: [] };
  pf.descriptor = { environmentId: "environment-primary", label: "Primary" };
  pf.bearerAccess = { access_token: "secondary-token", expires_in: 3_600 };
  pf.descriptorCalls = [];
  pf.bearerBootstrapCalls = [];
  pf.clearCalls.length = 0;
  pf.previewForgetCalls.length = 0;
  pf.trackCalls.length = 0;
  pf.ackCalls.length = 0;
}

afterEach(() => {
  vi.unstubAllGlobals();
  resetPf();
});

// ─────────────────────────────────────────────────────────────────────
// Existing pure-function coverage (unchanged behavior)
// ─────────────────────────────────────────────────────────────────────

describe("desktop SSH pairing", () => {
  it.effect("fetches the descriptor before consuming the one-time credential", () =>
    Effect.gen(function* () {
      const calls: string[] = [];
      const provisioned = yield* provisionDesktopSshEnvironment(makeBridge(calls), TARGET);
      expect(provisioned.environmentId).toBe(EnvironmentId.make("environment-ssh"));
      expect(calls).toEqual(["ensure+token", "descriptor", "token"]);
    }),
  );

  it.effect("does not consume the credential when descriptor discovery fails", () =>
    Effect.gen(function* () {
      const calls: string[] = [];
      yield* provisionDesktopSshEnvironment(
        makeBridge(calls, { failDescriptor: true }),
        TARGET,
      ).pipe(Effect.flip);
      expect(calls).toEqual(["ensure+token", "descriptor"]);
    }),
  );

  it.effect("blocks provisioning when the SSH environment issues no pairing token", () =>
    Effect.gen(function* () {
      const calls: string[] = [];
      const error = yield* provisionDesktopSshEnvironment(
        makeBridge(calls, { pairingToken: null }),
        TARGET,
      ).pipe(Effect.flip);
      expect(error).toBeInstanceOf(ConnectionBlockedError);
      expect(calls).toEqual(["ensure+token"]);
    }),
  );

  it.effect("maps a cancelled preparation to an authentication block", () =>
    Effect.gen(function* () {
      const calls: string[] = [];
      const error = yield* provisionDesktopSshEnvironment(
        makeBridge(calls, {
          failEnsure: new Error("[ssh_cancelled] SSH authentication cancelled for devbox."),
        }),
        TARGET,
      ).pipe(Effect.flip);
      expect(error).toBeInstanceOf(ConnectionBlockedError);
    }),
  );

  it.effect("maps a non-cancel preparation failure to a transient error", () =>
    Effect.gen(function* () {
      const calls: string[] = [];
      const error = yield* provisionDesktopSshEnvironment(
        makeBridge(calls, { failEnsure: "boom" }),
        TARGET,
      ).pipe(Effect.flip);
      expect(error).toBeInstanceOf(ConnectionTransientError);
    }),
  );

  it.effect("propagates a bearer-session failure while preparing", () =>
    Effect.gen(function* () {
      const calls: string[] = [];
      const error = yield* provisionDesktopSshEnvironment(
        makeBridge(calls, { failBearer: true }),
        TARGET,
      ).pipe(Effect.flip);
      expect(error).toBeInstanceOf(ConnectionTransientError);
      expect(calls).toEqual(["ensure+token", "descriptor", "token"]);
    }),
  );
});

describe("desktop-local bearer cache", () => {
  const registration = {} as never;

  it("refreshes a secondary bearer before it expires", () => {
    const issuedAtEpochMs = 10_000;
    const refreshAtEpochMs = secondaryBearerRefreshAtEpochMs(issuedAtEpochMs, 60);
    const expiresAtEpochMs = secondaryBearerExpiresAtEpochMs(issuedAtEpochMs, 60);
    const cached = {
      expiresAtEpochMs,
      signature: "secondary-signature",
      registration,
      refreshAtEpochMs,
    };

    expect(refreshAtEpochMs).toBe(65_000);
    expect(canReuseCachedPlatformRegistration(cached, cached.signature, 64_999)).toBe(true);
    expect(canReuseCachedPlatformRegistration(cached, cached.signature, 65_000)).toBe(false);
    expect(
      canRetainCachedPlatformRegistrationAfterRefreshFailure(cached, cached.signature, 69_999),
    ).toBe(true);
    expect(
      canRetainCachedPlatformRegistrationAfterRefreshFailure(cached, cached.signature, 70_000),
    ).toBe(false);
  });

  it("does not cache credentials whose lifetime is shorter than the refresh skew", () => {
    const refreshAtEpochMs = secondaryBearerRefreshAtEpochMs(10_000, 3);
    const cached = {
      expiresAtEpochMs: secondaryBearerExpiresAtEpochMs(10_000, 3),
      signature: "secondary-signature",
      registration,
      refreshAtEpochMs,
    };

    expect(refreshAtEpochMs).toBe(10_000);
    expect(canReuseCachedPlatformRegistration(cached, cached.signature, 10_000)).toBe(false);
  });

  it("retains only unexpired secondaries after a topology read failure", () => {
    const valid = {
      expiresAtEpochMs: 20_000,
      signature: "valid-secondary",
      registration,
      refreshAtEpochMs: 15_000,
    };
    const previous = new Map([
      ["valid-secondary", valid],
      [
        "expired-secondary",
        {
          expiresAtEpochMs: 10_000,
          signature: "expired-secondary",
          registration,
          refreshAtEpochMs: 5_000,
        },
      ],
    ]);

    expect(
      secondaryRegistrationsToRetainAfterTopologyRead(
        previous,
        { _tag: "Failure", cause: new Error("IPC unavailable") },
        10_000,
      ),
    ).toEqual(new Map([["valid-secondary", valid]]));
  });

  it("treats a successful empty topology as authoritative removal", () => {
    const previous = new Map([
      [
        "secondary",
        {
          expiresAtEpochMs: 20_000,
          signature: "secondary",
          registration,
          refreshAtEpochMs: 15_000,
        },
      ],
    ]);

    expect(
      secondaryRegistrationsToRetainAfterTopologyRead(
        previous,
        { _tag: "Success", bootstraps: [] },
        10_000,
      ),
    ).toEqual(new Map());
  });
});

describe("primary topology cache", () => {
  const registration = {} as never;
  const cached = {
    signature: "primary|http://127.0.0.1:3773/|ws://127.0.0.1:3773/",
    registration,
  };
  const previous = new Map([[PRIMARY_LOCAL_ENVIRONMENT_ID, cached]]);

  it("captures synchronous primary target read failures", () => {
    const cause = new Error("invalid primary target");
    expect(
      readPrimaryEnvironmentTargetResult(() => {
        throw cause;
      }),
    ).toEqual({ _tag: "Failure", cause });
  });

  it("retains the cached primary after a transient topology read failure", () => {
    expect(
      primaryRegistrationToRetainAfterTopologyRead(previous, {
        _tag: "Failure",
        cause: new Error("IPC unavailable"),
      }),
    ).toBe(cached);
  });

  it("treats a successful primary absence as authoritative removal", () => {
    expect(
      primaryRegistrationToRetainAfterTopologyRead(previous, {
        _tag: "Success",
        target: null,
      }),
    ).toBeUndefined();
  });
});

// ─────────────────────────────────────────────────────────────────────
// connectionPlatformLayer — capability services and connection source
// ─────────────────────────────────────────────────────────────────────

describe("connectionPlatformLayer capabilities", () => {
  it.effect("builds the layer and exposes the platform capability services", () => {
    stubBrowser({ desktopBridge: makeBridge([]) });
    return Effect.gen(function* () {
      const presentation = yield* ClientPresentation;
      expect(presentation.metadata.label).toBe("BiBCode Desktop");
      expect(yield* ClientPresentation).toBeDefined();
      expect(yield* CloudSession).toBeDefined();
      expect(yield* PrimaryEnvironmentAuth).toBeDefined();
      expect(yield* SshEnvironmentGateway).toBeDefined();
      expect(yield* PlatformConnectionSource).toBeDefined();
      expect(yield* EnvironmentOwnedDataCleanup).toBeDefined();
      expect(yield* EnvironmentRpcRequestObserver).toBeDefined();
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("labels the web client and omits the os when the platform is blank", () => {
    stubBrowser({ platform: "" });
    return Effect.gen(function* () {
      const presentation = yield* ClientPresentation;
      expect(presentation.metadata.label).toBe("BiBCode Web");
      expect("os" in presentation.metadata).toBe(false);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });
});

describe("connectionPlatformLayer cloud session token", () => {
  it.effect("blocks when no relay session is signed in", () => {
    stubBrowser();
    pf.session = null;
    return Effect.gen(function* () {
      const cloud = yield* CloudSession;
      const error = yield* cloud.clerkToken.pipe(Effect.flip);
      expect(error).toBeInstanceOf(ConnectionBlockedError);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("returns the clerk token when the relay session yields one", () => {
    stubBrowser();
    pf.session = { readClerkToken: () => Effect.succeed("clerk-token") };
    return Effect.gen(function* () {
      const cloud = yield* CloudSession;
      expect(yield* cloud.clerkToken).toBe("clerk-token");
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("blocks when the relay session has no clerk token", () => {
    stubBrowser();
    pf.session = { readClerkToken: () => Effect.succeed(null) };
    return Effect.gen(function* () {
      const cloud = yield* CloudSession;
      const error = yield* cloud.clerkToken.pipe(Effect.flip);
      expect(error).toBeInstanceOf(ConnectionBlockedError);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("maps a clerk token read failure to a transient error", () => {
    stubBrowser();
    pf.session = {
      readClerkToken: () => Effect.fail(new ReadClerkTokenError({ message: "network down" })),
    };
    return Effect.gen(function* () {
      const cloud = yield* CloudSession;
      const error = yield* cloud.clerkToken.pipe(Effect.flip);
      expect(error).toBeInstanceOf(ConnectionTransientError);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });
});

describe("connectionPlatformLayer primary bearer credential", () => {
  it.effect("wraps the desktop primary bearer token in an option", () => {
    stubBrowser();
    pf.desktopPrimaryBearer = () => Promise.resolve("primary-bearer");
    return Effect.gen(function* () {
      const auth = yield* PrimaryEnvironmentAuth;
      const token = yield* auth.bearerToken;
      expect(Option.isSome(token)).toBe(true);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("reports no credential when the desktop returns null", () => {
    stubBrowser();
    pf.desktopPrimaryBearer = () => Promise.resolve(null);
    return Effect.gen(function* () {
      const auth = yield* PrimaryEnvironmentAuth;
      expect(Option.isNone(yield* auth.bearerToken)).toBe(true);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("maps a desktop credential read rejection to a transient error", () => {
    stubBrowser();
    pf.desktopPrimaryBearer = () => Promise.reject(new Error("keychain locked"));
    return Effect.gen(function* () {
      const auth = yield* PrimaryEnvironmentAuth;
      const error = yield* auth.bearerToken.pipe(Effect.flip);
      expect(error).toBeInstanceOf(ConnectionTransientError);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });
});

describe("connectionPlatformLayer ssh gateway", () => {
  it.effect("provisions through the desktop bridge", () => {
    const bridge = makeBridge([]);
    stubBrowser({ desktopBridge: bridge });
    return Effect.gen(function* () {
      const ssh = yield* SshEnvironmentGateway;
      const provisioned = yield* ssh.provision(TARGET);
      expect(provisioned.environmentId).toBe(EnvironmentId.make("environment-ssh"));
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("blocks provisioning when no desktop bridge is present", () => {
    stubBrowser();
    return Effect.gen(function* () {
      const ssh = yield* SshEnvironmentGateway;
      const error = yield* ssh.provision(TARGET).pipe(Effect.flip);
      expect(error).toBeInstanceOf(ConnectionBlockedError);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("provision then reconnect never re-exchanges a consumed token", () => {
    const calls: string[] = [];
    const bridge = makeBridge(calls);
    stubBrowser({ desktopBridge: bridge });
    return Effect.gen(function* () {
      const ssh = yield* SshEnvironmentGateway;
      const provisioned = yield* ssh.provision(TARGET);
      expect(provisioned.bearerToken).toBe("bearer-for-pairing-token-1");

      const tunnel = yield* ssh.ensureTunnel(PREPARE_INPUT);
      expect(tunnel.pairingToken).toBeNull();
      const minted = yield* ssh.mintBearer(PREPARE_INPUT);
      expect(minted.bearerToken).toBe("bearer-for-pairing-token-2");
      expect(calls).toEqual([
        "ensure+token",
        "descriptor",
        "token",
        "ensure",
        "ensure+token",
        "token",
      ]);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("requests the standard client scopes for every SSH exchange", () => {
    const exchangedScopes: Array<ReadonlyArray<string> | undefined> = [];
    const bridge = makeBridge([], { exchangedScopes });
    stubBrowser({ desktopBridge: bridge });
    return Effect.gen(function* () {
      const ssh = yield* SshEnvironmentGateway;
      yield* ssh.provision(TARGET);
      yield* ssh.mintBearer(PREPARE_INPUT);
      expect(exchangedScopes).toEqual([AuthStandardClientScopes, AuthStandardClientScopes]);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("reports a refused mint exchange with the blocked connect-again copy", () => {
    const bridge = makeBridge([], { pairingToken: "reused-token" });
    stubBrowser({ desktopBridge: bridge });
    return Effect.gen(function* () {
      const ssh = yield* SshEnvironmentGateway;
      yield* ssh.mintBearer(PREPARE_INPUT);
      const error = yield* ssh.mintBearer(PREPARE_INPUT).pipe(Effect.flip);
      expect(error).toBeInstanceOf(ConnectionBlockedError);
      expect(error).toMatchObject({
        reason: "authentication",
        detail:
          "devbox rejected a new pairing credential. Connect again; if it keeps failing, remove the environment and add it again.",
      });
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("blocks tunnels and mints when no desktop bridge is present", () => {
    stubBrowser();
    return Effect.gen(function* () {
      const ssh = yield* SshEnvironmentGateway;
      expect(yield* ssh.ensureTunnel(PREPARE_INPUT).pipe(Effect.flip)).toBeInstanceOf(
        ConnectionBlockedError,
      );
      expect(yield* ssh.mintBearer(PREPARE_INPUT).pipe(Effect.flip)).toBeInstanceOf(
        ConnectionBlockedError,
      );
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("blocks a mint when the bridge issues no pairing token", () => {
    const bridge = makeBridge([], { pairingToken: null });
    stubBrowser({ desktopBridge: bridge });
    return Effect.gen(function* () {
      const ssh = yield* SshEnvironmentGateway;
      const error = yield* ssh.mintBearer(PREPARE_INPUT).pipe(Effect.flip);
      expect(error).toBeInstanceOf(ConnectionBlockedError);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  for (const [failure, expected] of [
    [
      "[ssh_http:401] SSH remote API request failed during fetch-environment-descriptor.",
      { _tag: "ConnectionBlockedError", reason: "authentication" },
    ],
    [
      "[ssh_http:403] SSH remote API request failed during fetch-environment-descriptor.",
      { _tag: "ConnectionBlockedError", reason: "permission" },
    ],
    [
      "[ssh_http:400] SSH remote API request failed during fetch-environment-descriptor.",
      { _tag: "ConnectionBlockedError", reason: "configuration" },
    ],
    [
      "[ssh_http:503] SSH remote API request failed during fetch-environment-descriptor.",
      { _tag: "ConnectionTransientError", reason: "remote-unavailable" },
    ],
    [
      "Could not reach the environment API: connection refused",
      { _tag: "ConnectionTransientError", reason: "remote-unavailable" },
    ],
    [
      "[ssh_cancelled] SSH authentication cancelled for devbox.",
      {
        _tag: "ConnectionBlockedError",
        reason: "authentication",
        detail: "SSH authentication cancelled for devbox.",
      },
    ],
    [
      "SSH launch command failed with status exit status: 255: ssh: connect to host cancelbox port 22: Connection refused",
      { _tag: "ConnectionTransientError", reason: "remote-unavailable" },
    ],
    [
      "[ssh_timeout:pairing] The remote host did not issue a pairing credential within 30 seconds.",
      {
        _tag: "ConnectionTransientError",
        reason: "timeout",
        detail:
          "The remote host did not issue a pairing credential within 30 seconds. Check the connection; BiBCode keeps trying.",
      },
    ],
  ] as const) {
    it.effect(`classifies SSH failure ${failure.slice(0, 40)}`, () => {
      const bridge = makeBridge([], { failEnsure: failure });
      stubBrowser({ desktopBridge: bridge });
      return Effect.gen(function* () {
        const ssh = yield* SshEnvironmentGateway;
        const error = yield* ssh.ensureTunnel(PREPARE_INPUT).pipe(Effect.flip);
        expect(error).toMatchObject(expected);
      }).pipe(Effect.provide(connectionPlatformLayer));
    });
  }

  it.effect("disconnects through the desktop bridge when present", () => {
    const calls: string[] = [];
    const bridge = makeBridge(calls);
    stubBrowser({ desktopBridge: bridge });
    return Effect.gen(function* () {
      const ssh = yield* SshEnvironmentGateway;
      yield* ssh.disconnect(TARGET);
      expect(calls).toContain("disconnect");
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("is a no-op disconnect when no desktop bridge is present", () => {
    stubBrowser();
    return Effect.gen(function* () {
      const ssh = yield* SshEnvironmentGateway;
      yield* ssh.disconnect(TARGET);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("maps a disconnect failure to a transient error", () => {
    const bridge = makeBridge([], { failDisconnect: true });
    stubBrowser({ desktopBridge: bridge });
    return Effect.gen(function* () {
      const ssh = yield* SshEnvironmentGateway;
      const error = yield* ssh.disconnect(TARGET).pipe(Effect.flip);
      expect(error).toBeInstanceOf(ConnectionTransientError);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });
});

describe("connectionPlatformLayer environment side effects", () => {
  it.effect("clears composer drafts for an environment", () => {
    stubBrowser();
    return Effect.gen(function* () {
      const cleanup = yield* EnvironmentOwnedDataCleanup;
      yield* cleanup.clear(EnvironmentId.make("environment-x"));
      expect(pf.clearCalls).toContain("environment-x");
      // A removed environment's desktop preview storage goes with it.
      expect(pf.previewForgetCalls).toContain("environment-x");
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("tracks and acknowledges observed RPC requests", () => {
    stubBrowser();
    return Effect.gen(function* () {
      const observer = yield* EnvironmentRpcRequestObserver;
      const observation = yield* observer.observe({
        environmentId: EnvironmentId.make("environment-x"),
        method: "session.start",
      });
      expect(pf.trackCalls).toEqual([
        {
          requestId: expect.any(String),
          request: { method: "session.start", environmentId: "environment-x" },
        },
      ]);
      // No environment has its profiler on, so no correlation header is sent.
      expect(observation.headers).toEqual({});
      yield* observation.complete;
      expect(pf.ackCalls).toHaveLength(1);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("sends a profiler correlation header only to environments being profiled", () => {
    stubBrowser();
    return Effect.gen(function* () {
      const observer = yield* EnvironmentRpcRequestObserver;
      clientCollector.setEnabledEnvironments(new Set(["environment-profiled"]));
      try {
        const profiled = yield* observer.observe({
          environmentId: EnvironmentId.make("environment-profiled"),
          method: "server.getConfig",
        });
        const other = yield* observer.observe({
          environmentId: EnvironmentId.make("environment-x"),
          method: "server.getConfig",
        });
        expect(profiled.headers[PROFILER_CORRELATION_HEADER]).toMatch(/^[0-9a-f]{16}:\d+$/);
        expect(other.headers).toEqual({});
        yield* profiled.complete;
        yield* other.complete;
      } finally {
        clientCollector.setEnabledEnvironments(new Set());
      }
    }).pipe(Effect.provide(connectionPlatformLayer));
  });
});

describe("connectionPlatformLayer connectivity and wakeups", () => {
  it.effect("reports the current network status and wires connectivity listeners", () => {
    const dom = makeDomStubs();
    return Effect.gen(function* () {
      const connectivity = yield* Connectivity.Connectivity;
      expect(yield* connectivity.status).toBe("online");

      const fiber = yield* Effect.forkChild(Stream.runDrain(connectivity.changes));
      yield* waitFor(() => dom.windowListeners("online").length > 0);
      expect(dom.windowListeners("offline").length).toBeGreaterThan(0);
      // Exercise both browser online/offline listener bodies.
      dom.fireWindow("online");
      dom.fireWindow("offline");
      yield* Fiber.interrupt(fiber);
      // The release finalizer removed the listeners.
      expect(dom.windowListeners("online").length).toBe(0);
      expect(dom.windowListeners("offline").length).toBe(0);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("window focus does not emit a supervisor application-active wakeup", () => {
    const dom = makeDomStubs();
    return Effect.gen(function* () {
      const wakeups = yield* Wakeups.ConnectionWakeups;
      const seen: string[] = [];
      const fiber = yield* Effect.forkChild(
        Stream.runForEach(wakeups.changes, (event) =>
          Effect.sync(() => {
            seen.push(event);
          }),
        ),
      );
      yield* waitFor(() => dom.windowListeners("focus").length === 1);
      dom.fireWindow("blur");
      dom.fireWindow("focus");
      for (let i = 0; i < 20; i += 1) yield* Effect.yieldNow;
      expect(seen).toEqual([]);
      dom.fireDocument("visibilitychange");
      yield* waitFor(() => seen.length === 1);
      expect(seen).toEqual(["application-active"]);
      yield* Fiber.interrupt(fiber);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("shares and coalesces focus and visibility wakeups across subscribers", () => {
    const dom = makeDomStubs();
    return Effect.gen(function* () {
      const wakeups = yield* Wakeups.ConnectionWakeups;
      const seen: void[] = [];
      const first = yield* Effect.forkChild(
        Stream.runForEach(wakeups.focusVisibility, (event) =>
          Effect.sync(() => {
            seen.push(event);
          }),
        ),
      );
      const second = yield* Effect.forkChild(Stream.runDrain(wakeups.focusVisibility));
      yield* waitFor(() => dom.windowListeners("focus").length === 1);
      expect(dom.documentListeners("visibilitychange")).toHaveLength(1);
      dom.fireWindow("blur");
      Object.defineProperty(document, "visibilityState", { value: "hidden", configurable: true });
      dom.fireDocument("visibilitychange");
      Object.defineProperty(document, "visibilityState", { value: "visible", configurable: true });
      dom.fireDocument("visibilitychange");
      dom.fireWindow("focus");
      dom.fireDocument("visibilitychange");
      yield* waitFor(() => seen.length === 1);
      dom.fireWindow("blur");
      dom.fireWindow("focus");
      yield* waitFor(() => seen.length === 2);
      expect(seen).toHaveLength(2);
      yield* Fiber.interrupt(first);
      expect(dom.windowListeners("focus")).toHaveLength(1);
      yield* Fiber.interrupt(second);
      yield* waitFor(() => dom.windowListeners("focus").length === 0);
      expect(dom.documentListeners("visibilitychange")).toHaveLength(0);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("a stalled focus consumer cannot delay supervisor visibility wakeups", () => {
    const dom = makeDomStubs();
    return Effect.gen(function* () {
      const wakeups = yield* Wakeups.ConnectionWakeups;
      let stalled = false;
      const seen: string[] = [];
      const focus = yield* Effect.forkChild(
        Stream.runForEach(wakeups.focusVisibility, () => {
          stalled = true;
          return Effect.never;
        }),
      );
      const supervisor = yield* Effect.forkChild(
        Stream.runForEach(wakeups.changes, (event) =>
          Effect.sync(() => {
            seen.push(event);
          }),
        ),
      );
      yield* waitFor(() => dom.windowListeners("focus").length === 1);
      dom.fireWindow("blur");
      dom.fireWindow("focus");
      yield* waitFor(() => stalled);
      for (let index = 0; index < 40; index += 1) {
        dom.fireWindow("blur");
        dom.fireWindow("focus");
        yield* Effect.yieldNow;
      }
      Object.defineProperty(document, "visibilityState", { value: "hidden", configurable: true });
      dom.fireDocument("visibilitychange");
      Object.defineProperty(document, "visibilityState", { value: "visible", configurable: true });
      dom.fireDocument("visibilitychange");
      yield* waitFor(() => seen.length === 1);
      expect(seen).toEqual(["application-active"]);
      yield* Fiber.interrupt(focus);
      yield* Fiber.interrupt(supervisor);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("a focus frame cannot displace an unread application-active wakeup", () => {
    const dom = makeDomStubs();
    const settle = Effect.gen(function* () {
      for (let index = 0; index < 40; index += 1) yield* Effect.yieldNow;
    });
    const returnToWindow = () => {
      Object.defineProperty(document, "visibilityState", { value: "hidden", configurable: true });
      dom.fireDocument("visibilitychange");
      Object.defineProperty(document, "visibilityState", { value: "visible", configurable: true });
      dom.fireDocument("visibilitychange");
    };
    return Effect.gen(function* () {
      const wakeups = yield* Wakeups.ConnectionWakeups;
      const release = yield* Deferred.make<void>();
      const seen: string[] = [];
      const supervisor = yield* Effect.forkChild(
        Stream.runForEach(wakeups.changes, (event) =>
          Effect.gen(function* () {
            seen.push(event);
            // The supervisor is still handling its first wakeup.
            if (seen.length === 1) yield* Deferred.await(release);
          }),
        ),
      );
      yield* waitFor(() => dom.windowListeners("focus").length === 1);
      returnToWindow();
      yield* waitFor(() => seen.length === 1);
      // The next wakeup waits in the merge hand-off, so the one after it stays
      // unread in the one-frame sliding buffer until a focus frame replaces it.
      returnToWindow();
      yield* settle;
      returnToWindow();
      yield* settle;
      dom.fireWindow("blur");
      dom.fireWindow("focus");
      yield* settle;
      yield* Deferred.succeed(release, undefined);
      for (let index = 0; index < 2_000 && seen.length < 3; index += 1) yield* Effect.yieldNow;
      yield* settle;
      expect(seen).toEqual(["application-active", "application-active", "application-active"]);
      yield* Fiber.interrupt(supervisor);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("a late subscriber is not woken by activity from before it subscribed", () => {
    const dom = makeDomStubs();
    return Effect.gen(function* () {
      const wakeups = yield* Wakeups.ConnectionWakeups;
      const focusReturns: void[] = [];
      const focus = yield* Effect.forkChild(
        Stream.runForEach(wakeups.focusVisibility, (event) =>
          Effect.sync(() => {
            focusReturns.push(event);
          }),
        ),
      );
      yield* waitFor(() => dom.windowListeners("focus").length === 1);
      Object.defineProperty(document, "visibilityState", { value: "hidden", configurable: true });
      dom.fireDocument("visibilitychange");
      Object.defineProperty(document, "visibilityState", { value: "visible", configurable: true });
      dom.fireDocument("visibilitychange");
      yield* waitFor(() => focusReturns.length === 1);

      // The shared listeners are already running and have counted one
      // application-active wakeup; a supervisor subscribing now starts from it.
      const seen: string[] = [];
      const supervisor = yield* Effect.forkChild(
        Stream.runForEach(wakeups.changes, (event) =>
          Effect.sync(() => {
            seen.push(event);
          }),
        ),
      );
      for (let index = 0; index < 20; index += 1) yield* Effect.yieldNow;
      dom.fireWindow("blur");
      dom.fireWindow("focus");
      yield* waitFor(() => focusReturns.length === 2);
      for (let index = 0; index < 20; index += 1) yield* Effect.yieldNow;
      expect(seen).toEqual([]);
      dom.fireDocument("visibilitychange");
      yield* waitFor(() => seen.length === 1);
      expect(seen).toEqual(["application-active"]);
      yield* Fiber.interrupt(supervisor);
      yield* Fiber.interrupt(focus);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("wires a visibility-change wakeup listener and tears it down", () => {
    const dom = makeDomStubs();
    return Effect.gen(function* () {
      const wakeups = yield* Wakeups.ConnectionWakeups;
      const fiber = yield* Effect.forkChild(Stream.runDrain(wakeups.changes));
      yield* waitFor(() => dom.documentListeners("visibilitychange").length > 0);
      // Fire while visible so the listener enqueues an application-active wakeup.
      dom.fireDocument("visibilitychange");
      yield* Fiber.interrupt(fiber);
      expect(dom.documentListeners("visibilitychange").length).toBe(0);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });
});

// ─────────────────────────────────────────────────────────────────────
// PlatformConnectionSource registrations stream
// ─────────────────────────────────────────────────────────────────────

describe("connectionPlatformLayer connection source", () => {
  it.effect("emits no registrations for the hosted static app", () => {
    pf.isHostedStatic = true;
    stubBrowser();
    return Effect.gen(function* () {
      const source = yield* PlatformConnectionSource;
      const head = yield* Stream.runHead(source.registrations);
      expect(Option.isNone(head)).toBe(true);
    }).pipe(Effect.provide(connectionPlatformLayer));
  });

  it.effect("polls the primary and desktop-local topology into registrations", () => {
    stubBrowser({ desktopBridge: makeBridge([]) });
    pf.isHostedStatic = false;
    pf.primaryTarget = {
      source: "cli",
      target: {
        httpBaseUrl: "http://127.0.0.1:3773/",
        wsBaseUrl: "ws://127.0.0.1:3773/",
      },
    };
    pf.secondaryRead = {
      _tag: "Success",
      bootstraps: [
        {
          id: "wsl",
          label: "WSL: Ubuntu",
          httpBaseUrl: "http://127.0.0.1:3202/",
          wsBaseUrl: "ws://127.0.0.1:3202/",
          bootstrapToken: "bootstrap-token",
        },
        {
          // A not-yet-ready desktop-local backend is skipped this poll.
          id: "pending",
          label: "",
          httpBaseUrl: null,
          wsBaseUrl: null,
          bootstrapToken: undefined,
        },
      ],
    };
    return Effect.gen(function* () {
      const source = yield* PlatformConnectionSource;
      const fiber = yield* Effect.forkChild(
        Stream.runHead(source.registrations.pipe(Stream.take(1))),
      );
      yield* TestClock.adjust("3 seconds");
      const head = yield* Fiber.join(fiber);
      expect(Option.isSome(head)).toBe(true);
      const registrations = Option.getOrThrow(head);
      // Primary (same-origin) + secondary (desktop-local bearer) registrations.
      expect(registrations.length).toBeGreaterThanOrEqual(2);
    }).pipe(Effect.provide(Layer.mergeAll(connectionPlatformLayer, TestClock.layer())));
  });

  it.effect("retains a configured unavailable WSL secondary without fabricating a session", () => {
    stubBrowser({ desktopBridge: makeBridge([]) });
    pf.isHostedStatic = false;
    pf.primaryTarget = {
      source: "cli",
      target: {
        httpBaseUrl: "http://127.0.0.1:3773/",
        wsBaseUrl: "ws://127.0.0.1:3773/",
      },
    };
    pf.secondaryRead = {
      _tag: "Success",
      bootstraps: [
        {
          id: "wsl:Ubuntu",
          label: "WSL (Ubuntu)",
          configuredDistro: "Ubuntu",
          runningDistro: null,
          httpBaseUrl: null,
          wsBaseUrl: null,
          preflightError: {
            kind: "wsl-secondary-unavailable",
            detail: "the configured WSL distribution could not start",
          },
        },
      ],
    };
    return Effect.gen(function* () {
      const source = yield* PlatformConnectionSource;
      const fiber = yield* Effect.forkChild(
        Stream.runHead(source.registrations.pipe(Stream.take(1))),
      );
      yield* TestClock.adjust("3 seconds");
      const registrations = Option.getOrThrow(yield* Fiber.join(fiber));

      expect(registrations).toContainEqual(
        expect.objectContaining({
          _tag: "UnavailableConnectionRegistration",
          target: expect.objectContaining({
            _tag: "UnavailableConnectionTarget",
            environmentId: "wsl:Ubuntu",
            label: "WSL (Ubuntu)",
            connectionId: "local:wsl:Ubuntu",
            configuredDistro: "Ubuntu",
            detail: "the configured WSL distribution could not start",
          }),
        }),
      );
      expect(pf.descriptorCalls).toEqual(["http://127.0.0.1:3773/"]);
      expect(pf.bearerBootstrapCalls).toEqual([]);
    }).pipe(Effect.provide(Layer.mergeAll(connectionPlatformLayer, TestClock.layer())));
  });

  it.effect("logs and yields an empty batch when both topology reads fail", () => {
    stubBrowser();
    pf.isHostedStatic = false;
    pf.primaryTarget = new Error("invalid primary target");
    pf.secondaryRead = { _tag: "Failure", cause: new Error("IPC unavailable") };
    return Effect.gen(function* () {
      const source = yield* PlatformConnectionSource;
      const fiber = yield* Effect.forkChild(
        Stream.runHead(source.registrations.pipe(Stream.take(1))),
      );
      yield* TestClock.adjust("3 seconds");
      const head = yield* Fiber.join(fiber);
      expect(Option.isSome(head)).toBe(true);
      // No primary target and a failed secondary read yields an empty batch.
      expect(Option.getOrThrow(head)).toEqual([]);
    }).pipe(Effect.provide(Layer.mergeAll(connectionPlatformLayer, TestClock.layer())));
  });
});

describe("connectionPlatformLayer withheld desktop primary", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it.effect("keeps the primary in every poll and reuses it when its bootstrap returns", () => {
    const primary = {
      id: "primary",
      label: "Local",
      httpBaseUrl: "http://127.0.0.1:14373/",
      wsBaseUrl: "ws://127.0.0.1:14373/",
    };
    let bootstraps: ReturnType<DesktopBridge["getLocalEnvironmentBootstraps"]> = [primary];
    stubBrowser({
      desktopBridge: {
        ...makeBridge([]),
        getLocalEnvironmentBootstraps: () => bootstraps,
      },
    });
    Object.assign(window, { location: new URL("http://tauri.localhost") });
    pf.descriptor = { environmentId: EnvironmentId.make("primary"), label: "Local" };

    return Effect.gen(function* () {
      const targetModule = yield* Effect.promise(() => import("../environments/primary/target"));
      const actualTargetModule = yield* Effect.promise(() =>
        vi.importActual<typeof import("../environments/primary/target")>(
          "../environments/primary/target",
        ),
      );
      // Keep earlier tests' topology mock local to them; exercise the real bridge reader here.
      vi.spyOn(targetModule, "readPrimaryEnvironmentTarget").mockImplementation(
        actualTargetModule.readPrimaryEnvironmentTarget,
      );

      const source = yield* PlatformConnectionSource;
      const pull = yield* Stream.toPull(source.registrations);
      const [initial] = yield* pull;

      bootstraps = [];
      const withheldPoll = yield* Effect.forkChild(pull);
      yield* TestClock.adjust("3 seconds");
      const [withheld] = yield* Fiber.join(withheldPoll);

      bootstraps = [primary];
      const returnedPoll = yield* Effect.forkChild(pull);
      yield* TestClock.adjust("3 seconds");
      const [returned] = yield* Fiber.join(returnedPoll);

      // packages/client-runtime/src/connection/registry.test.ts:
      // "retains shell and thread caches for a desired unavailable platform environment until disable"
      // covers present-keeps/absent-clears. Every poll must include the primary so
      // reconcilePlatform never removes it or clears its composer drafts.
      for (const registrations of [initial, withheld, returned]) {
        expect(registrations).toHaveLength(1);
        expect(registrations[0]).toMatchObject({
          _tag: "PrimaryConnectionRegistration",
          target: {
            environmentId: "primary",
            httpBaseUrl: "http://127.0.0.1:14373/",
            wsBaseUrl: "ws://127.0.0.1:14373/",
          },
        });
      }
      expect(withheld[0]).toBe(initial[0]);
      expect(returned[0]).toBe(initial[0]);
      expect(pf.descriptorCalls).toEqual(["http://127.0.0.1:14373/"]);
    }).pipe(
      Effect.provide(Layer.mergeAll(connectionPlatformLayer, TestClock.layer())),
      Effect.scoped,
    );
  });
});

describe("connectionPlatformLayer resolved primary HTTP", () => {
  afterEach(() => vi.unstubAllEnvs());

  it.effect("refreshes a primary registration when the effective HTTP proxy changes", () => {
    stubBrowser();
    Object.assign(window, { location: new URL("http://127.0.0.1:4885/") });
    vi.stubEnv("VITE_DEV_SERVER_URL", "http://127.0.0.1:4885");
    pf.primaryTarget = {
      source: "configured",
      target: { httpBaseUrl: "http://127.0.0.1:4887/", wsBaseUrl: "ws://127.0.0.1:4887/" },
    };
    return Effect.gen(function* () {
      const source = yield* PlatformConnectionSource;
      const pull = yield* Stream.toPull(source.registrations);
      const [proxied] = yield* pull;
      expect(proxied).toHaveLength(1);
      expect(proxied[0]?.target).toMatchObject({
        httpBaseUrl: "http://127.0.0.1:4885",
        wsBaseUrl: "ws://127.0.0.1:4887/",
      });
      vi.stubEnv("VITE_DEV_SERVER_URL", "");
      const next = yield* Effect.forkChild(pull);
      yield* TestClock.adjust("3 seconds");
      const [direct] = yield* Fiber.join(next);
      expect(direct[0]?.target).toMatchObject({
        httpBaseUrl: "http://127.0.0.1:4887/",
        wsBaseUrl: "ws://127.0.0.1:4887/",
      });
      expect(direct[0]).not.toBe(proxied[0]);
      expect(pf.descriptorCalls).toEqual(["http://127.0.0.1:4885", "http://127.0.0.1:4887/"]);
    }).pipe(
      Effect.provide(Layer.mergeAll(connectionPlatformLayer, TestClock.layer())),
      Effect.scoped,
    );
  });

  it.effect("retains the cached primary through a malformed proxy hint and resumes polling", () => {
    stubBrowser();
    Object.assign(window, { location: new URL("http://127.0.0.1:4885/") });
    vi.stubEnv("VITE_DEV_SERVER_URL", "http://127.0.0.1:4885");
    pf.primaryTarget = {
      source: "configured",
      target: { httpBaseUrl: "http://127.0.0.1:4887/", wsBaseUrl: "ws://127.0.0.1:4887/" },
    };
    return Effect.gen(function* () {
      const source = yield* PlatformConnectionSource;
      const pull = yield* Stream.toPull(source.registrations);
      const [initial] = yield* pull;
      expect(initial).toHaveLength(1);
      vi.stubEnv("VITE_DEV_SERVER_URL", "http://[");
      const malformedPoll = yield* Effect.forkChild(pull);
      yield* TestClock.adjust("3 seconds");
      const [retained] = yield* Fiber.join(malformedPoll);
      expect(retained).toHaveLength(1);
      expect(retained[0]).toBe(initial[0]);
      expect(pf.descriptorCalls).toEqual(["http://127.0.0.1:4885"]);
      vi.stubEnv("VITE_DEV_SERVER_URL", "http://127.0.0.1:4885");
      const restoredPoll = yield* Effect.forkChild(pull);
      yield* TestClock.adjust("3 seconds");
      const [restored] = yield* Fiber.join(restoredPoll);
      expect(restored).toHaveLength(1);
      expect(restored[0]).toBe(initial[0]);
      expect(restored[0]?.target).toMatchObject({
        httpBaseUrl: "http://127.0.0.1:4885",
        wsBaseUrl: "ws://127.0.0.1:4887/",
      });
      expect(pf.descriptorCalls).toEqual(["http://127.0.0.1:4885"]);
    }).pipe(
      Effect.provide(Layer.mergeAll(connectionPlatformLayer, TestClock.layer())),
      Effect.scoped,
    );
  });

  it.effect("uses one desktop topology snapshot for HTTP and WebSocket discovery", () => {
    let topologyReads = 0;
    const first = {
      id: "primary",
      label: "Local",
      httpBaseUrl: "http://127.0.0.1:14373/",
      wsBaseUrl: "ws://127.0.0.1:14373/",
    };
    const later = {
      ...first,
      httpBaseUrl: "http://127.0.0.1:14374/",
      wsBaseUrl: "ws://127.0.0.1:14374/",
    };
    stubBrowser({
      desktopBridge: {
        ...makeBridge([]),
        getLocalEnvironmentBootstraps: () => [++topologyReads === 1 ? first : later],
      },
    });
    Object.assign(window, { location: new URL("http://127.0.0.1:4885/") });
    vi.stubEnv("VITE_DEV_SERVER_URL", "http://127.0.0.1:4885");
    return Effect.gen(function* () {
      const targetModule = yield* Effect.promise(() => import("../environments/primary/target"));
      const actualTargetModule = yield* Effect.promise(() =>
        vi.importActual<typeof import("../environments/primary/target")>(
          "../environments/primary/target",
        ),
      );
      vi.spyOn(targetModule, "readPrimaryEnvironmentTarget").mockImplementation(
        actualTargetModule.readPrimaryEnvironmentTarget,
      );
      const source = yield* PlatformConnectionSource;
      const result = Option.getOrThrow(yield* Stream.runHead(source.registrations));
      expect(result[0]?.target).toMatchObject({
        httpBaseUrl: first.httpBaseUrl,
        wsBaseUrl: first.wsBaseUrl,
      });
      expect(topologyReads).toBe(1);
      expect(pf.descriptorCalls).toEqual([first.httpBaseUrl]);
    }).pipe(
      Effect.provide(connectionPlatformLayer),
      Effect.scoped,
      Effect.ensuring(Effect.sync(() => vi.restoreAllMocks())),
    );
  });
});
