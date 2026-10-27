import { EnvironmentId, ThreadId } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";
import { createAttachmentAdmissionOwner } from "./attachmentAdmissionOwner.ts";

const scope = { environmentId: EnvironmentId.make("host"), threadId: ThreadId.make("thread") };
const authority = { storageInstanceId: "store", hostIdentity: "host-key" };
describe("application attachment admission owner", () => {
  it("retains immutable intent after views unsubscribe and permits only one logical send", () => {
    const owner = createAttachmentAdmissionOwner<{ commandId: string }>();
    const unsubscribe = owner.subscribe(scope, () => {});
    const lease = owner.claim(scope)!;
    expect(owner.claim(scope)).toBeNull();
    expect(owner.authorize(scope, lease, authority)).toBe(true);
    unsubscribe();
    expect(owner.set(scope, lease, { commandId: "original" })).toBe(true);
    owner.finish(scope, lease);
    expect(owner.get(scope)).toEqual({ commandId: "original" });
    const nextView = owner.claim(scope)!;
    expect(owner.authorize(scope, nextView, { ...authority })).toBe(true);
    expect(owner.get(scope)).toEqual({ commandId: "original" });
  });
  it.each(["storageInstanceId", "hostIdentity"] as const)(
    "keeps unresolved intent when %s changes",
    (key) => {
      const owner = createAttachmentAdmissionOwner<{ commandId: string }>();
      const lease = owner.claim(scope)!;
      owner.authorize(scope, lease, authority);
      owner.set(scope, lease, { commandId: "original" });
      owner.finish(scope, lease);
      const next = owner.claim(scope)!;
      expect(owner.authorize(scope, next, { ...authority, [key]: "changed" })).toBe(false);
      expect(owner.get(scope)).toEqual({ commandId: "original" });
    },
  );
  it("isolates environment/thread keys and ignores writes from a disposed owner lifetime", () => {
    const owner = createAttachmentAdmissionOwner<string>();
    const lease = owner.claim(scope)!;
    const other = { ...scope, environmentId: EnvironmentId.make("other") };
    expect(owner.claim(other)).not.toBeNull();
    owner.dispose();
    expect(owner.set(scope, lease, "late result")).toBe(false);
    expect(owner.get(scope)).toBeUndefined();
  });
});
