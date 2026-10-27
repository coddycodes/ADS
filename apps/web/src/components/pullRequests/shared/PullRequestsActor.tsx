import type { PullRequestsActor as Actor } from "@coddy/contracts";
export function PullRequestsActor({
  actor,
  preferName = false,
  showInitials = true,
}: {
  actor: Actor;
  preferName?: boolean;
  showInitials?: boolean;
}) {
  const name = actor.name?.trim() || actor.login;
  const initials = name
    .split(/\s+/)
    .slice(0, 2)
    .map((part) => Array.from(part)[0])
    .join("")
    .toUpperCase();
  return (
    <span className="inline-flex items-center gap-1.5">
      {showInitials ? (
        <span
          aria-hidden="true"
          className="inline-flex size-6 items-center justify-center rounded-full bg-muted text-xs font-medium"
        >
          {initials}
        </span>
      ) : null}
      <span>{preferName ? name : actor.login}</span>
      {actor.isBot ? <span className="text-muted-foreground">(bot)</span> : null}
    </span>
  );
}
