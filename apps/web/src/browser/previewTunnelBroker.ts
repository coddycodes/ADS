import type { EnvironmentSupervisor } from "@coddy/client-runtime/connection";
import { request, runStream } from "@coddy/client-runtime/rpc";
import { runInEnvironment } from "@coddy/client-runtime/state/runtime";
import {
  EnvironmentId,
  WS_METHODS,
  type DesktopPreviewTunnelBridge,
  type DesktopPreviewTunnelRequest,
} from "@coddy/contracts";
import * as Cause from "effect/Cause";
import * as Effect from "effect/Effect";
import * as Stream from "effect/Stream";
import { type AtomRegistry } from "effect/reactivity";

import { connectionAtomRuntime } from "../connection/runtime";
import { readPreparedConnection } from "../state/session";

/**
 * The environment's server exactly as this computer reaches it on loopback, such as an SSH
 * environment's local forward (`127.0.0.1:<forward port>`): its own pages, like signed file
 * links, load from there. The same port under another spelling (`localhost:<port>`) is a dev
 * server on the environment.
 */
function serverHasTunnels(environmentId: string): boolean {
  return (
    readPreparedConnection(EnvironmentId.make(environmentId))?.descriptor?.capabilities
      ?.previewTunnels === true
  );
}

function isLocalEndpoint(environmentId: string, host: string, port: number): boolean {
  const httpBaseUrl = readPreparedConnection(EnvironmentId.make(environmentId))?.httpBaseUrl;
  if (httpBaseUrl === undefined) return false;
  try {
    const url = new URL(httpBaseUrl);
    const unbracket = (name: string) => name.replace(/^\[|\]$/g, "").toLowerCase();
    const urlPort = Number(url.port || (url.protocol === "https:" ? 443 : 80));
    return unbracket(url.hostname) === unbracket(host) && urlPort === port;
  } catch {
    return false;
  }
}

/**
 * One environment's side of preview tunnels: `open` delivers `onOpened` once, then each chunk
 * through `onData`, waiting for each before taking the next, and settles when the stream ends.
 */
export interface PreviewTunnelTransport {
  open(
    environmentId: string,
    port: number,
    handlers: {
      readonly onOpened: (tunnelId: string) => Promise<void>;
      readonly onData: (data: string) => Promise<void>;
    },
    signal: AbortSignal,
  ): Promise<void>;
  write(environmentId: string, tunnelId: string, data: string): Promise<void>;
  close(environmentId: string, tunnelId: string): Promise<void>;
}

const ignore = (): void => undefined;

/**
 * Tunnels one environment carries at once. Each holds one of the connection's 64 RPC request
 * slots for its lifetime, and one more while it writes, so this keeps at least half of them
 * for its writes and everything else; a page's further connections wait for a slot.
 */
export const MAX_TUNNELS_PER_ENVIRONMENT = 16;

/** Requests waiting for a slot beyond this are refused at once. */
const MAX_WAITING_TUNNELS = 64;
/** The desktop refuses a request unanswered for 20 s; a later slot must not open it. */
const TUNNEL_ANSWER_DEADLINE_MS = 15_000;

/** `limit` slots: `take` waits for one and resolves with its release. */
function createSlots(limit: number) {
  let used = 0;
  const waiting: Array<() => void> = [];
  const release = () => {
    const next = waiting.shift();
    if (next) next();
    else used -= 1;
  };
  return {
    waiting: () => waiting.length,
    take: (): Promise<() => void> => {
      if (used < limit) {
        used += 1;
        return Promise.resolve(release);
      }
      return new Promise((resolve) => waiting.push(() => resolve(release)));
    },
  };
}

/**
 * Serves one page connection the desktop's SOCKS server handed over (#69): opens a tunnel on
 * the environment's connection, answers the desktop once it opens or fails, then moves bytes
 * both ways until either side ends, and closes both.
 */
export async function servePreviewTunnel(
  tunnelRequest: DesktopPreviewTunnelRequest,
  bridge: DesktopPreviewTunnelBridge,
  transport: PreviewTunnelTransport,
  /** The environment's own endpoint on this computer (an SSH forward): it is not tunneled. */
  isLocalEndpoint: (environmentId: string, host: string, port: number) => boolean = () => false,
): Promise<void> {
  const { connectionId, environmentId, host, port } = tunnelRequest;
  if (isLocalEndpoint(environmentId, host, port)) {
    await bridge.answer(connectionId, "direct").catch(ignore);
    return;
  }
  const abort = new AbortController();
  let tunnelId: string | null = null;
  let pumping: Promise<void> = Promise.resolve();
  // What the page sends goes up one chunk at a time; the page closing its side closes the tunnel.
  const pumpUp = async (id: string) => {
    try {
      while (!abort.signal.aborted) {
        const data = await bridge.read(connectionId);
        if (data === null) break;
        await transport.write(environmentId, id, data);
      }
    } finally {
      abort.abort();
      await transport.close(environmentId, id).catch(ignore);
    }
  };
  try {
    await transport.open(
      environmentId,
      port,
      {
        onOpened: async (id) => {
          tunnelId = id;
          // The page gets its SOCKS reply before any of the environment's bytes.
          await bridge.answer(connectionId, "opened");
          pumping = pumpUp(id).catch(ignore);
        },
        onData: (data) => bridge.write(connectionId, data),
      },
      abort.signal,
    );
  } catch {
    // Refused before it opened: the page sees its connection refused.
    if (tunnelId === null) await bridge.answer(connectionId, "refused").catch(ignore);
  } finally {
    abort.abort();
    await bridge.close(connectionId).catch(ignore);
    await pumping;
  }
}

/** Runs `effect` in `environmentId` through the app's connection runtime. */
function runInEnvironmentPromise<A, E>(
  registry: AtomRegistry.AtomRegistry,
  environmentId: string,
  effect: Effect.Effect<A, E, EnvironmentSupervisor>,
  signal?: AbortSignal,
): Promise<A> {
  const atom = connectionAtomRuntime.atom(
    runInEnvironment(EnvironmentId.make(environmentId), effect),
  );
  return new Promise<A>((resolve, reject) => {
    let settled = false;
    let unmount: () => void = ignore;
    let unsubscribe: () => void = ignore;
    const settle = (finish: () => void) => {
      if (settled) return;
      settled = true;
      unsubscribe();
      unmount();
      signal?.removeEventListener("abort", onAbort);
      finish();
    };
    const onAbort = () => settle(() => reject(new Error("preview tunnel stopped")));
    if (signal?.aborted) return onAbort();
    signal?.addEventListener("abort", onAbort);
    unmount = registry.mount(atom);
    const stop = registry.subscribe(
      atom,
      (emission) => {
        if (emission._tag === "Success" && !emission.waiting) {
          settle(() => resolve(emission.value));
        } else if (emission._tag === "Failure" && !emission.waiting) {
          settle(() => reject(Cause.squash(emission.cause)));
        }
      },
      { immediate: true },
    );
    // An immediate emission may have settled before `subscribe` returned.
    if (settled) stop();
    else unsubscribe = stop;
  });
}

/** Tunnels over each environment's own RPC connection (`preview.tunnel*`). */
export function createPreviewTunnelTransport(
  registry: AtomRegistry.AtomRegistry,
): PreviewTunnelTransport {
  return {
    open: (environmentId, port, handlers, signal) =>
      runInEnvironmentPromise(
        registry,
        environmentId,
        // Each chunk is written to the page before the next is pulled, so the server sends
        // the next one only after the page took this one.
        Stream.runForEach(runStream(WS_METHODS.previewTunnelOpen, { port }), (event) =>
          Effect.promise(() =>
            event._tag === "Opened"
              ? handlers.onOpened(event.tunnelId)
              : handlers.onData(event.data),
          ),
        ),
        signal,
      ),
    write: (environmentId, tunnelId, data) =>
      runInEnvironmentPromise(
        registry,
        environmentId,
        request(WS_METHODS.previewTunnelWrite, { tunnelId, data }),
      ),
    close: (environmentId, tunnelId) =>
      runInEnvironmentPromise(
        registry,
        environmentId,
        request(WS_METHODS.previewTunnelClose, { tunnelId }),
      ),
  };
}

/**
 * Serves the desktop's preview tunnel requests while the app runs. Does nothing where the
 * desktop routes no previews through SOCKS (browser mode, Windows, macOS).
 */
export { createSlots as createTunnelSlotsForTests };

export function startPreviewTunnelBroker(registry: AtomRegistry.AtomRegistry): () => void {
  const bridge = typeof window === "undefined" ? undefined : window.desktopBridge?.preview?.tunnels;
  if (bridge === undefined) return ignore;
  const transport = createPreviewTunnelTransport(registry);
  const slotsByEnvironment = new Map<string, ReturnType<typeof createSlots>>();
  return bridge.onRequest((tunnelRequest) => {
    // The environment's own endpoint here needs no tunnel, slot or server support.
    const { connectionId, environmentId, host, port } = tunnelRequest;
    if (isLocalEndpoint(environmentId, host, port)) {
      void bridge.answer(connectionId, "direct").catch(ignore);
      return;
    }
    let slots = slotsByEnvironment.get(tunnelRequest.environmentId);
    if (slots === undefined) {
      slots = createSlots(MAX_TUNNELS_PER_ENVIRONMENT);
      slotsByEnvironment.set(tunnelRequest.environmentId, slots);
    }
    // The environment may have reconnected to a server without tunnels since its preview host
    // chose them: an unknown method there would fail the whole connection.
    if (slots.waiting() >= MAX_WAITING_TUNNELS || !serverHasTunnels(tunnelRequest.environmentId)) {
      void bridge.answer(tunnelRequest.connectionId, "refused").catch(ignore);
      return;
    }
    const requestedAt = Date.now();
    void slots.take().then((release) => {
      // The desktop already refused a request this old; opening it would leak a tunnel.
      if (Date.now() - requestedAt > TUNNEL_ANSWER_DEADLINE_MS) {
        release();
        return;
      }
      // The environment may have reconnected to another server while this request waited.
      // ponytail: checked right before the open, not atomically with the session it goes to.
      if (!serverHasTunnels(environmentId)) {
        release();
        void bridge.answer(connectionId, "refused").catch(ignore);
        return;
      }
      return servePreviewTunnel(tunnelRequest, bridge, transport).finally(release);
    });
  });
}
