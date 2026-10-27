import type { ScopedThreadRef } from "@coddy/contracts";

export interface AttachmentAdmissionAuthority {
  readonly storageInstanceId: string;
  readonly hostIdentity: string;
}

/** Application-local intent, never a persisted draft or view-owned resource. */
export function createAttachmentAdmissionOwner<Value>() {
  const records = new Map<
    string,
    {
      lease: object | null;
      authority: AttachmentAdmissionAuthority | null;
      value?: Value;
    }
  >();
  const listeners = new Map<string, Set<() => void>>();
  const key = (scope: ScopedThreadRef) => JSON.stringify([scope.environmentId, scope.threadId]);
  const publish = (scopeKey: string) => {
    for (const listener of listeners.get(scopeKey) ?? []) listener();
  };
  return {
    get: (scope: ScopedThreadRef): Value | undefined => records.get(key(scope))?.value,
    isBusy: (scope: ScopedThreadRef): boolean => records.get(key(scope))?.lease != null,
    subscribe: (scope: ScopedThreadRef, listener: () => void) => {
      const scopeKey = key(scope);
      let current = listeners.get(scopeKey);
      if (!current) {
        current = new Set();
        listeners.set(scopeKey, current);
      }
      current.add(listener);
      return () => {
        current.delete(listener);
        if (current.size === 0) listeners.delete(scopeKey);
      };
    },
    claim: (scope: ScopedThreadRef): object | null => {
      const scopeKey = key(scope);
      const current = records.get(scopeKey);
      if (current?.lease) return null;
      const lease = {};
      records.set(scopeKey, { ...current, authority: current?.authority ?? null, lease });
      publish(scopeKey);
      return lease;
    },
    authorize: (
      scope: ScopedThreadRef,
      lease: object,
      authority: AttachmentAdmissionAuthority,
    ): boolean => {
      const current = records.get(key(scope));
      if (current?.lease !== lease) return false;
      if (
        current.value !== undefined &&
        current.authority !== null &&
        (current.authority.storageInstanceId !== authority.storageInstanceId ||
          current.authority.hostIdentity !== authority.hostIdentity)
      )
        return false;
      current.authority = authority;
      return true;
    },
    set: (scope: ScopedThreadRef, lease: object, value: Value): boolean => {
      const current = records.get(key(scope));
      if (current?.lease !== lease) return false;
      current.value = value;
      return true;
    },
    delete: (scope: ScopedThreadRef, lease?: object): boolean => {
      const current = records.get(key(scope));
      if (!current || (current.lease !== null && current.lease !== lease)) return false;
      delete current.value;
      if (current.lease === null) records.delete(key(scope));
      return true;
    },
    finish: (scope: ScopedThreadRef, lease: object): void => {
      const scopeKey = key(scope);
      const current = records.get(scopeKey);
      if (current?.lease !== lease) return;
      current.lease = null;
      if (current.value === undefined) records.delete(scopeKey);
      publish(scopeKey);
    },
    dispose: (): void => {
      const keys = [...records.keys()];
      records.clear();
      for (const scopeKey of keys) publish(scopeKey);
      listeners.clear();
    },
  };
}
