import type { ClientSettings } from "@coddy/contracts/settings";
import { TriangleAlertIcon } from "lucide-react";
import { memo, useLayoutEffect, useRef, useState } from "react";

import { useClientSettings } from "../../hooks/useSettings";
import {
  SLOW_RPC_ACK_THRESHOLD_MS,
  type SlowRpcAckRequest,
  useHasSlowRpcAckRequests,
  useSlowRpcAckRequests,
} from "../../rpc/requestLatencyState";
import { useEnvironments } from "../../state/environments";
import { formatTimestamp } from "../../timestampFormat";
import {
  Popover,
  PopoverDescription,
  PopoverPopup,
  PopoverTitle,
  PopoverTrigger,
} from "../ui/popover";

// The live region's text depends only on whether any request is slow, so screen
// readers hear the first slow request and not every later change to the count.
const SLOW_REQUESTS_ANNOUNCEMENT = "Some requests are slow";

// Elements that take focus from the keyboard; `querySelectorAll` returns them in document order.
const TABBABLE_SELECTOR = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled]):not([type='hidden'])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "[tabindex]:not([tabindex='-1'])",
  "[contenteditable='true']",
].join(", ");
const UNFOCUSABLE_ANCESTOR_SELECTOR =
  "[inert], [hidden], [aria-hidden='true'], [data-base-ui-focus-guard]";

const selectTimestampFormat = (settings: ClientSettings) => settings.timestampFormat;

function slowRequestCountLabel(count: number): string {
  return `${count} slow request${count === 1 ? "" : "s"}`;
}

function slowThresholdLabel(thresholdMs: number): string {
  const seconds = Math.round(thresholdMs / 1000);
  return `${seconds} second${seconds === 1 ? "" : "s"}`;
}

/**
 * Runs just before the indicator's trigger and list leave the page. If either holds
 * focus, moves it to the next control in the same status bar, or else to the control
 * just before the indicator, so keyboard users keep their place instead of landing on
 * the page body. Suppresses scrolling within the always-visible status bar, but
 * allows a fallback outside the bar to scroll into view.
 */
function moveFocusOffLeavingIndicator(trigger: HTMLElement, list: HTMLElement | null): void {
  const document = trigger.ownerDocument;
  const isLeaving = (element: Element | null): boolean =>
    element !== null && (trigger.contains(element) || (list?.contains(element) ?? false));
  if (!isLeaving(document.activeElement)) return;

  const tabbable = Array.from(document.querySelectorAll<HTMLElement>(TABBABLE_SELECTOR));
  const position = tabbable.indexOf(trigger);
  if (position < 0) return;
  const canTakeFocus = (element: HTMLElement): boolean =>
    !isLeaving(element) && element.closest(UNFOCUSABLE_ANCESTOR_SELECTOR) === null;
  const statusBar = trigger.closest("[data-status-bar]");
  const nextInStatusBar = tabbable
    .slice(position + 1)
    .filter((element) => statusBar?.contains(element) === true && canTakeFocus(element));
  const before = tabbable.slice(0, position).filter(canTakeFocus).toReversed();
  for (const candidate of [...nextInStatusBar, ...before]) {
    if (statusBar?.contains(candidate)) {
      candidate.focus({ preventScroll: true });
    } else {
      candidate.focus();
    }
    if (document.activeElement === candidate) return;
  }
}

function SlowRequestList({ requests }: { readonly requests: ReadonlyArray<SlowRpcAckRequest> }) {
  const timestampFormat = useClientSettings(selectTimestampFormat);
  const { environments } = useEnvironments();
  const environmentLabels = new Map<string, string>(
    environments.map(({ environmentId, label }) => [
      environmentId,
      label.trim() ? label : environmentId,
    ]),
  );
  return (
    <ul className="space-y-2">
      {requests.map((request) => (
        <li className="min-w-0 border-t border-border pt-2" key={request.requestId}>
          <div className="wrap-break-word font-medium text-foreground">
            {request.method} ·{" "}
            {environmentLabels.get(request.environmentId) ?? request.environmentId}
          </div>
          <div className="mt-0.5 text-muted-foreground">
            Started{" "}
            <time dateTime={request.startedAt}>
              {formatTimestamp(request.startedAt, timestampFormat)}
            </time>
          </div>
        </li>
      ))}
    </ul>
  );
}

/** The trigger and its list. Mounted only while at least one request is slow. */
function SlowRequestsMenu() {
  const requests = useSlowRpcAckRequests();
  const [open, setOpen] = useState(false);
  const elements = useRef<{ trigger: HTMLButtonElement | null; list: HTMLDivElement | null }>({
    trigger: null,
    list: null,
  });
  useLayoutEffect(() => {
    const current = elements.current;
    // React runs this cleanup before it removes the trigger and the list, while focus
    // can still be found inside them.
    return () => {
      if (current.trigger !== null) moveFocusOffLeavingIndicator(current.trigger, current.list);
    };
  }, []);
  const thresholdMs = requests[0]?.thresholdMs ?? SLOW_RPC_ACK_THRESHOLD_MS;
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger
        className="inline-flex h-5 shrink-0 items-center gap-1 rounded px-1 text-xs text-warning-foreground outline-none hover:bg-accent/70 focus-visible:ring-2 focus-visible:ring-ring"
        ref={(node) => {
          elements.current.trigger = node;
        }}
      >
        <TriangleAlertIcon aria-hidden className="size-3 shrink-0" />
        {slowRequestCountLabel(requests.length)}
      </PopoverTrigger>
      <PopoverPopup
        align="end"
        className="w-[min(22.5rem,calc(100vw-1rem))] max-w-[calc(100vw-1rem)]"
        // Opening moves focus onto the list itself (it has no controls), so a screen
        // reader starts with its title and description.
        initialFocus={() => elements.current.list}
        ref={(node) => {
          elements.current.list = node;
        }}
        side="top"
      >
        <div className="space-y-2 text-xs" data-text-surface="card">
          <div className="space-y-0.5">
            <PopoverTitle className="font-medium text-foreground text-xs leading-4">
              Slow requests
            </PopoverTitle>
            <PopoverDescription className="text-muted-foreground text-xs">
              Waiting more than {slowThresholdLabel(thresholdMs)} for a response.
            </PopoverDescription>
          </div>
          {open ? <SlowRequestList requests={requests} /> : null}
        </div>
      </PopoverPopup>
    </Popover>
  );
}

/**
 * Status bar warning shown while requests wait longer than the slow threshold
 * for a response. This shell subscribes only to whether any request is slow;
 * the mounted menu owns count and list updates. `memo` keeps the status bar's
 * own frequent re-renders out of it: the React Compiler does not compile
 * `AppStatusBarView`, which reads refs during render.
 */
export const SlowRequestsIndicator = memo(function SlowRequestsIndicator() {
  const hasSlowRequests = useHasSlowRpcAckRequests();
  return (
    <>
      <span aria-atomic="true" aria-live="polite" className="sr-only" role="status">
        {hasSlowRequests ? SLOW_REQUESTS_ANNOUNCEMENT : ""}
      </span>
      {hasSlowRequests ? <SlowRequestsMenu /> : null}
    </>
  );
});
