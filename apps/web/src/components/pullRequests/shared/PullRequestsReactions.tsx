import type { PullRequestsPermission, PullRequestsReactionSummary } from "@coddy/contracts";
import { memo, useRef, useState } from "react";
import { Menu, MenuItem, MenuPopup, MenuTrigger } from "../../ui/menu";
import { PermissionButton, constrainPermission } from "../../ui/permission-button";
const EMOJI = {
  "+1": "👍",
  "-1": "👎",
  laugh: "😄",
  confused: "😕",
  heart: "❤️",
  hooray: "🎉",
  rocket: "🚀",
  eyes: "👀",
};
type Content = PullRequestsReactionSummary[number]["content"];
export const PullRequestsReactions = memo(function PullRequestsReactions({
  reactions,
  permission,
  onToggle,
  busy = false,
  live = true,
}: {
  reactions: PullRequestsReactionSummary;
  permission?: PullRequestsPermission;
  onToggle?: (content: Content, on: boolean) => Promise<unknown>;
  busy?: boolean;
  /**
   * Reacting only ever acts on a live detail, never a snapshot. Defaults to
   * `true` so callers that have not adopted the split keep acting as before.
   */
  live?: boolean;
}) {
  const active = useRef(false);
  const [pending, setPending] = useState(false);
  const interactive = permission !== undefined && onToggle !== undefined;
  if (!interactive && reactions.length === 0) return null;
  const currentPermission = permission
    ? constrainPermission(
        permission,
        !live ? "Loading…" : pending || busy ? "Wait for the current action to finish" : null,
      )
    : null;
  const toggle = (content: Content) => {
    if (!currentPermission?.allowed || !onToggle || active.current) return;
    active.current = true;
    setPending(true);
    void onToggle(
      content,
      !reactions.find((reaction) => reaction.content === content)?.viewerReacted,
    )
      .catch(() => undefined)
      .finally(() => {
        active.current = false;
        setPending(false);
      });
  };
  return (
    <div aria-label="Reactions" className="flex flex-wrap gap-1.5">
      {reactions.map((reaction) => {
        const label = `${reaction.content}: ${reaction.count}${reaction.viewerReacted ? ", You reacted" : ""}`;
        return interactive && currentPermission ? (
          <PermissionButton
            mutation
            key={reaction.content}
            permission={currentPermission}
            onClick={() => toggle(reaction.content)}
            aria-label={label}
            aria-pressed={reaction.viewerReacted}
            variant="outline"
            size="sm"
          >
            {EMOJI[reaction.content]} {reaction.count}
          </PermissionButton>
        ) : (
          <span
            key={reaction.content}
            className={`rounded-full border px-2 py-0.5 text-xs ${reaction.viewerReacted ? "border-primary bg-primary/10" : "border-border"}`}
            title={reaction.viewerReacted ? "You reacted" : undefined}
            aria-label={label}
          >
            {EMOJI[reaction.content]} {reaction.count}
          </span>
        );
      })}
      {interactive && currentPermission ? (
        <Menu>
          <MenuTrigger
            disabled={!currentPermission.allowed}
            render={
              <PermissionButton
                mutation
                permission={currentPermission}
                variant="outline"
                size="sm"
              />
            }
          >
            Add reaction
          </MenuTrigger>
          <MenuPopup aria-label="Choose a reaction">
            {(Object.keys(EMOJI) as Content[]).map((content) => (
              <MenuItem
                key={content}
                nativeButton
                disabled={!currentPermission.allowed}
                render={
                  <PermissionButton
                    mutation
                    permission={currentPermission}
                    variant="ghost"
                    aria-label={`React ${content}`}
                  />
                }
                onClick={() => toggle(content)}
              >
                {EMOJI[content]} {content}
              </MenuItem>
            ))}
          </MenuPopup>
        </Menu>
      ) : null}
    </div>
  );
});
