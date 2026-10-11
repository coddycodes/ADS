import { copyLink } from "~/browser/linkNotices";
import {
  enqueueOpenPrompt,
  markOpenPromptBlocked,
  type OpenPrompt,
  removeOpenPrompt,
  useOpenPromptStore,
} from "~/browser/openPromptQueue";
import { openLink } from "~/browser/openLink";
import { getClientSettings } from "~/hooks/useSettings";
import type { AppRouter } from "~/router";
import { previewEnvironment } from "~/state/preview";
import { useAtomCommand } from "~/state/use-atom-command";
import { buildThreadRouteParams } from "~/threadRoutes";

import { Button } from "../ui/button";

function promptLead(prompt: OpenPrompt): string {
  if (prompt.blocked) return "Your browser blocked a new tab for";
  switch (prompt.source) {
    case "agent":
      return "Agent wants to open";
    case "command":
      return prompt.threadTitle === undefined
        ? "A command wants to open"
        : `A command in ${prompt.threadTitle} wants to open`;
    case "link":
      return "Open";
  }
}

/**
 * A non-modal bar asking to open one address at a time; nothing opens without
 * a click. AppRoot's top banner stack positions it, outside the router, so it
 * takes the router to show a thread that isn't on screen.
 */
export function OpenPromptBanner(props: { readonly router: Pick<AppRouter, "navigate"> }) {
  const { router } = props;
  const prompt = useOpenPromptStore((state) => state.prompts[0] ?? null);
  const waiting = useOpenPromptStore((state) => state.prompts.length - 1);
  const openPreview = useAtomCommand(previewEnvironment.open, { reportFailure: true });
  if (prompt === null) return null;
  return (
    <div
      role="region"
      aria-label="Requests to open links"
      className="flex flex-wrap items-center justify-center gap-3 border-b border-border bg-background px-4 py-2 text-sm"
    >
      <span aria-live="polite" className="min-w-0 break-all">
        {promptLead(prompt)} <span className="font-mono">{prompt.url}</span> on{" "}
        {prompt.environmentLabel}
        {prompt.blocked ? ". Allow pop-ups for this site to open links directly." : null}
        {waiting > 0 ? (
          <span className="text-muted-foreground"> ({waiting} more waiting)</span>
        ) : null}
      </span>
      <Button
        size="xs"
        onClick={() => {
          // The asking thread wasn't on screen: show it, so the page opens beside it.
          if (prompt.threadTitle !== undefined) {
            void router.navigate({
              to: "/$environmentId/$threadId",
              params: buildThreadRouteParams(prompt.threadRef),
            });
          }
          let blocked = false;
          // Runs inside the click, so the new tab keeps its user activation.
          const outcome = openLink({
            url: prompt.url,
            threadRef: prompt.threadRef,
            // A blocked link asked for a new tab; Open keeps it there.
            invert: prompt.source === "link" && getClientSettings().browserLinkTarget === "app",
            openPreview,
            onPopupBlocked: () => {
              blocked = true;
            },
            // A retryable failure after the tab opened: offer the same request again.
            onUnopened: () => enqueueOpenPrompt({ ...prompt, blocked: false }),
            // The internal browser couldn't open it (its own error shows): offer it again too.
            onError: () => enqueueOpenPrompt({ ...prompt, blocked: false }),
          });
          if (blocked) markOpenPromptBlocked(prompt.id);
          // A refusal shows its notice; the prompt stays so Open can be retried.
          else if (outcome === "app" || outcome === "system") removeOpenPrompt(prompt.id);
        }}
      >
        {prompt.threadTitle === undefined ? "Open" : "Show thread and open"}
      </Button>
      <Button size="xs" variant="outline" onClick={() => copyLink(prompt.url)}>
        Copy link
      </Button>
      <Button size="xs" variant="outline" onClick={() => removeOpenPrompt(prompt.id)}>
        Dismiss
      </Button>
    </div>
  );
}
