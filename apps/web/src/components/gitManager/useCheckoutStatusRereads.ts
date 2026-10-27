import type { EnvironmentId } from "@coddy/contracts";
import { useCallback, useState } from "react";

import { useAtomCommand } from "../../state/use-atom-command";
import { vcsEnvironment } from "../../state/vcs";

/** Keep this above tab panels so a status read remains busy when its view unmounts. */
export function useCheckoutStatusRereads({
  environmentId,
  cwd,
}: {
  readonly environmentId: EnvironmentId;
  readonly cwd: string;
}) {
  const refreshStatus = useAtomCommand(vcsEnvironment.refreshStatus, { reportFailure: false });
  const checkoutKey = `${environmentId}\u0000${cwd}`;
  const [pendingReads, setPendingReads] = useState<ReadonlyMap<string, number>>(() => new Map());

  const onRetry = useCallback(() => {
    setPendingReads((current) => {
      const next = new Map(current);
      next.set(checkoutKey, (current.get(checkoutKey) ?? 0) + 1);
      return next;
    });
    // The runtime shares requests; each caller still owns one busy count until it settles.
    void refreshStatus({ environmentId, input: { cwd } }).finally(() => {
      setPendingReads((current) => {
        const next = new Map(current);
        const remaining = (current.get(checkoutKey) ?? 0) - 1;
        if (remaining > 0) next.set(checkoutKey, remaining);
        else next.delete(checkoutKey);
        return next;
      });
    });
  }, [checkoutKey, cwd, environmentId, refreshStatus]);

  return { retrying: pendingReads.has(checkoutKey), onRetry };
}
