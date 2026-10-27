/** A browser tab opened inside a click, to be pointed at a URL that is still resolving. */
export interface PendingTab {
  readonly navigate: (url: string) => void;
  readonly close: () => void;
}

/**
 * Opens a blank tab while the click's user activation is still live; the
 * caller navigates it once an async resolution finishes. Returns null when
 * the browser blocks the popup.
 */
export function openPendingTab(): PendingTab | null {
  // No "noopener" here: it would make window.open return null and lose the handle.
  const tab = window.open("about:blank", "_blank");
  if (!tab) return null;
  // The previewed page must not script Coddy Studio through window.opener.
  tab.opener = null;
  return {
    // Only web addresses: a javascript: or file: URL must never run in the new tab.
    navigate: (url) => (isHttpUrl(url) ? tab.location.replace(url) : tab.close()),
    close: () => tab.close(),
  };
}

function isHttpUrl(url: string): boolean {
  try {
    const { protocol } = new URL(url);
    return protocol === "http:" || protocol === "https:";
  } catch {
    return false;
  }
}
