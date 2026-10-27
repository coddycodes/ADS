import { useAtomValue } from "@effect/atom-react";
import { managedRelaySessionAtom } from "@coddy/client-runtime/relay";
import { useEffect } from "react";

import { useAtomCommand } from "../state/use-atom-command";
import { type CloudLinkTarget, needsManagedEndpointRelink } from "./linkEnvironment";
import { relinkPrimaryManagedEndpointCommand } from "./linkEnvironmentAtoms";
import { readManagedRelayClerkToken } from "./managedAuth";
import { usePrimaryCloudLinkState } from "./primaryCloudLinkState";

/** Environments re-linked this app session. */
const attemptedThisSession = new Set<string>();

/**
 * Re-links `target` once per app session so the relay provisions its tunnel again.
 * The attempt is claimed before any await, so overlapping calls dispatch one re-link; a
 * missing token (signed out) gives the claim back. The command itself re-checks the
 * server's link state and account on the link scheduler.
 */
export async function relinkOncePerSession(deps: {
  readonly target: CloudLinkTarget;
  readonly accountId: string;
  readonly readToken: () => Promise<string | null>;
  readonly relink: (input: {
    readonly target: CloudLinkTarget;
    readonly clerkToken: string;
    readonly accountId: string;
  }) => Promise<{ readonly _tag: string }>;
  readonly refresh: () => void;
  readonly attempted?: Set<string>;
}): Promise<void> {
  const attempted = deps.attempted ?? attemptedThisSession;
  const key = deps.target.environmentId;
  if (attempted.has(key)) return;
  attempted.add(key);
  const clerkToken = await deps.readToken().catch(() => null);
  if (!clerkToken) {
    attempted.delete(key);
    return;
  }
  const result = await deps.relink({
    target: deps.target,
    clerkToken,
    accountId: deps.accountId,
  });
  if (result._tag !== "Success") {
    console.warn(
      "[coddy-connect] Could not refresh the Coddy Connect tunnel; retrying next session.",
    );
    return;
  }
  deps.refresh();
}

/**
 * Re-links the primary environment once per session when its Coddy Connect tunnel needs
 * provisioning again (no preview hostnames yet, or the server moved to another port) and
 * is linked to the signed-in account. A sign-in after mount retries.
 */
export function ManagedEndpointRelink(): null {
  const accountId = useAtomValue(managedRelaySessionAtom)?.accountId ?? null;
  const { data, target, refresh } = usePrimaryCloudLinkState();
  // Boolean dependencies: link-state revalidations that change nothing rerun nothing.
  const needed = needsManagedEndpointRelink(data) && data?.cloudUserId === accountId;
  const relink = useAtomCommand(relinkPrimaryManagedEndpointCommand, {
    reportFailure: false,
  });
  useEffect(() => {
    if (!target || !accountId || !needed) return;
    void relinkOncePerSession({
      target,
      accountId,
      readToken: readManagedRelayClerkToken,
      relink,
      refresh,
    });
  }, [accountId, needed, refresh, relink, target]);
  return null;
}
