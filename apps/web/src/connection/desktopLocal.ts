import {
  DESKTOP_LOCAL_CONNECTION_ID_PREFIX,
  isDesktopLocalConnectionId,
  type ConnectionTarget,
} from "@coddy/client-runtime/connection";
import {
  PRIMARY_LOCAL_ENVIRONMENT_ID,
  type DesktopBridge,
  type DesktopEnvironmentBootstrap,
} from "@coddy/contracts";

/**
 * Desktop-local secondary backends (e.g. a parallel WSL backend) are registered
 * by the connection platform source as bearer connections whose id carries this
 * prefix. It is the renderer's single signal that an environment is a
 * host-managed local backend rather than a user-saved remote, SSH, or relay
 * environment.
 *
 * Keep this the one source of truth: the producer (`connection/platform.ts`)
 * mints ids via {@link desktopLocalConnectionId} and every consumer classifies
 * via {@link isDesktopLocalConnectionTarget}, so the convention can never drift
 * between the two.
 */
export { DESKTOP_LOCAL_CONNECTION_ID_PREFIX } from "@coddy/client-runtime/connection";

export function desktopLocalConnectionId(backendId: string): string {
  return `${DESKTOP_LOCAL_CONNECTION_ID_PREFIX}${backendId}`;
}

export function isDesktopLocalConnectionTarget(
  target: ConnectionTarget,
): target is Extract<
  ConnectionTarget,
  { readonly _tag: "BearerConnectionTarget" | "UnavailableConnectionTarget" }
> {
  return (
    (target._tag === "BearerConnectionTarget" || target._tag === "UnavailableConnectionTarget") &&
    isDesktopLocalConnectionId(target.connectionId)
  );
}

export function desktopLocalBackendId(target: ConnectionTarget): string | null {
  return isDesktopLocalConnectionTarget(target)
    ? target.connectionId.slice(DESKTOP_LOCAL_CONNECTION_ID_PREFIX.length)
    : null;
}

export type DesktopSecondaryBootstrapsRead =
  | {
      readonly _tag: "Success";
      readonly bootstraps: ReadonlyArray<DesktopEnvironmentBootstrap>;
    }
  | {
      readonly _tag: "Failure";
      readonly cause: unknown;
    };

export interface DesktopSecondaryBootstrapsReader {
  readonly readResult: () => DesktopSecondaryBootstrapsRead;
  readonly readSnapshot: () => ReadonlyArray<DesktopEnvironmentBootstrap>;
}

/**
 * Build a topology reader whose snapshot advances only after successful bridge
 * reads. A successful empty read is authoritative; a thrown read preserves the
 * previous snapshot so UI consumers cannot temporarily disagree with the
 * platform's retained registrations.
 */
export function createDesktopSecondaryBootstrapsReader(
  resolveBridge: () => Pick<DesktopBridge, "getLocalEnvironmentBootstraps"> | undefined,
): DesktopSecondaryBootstrapsReader {
  let snapshot: ReadonlyArray<DesktopEnvironmentBootstrap> = [];
  // The bridge's array the snapshot came from: an unchanged topology keeps the snapshot's
  // identity, so polling readers do not re-render.
  let source: ReadonlyArray<DesktopEnvironmentBootstrap> | undefined;

  const readResult = (): DesktopSecondaryBootstrapsRead => {
    const bridge = resolveBridge();
    if (bridge === undefined) {
      if (snapshot.length > 0) snapshot = [];
      source = undefined;
      return { _tag: "Success", bootstraps: snapshot };
    }
    try {
      const bootstraps = bridge.getLocalEnvironmentBootstraps();
      if (bootstraps !== source) {
        snapshot = bootstraps.filter((entry) => entry.id !== PRIMARY_LOCAL_ENVIRONMENT_ID);
        source = bootstraps;
      }
      return { _tag: "Success", bootstraps: snapshot };
    } catch (cause) {
      return { _tag: "Failure", cause };
    }
  };

  return {
    readResult,
    readSnapshot: () => {
      const result = readResult();
      return result._tag === "Success" ? result.bootstraps : snapshot;
    },
  };
}

const desktopSecondaryBootstrapsReader = createDesktopSecondaryBootstrapsReader(
  () => window.desktopBridge,
);

/** Read the topology while preserving failures for platform cache policy. */
export function readDesktopSecondaryBootstrapsResult(): DesktopSecondaryBootstrapsRead {
  return desktopSecondaryBootstrapsReader.readResult();
}

/** Read the latest successful topology snapshot for renderer consumers. */
export function readDesktopSecondaryBootstraps(): ReadonlyArray<DesktopEnvironmentBootstrap> {
  return desktopSecondaryBootstrapsReader.readSnapshot();
}
