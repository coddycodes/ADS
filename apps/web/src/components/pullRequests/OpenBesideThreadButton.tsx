import type { ScopedProjectRef, SourceControlProviderKind } from "@bibcode/contracts";
import { useNavigate } from "@tanstack/react-router";
import { useMemo } from "react";

import { useRightPanelStore } from "../../rightPanelStore";
import { useThreadShellsForProjectRefs } from "../../state/entities";
import { Button } from "../ui/button";
import { Menu, MenuItem, MenuPopup, MenuTrigger } from "../ui/menu";
import { Popover, PopoverPopup, PopoverTrigger } from "../ui/popover";
import { rankBesideThreadCandidates } from "./openBesideThread.logic";

const TRIGGER_LABEL = "Open beside a thread…";
const NO_THREAD_REASON = "This project has no thread to open it beside.";

/** The full page's way to pin a request beside one of the project's threads. */
export function OpenBesideThreadButton({
  projectRef,
  number,
  provider,
  headBranch,
}: {
  projectRef: ScopedProjectRef;
  number: number;
  provider: SourceControlProviderKind;
  headBranch: string;
}) {
  const navigate = useNavigate();
  const projectRefs = useMemo(() => [projectRef], [projectRef]);
  const threads = useThreadShellsForProjectRefs(projectRefs);
  const candidates = useMemo(
    () => rankBesideThreadCandidates(threads, headBranch),
    [headBranch, threads],
  );
  if (candidates.length === 0) {
    // A disabled button ignores the pointer, so its own title never shows: keep it
    // focusable and explain the reason in a hover popup, as the git actions do.
    return (
      <Popover>
        <PopoverTrigger
          openOnHover
          render={
            <Button
              aria-disabled="true"
              className="cursor-not-allowed opacity-64"
              size="sm"
              variant="outline"
            />
          }
        >
          {TRIGGER_LABEL}
        </PopoverTrigger>
        <PopoverPopup tooltipStyle side="bottom" align="end">
          {NO_THREAD_REASON}
        </PopoverPopup>
      </Popover>
    );
  }
  return (
    <Menu>
      <MenuTrigger render={<Button size="sm" variant="outline" />}>{TRIGGER_LABEL}</MenuTrigger>
      <MenuPopup align="end" className="max-w-96 min-w-52">
        {candidates.map((candidate) => (
          <MenuItem
            key={`${candidate.ref.environmentId}:${candidate.ref.threadId}`}
            aria-label={candidate.onBranch ? `${candidate.title}, on this branch` : candidate.title}
            onClick={() => {
              useRightPanelStore.getState().openPullRequest(candidate.ref, { number, provider });
              void navigate({
                to: "/$environmentId/$threadId",
                params: {
                  environmentId: candidate.ref.environmentId,
                  threadId: candidate.ref.threadId,
                },
              });
            }}
          >
            <span className="truncate">{candidate.title}</span>
            {candidate.onBranch ? (
              <span className="ml-auto text-xs text-muted-foreground">On this branch</span>
            ) : null}
          </MenuItem>
        ))}
      </MenuPopup>
    </Menu>
  );
}
