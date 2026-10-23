import { useAtomValue } from "@effect/atom-react";
import * as Option from "effect/Option";
import { AsyncResult, Atom } from "effect/reactivity";
import { useMemo } from "react";

import { vcsEnvironment } from "../../state/vcs";
import { resolveWorkspaceBranchLabel } from "../Sidebar.logic";
import type { AgentCheckout } from "../sidebar/agentsSection.logic";

/**
 * The live branch of each checkout, from its passive VCS summary (the 30-second
 * producer the left panel shares), keyed by checkout key. One atom reads them all,
 * so the list can filter on the branch a row shows.
 */
export function useAgentLiveBranches(
  checkouts: ReadonlyArray<AgentCheckout>,
): ReadonlyMap<string, string> {
  // Keyed by the checkout set, not the array: shells change on every thread event,
  // and a new atom would resubscribe every summary.
  const checkoutsKey = checkouts.map((checkout) => checkout.key).join("\n");
  const atom = useMemo(
    () =>
      Atom.make((get) => {
        const branches = new Map<string, string>();
        for (const checkout of checkouts) {
          const summary = Option.getOrNull(
            AsyncResult.value(
              get(
                vcsEnvironment.summary({
                  environmentId: checkout.environmentId,
                  input: { cwd: checkout.cwd },
                }),
              ),
            ),
          );
          const branch = resolveWorkspaceBranchLabel(summary, null);
          if (branch !== null) branches.set(checkout.key, branch);
        }
        return branches;
      }).pipe(Atom.withLabel("agents-live-branches")),
    // `checkouts` is read only through its keys.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [checkoutsKey],
  );
  return useAtomValue(atom);
}
