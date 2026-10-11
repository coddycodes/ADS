import { stackedThreadToast, toastManager } from "~/components/ui/toast";

import { type PreviewTargetResolution, UNREACHABLE_MESSAGES } from "./browserTargetResolver";

const copyLinkAction = (url: string) => ({
  children: "Copy link",
  onClick: () => void navigator.clipboard?.writeText(url).catch(() => undefined),
});

export function showPreviewUnreachableNotice(
  resolution: Extract<PreviewTargetResolution, { kind: "unreachable" }>,
): void {
  showPreviewUnreachableMessage(
    UNREACHABLE_MESSAGES[resolution.reason](resolution.environmentLabel),
    resolution.url,
  );
}

/** The same notice for a resolution that failed after the gateway was asked. */
export function showPreviewUnreachableMessage(message: string, url: string): void {
  toastManager.add(
    stackedThreadToast({
      type: "warning",
      title: "Can't open this address here",
      description: `${message} (${url})`,
      actionVariant: "outline",
      actionProps: copyLinkAction(url),
    }),
  );
}

export function showPreviewFailedNotice(input: { readonly onOpenInEditor: () => void }): void {
  toastManager.add(
    stackedThreadToast({
      type: "warning",
      title: "Couldn't preview this file",
      description: "You can open it in the editor instead.",
      actionVariant: "outline",
      actionProps: { children: "Open in editor", onClick: input.onOpenInEditor },
    }),
  );
}

export function showFileOutsideWorkspaceNotice(input: {
  readonly onOpenInEditor: () => void;
}): void {
  toastManager.add(
    stackedThreadToast({
      type: "info",
      title: "This file is outside the workspace",
      description: "Only files inside the thread's workspace can open in the browser.",
      actionVariant: "outline",
      actionProps: { children: "Open in editor", onClick: input.onOpenInEditor },
    }),
  );
}

/** An SSH preview fell back to another local port because `hostPort` is taken on this computer. */
export function showSamePortBusyNotice(hostPort: string): void {
  toastManager.add(
    stackedThreadToast({
      type: "info",
      title: `${hostPort} is in use on this computer`,
      description: `This preview runs on a different local port. Apps that expect ${hostPort} (OAuth sign-in, for example) may not work until you free it.`,
    }),
  );
}

/** Copies a link; a refused clipboard leaves nothing further to do. */
export function copyLink(url: string): void {
  copyLinkAction(url).onClick();
}

export function showLinkOpenFailedNotice(url: string): void {
  toastManager.add(
    stackedThreadToast({
      type: "error",
      title: "Couldn't open the link",
      description: url,
      actionVariant: "outline",
      actionProps: copyLinkAction(url),
    }),
  );
}
