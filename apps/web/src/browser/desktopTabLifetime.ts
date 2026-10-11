import { previewBridge } from "~/components/preview/previewBridge";

interface DesktopTabLease {
  /** The tab's preview storage partition (its environment; `null` for the local one). */
  readonly partition: string | null;
  references: number;
  closeTimer: number | null;
  ready: Promise<void>;
  createFailed: boolean;
}

const leases = new Map<string, DesktopTabLease>();
let nativeOperationTail: Promise<void> | undefined;

export interface AcquiredDesktopTab {
  readonly ready: Promise<void>;
  readonly navigate: (url: string, shouldNavigate?: () => boolean) => Promise<void>;
  readonly release: () => void;
}

function enqueueNativeOperation(operation: () => Promise<void>): Promise<void> {
  let result: Promise<void>;
  try {
    result = nativeOperationTail === undefined ? operation() : nativeOperationTail.then(operation);
  } catch (error) {
    result = Promise.reject(error);
  }
  const clearTail = () => {
    if (nativeOperationTail === tail) nativeOperationTail = undefined;
  };
  const tail = result.then(clearTail, clearTail);
  nativeOperationTail = tail;
  return result;
}

async function closeTab(tabId: string, lease: DesktopTabLease): Promise<void> {
  try {
    await lease.ready;
  } catch {
    // A rejected creation has no native child to preserve.
  }
  await previewBridge?.closeTab(tabId);
}

function createTab(tabId: string, lease: DesktopTabLease): void {
  const inactive: Array<readonly [string, DesktopTabLease]> = [];
  for (const [inactiveTabId, inactiveLease] of leases) {
    if (inactiveTabId === tabId || inactiveLease.references > 0) continue;
    if (inactiveLease.closeTimer !== null) window.clearTimeout(inactiveLease.closeTimer);
    inactiveLease.closeTimer = null;
    leases.delete(inactiveTabId);
    inactive.push([inactiveTabId, inactiveLease]);
  }
  const ready = enqueueNativeOperation(async () => {
    for (const [inactiveTabId, inactiveLease] of inactive) {
      await closeTab(inactiveTabId, inactiveLease);
    }
    await previewBridge?.createTab(tabId, lease.partition);
  });
  lease.ready = ready;
  lease.createFailed = false;
  void ready.then(undefined, () => {
    if (lease.ready === ready) lease.createFailed = true;
  });
}

export function acquireDesktopTab(tabId: string, partition: string | null): AcquiredDesktopTab {
  let current = leases.get(tabId);
  if (!current) {
    current = {
      partition,
      references: 0,
      closeTimer: null,
      ready: Promise.resolve(),
      createFailed: false,
    };
    createTab(tabId, current);
  } else if (current.createFailed) {
    createTab(tabId, current);
  }
  if (current.closeTimer !== null) window.clearTimeout(current.closeTimer);
  current.references += 1;
  current.closeTimer = null;
  leases.set(tabId, current);
  const ready = current.ready;
  let released = false;

  return {
    ready,
    navigate: async (url, shouldNavigate = () => true) => {
      await ready;
      if (!shouldNavigate()) return;
      await previewBridge?.navigate(tabId, url);
    },
    release: () => {
      if (released) return;
      released = true;
      const lease = leases.get(tabId);
      if (lease !== current) return;
      lease.references = Math.max(0, lease.references - 1);
      if (lease.references > 0) return;
      lease.closeTimer = window.setTimeout(() => {
        const latest = leases.get(tabId);
        if (latest !== current || latest.references > 0) return;
        leases.delete(tabId);
        void enqueueNativeOperation(() => closeTab(tabId, latest)).catch(() => undefined);
      }, 0);
    },
  };
}

export async function navigateDesktopTab(
  tabId: string,
  partition: string | null,
  url: string,
  shouldNavigate: () => boolean = () => true,
): Promise<void> {
  const lease = acquireDesktopTab(tabId, partition);
  try {
    await lease.navigate(url, shouldNavigate);
  } finally {
    lease.release();
  }
}
