import { EnvironmentId } from "@coddy/contracts";
import { describe, expect, it } from "@effect/vitest";

import {
  BearerConnectionTarget,
  PrimaryConnectionTarget,
  RelayConnectionTarget,
  SshConnectionTarget,
} from "./model.ts";
import { decideStorageIdentity, storageIdentityTargetKey } from "./storageIdentity.ts";

const ENVIRONMENT_ID = EnvironmentId.make("environment-1");

describe("storageIdentityTargetKey", () => {
  it("uses one platform-owned key for the primary target without exposing its URLs", () => {
    const target = new PrimaryConnectionTarget({
      environmentId: ENVIRONMENT_ID,
      label: "Primary",
      httpBaseUrl: "https://user:secret@primary.example.test/private",
      wsBaseUrl: "wss://user:secret@primary.example.test/private",
    });

    expect(storageIdentityTargetKey(target)).toBe("platform:primary");
  });

  it("keys bearer targets only by their stable connection ID", () => {
    const target = new BearerConnectionTarget({
      environmentId: ENVIRONMENT_ID,
      label: "Bearer secret label",
      connectionId: "bearer-connection-1",
      serverEnvironmentId: null,
    });

    expect(storageIdentityTargetKey(target)).toBe("bearer:bearer-connection-1");
  });

  it("keys relay targets by their stable logical environment ID", () => {
    const target = new RelayConnectionTarget({
      environmentId: ENVIRONMENT_ID,
      label: "Relay secret label",
    });

    expect(storageIdentityTargetKey(target)).toBe("relay:environment-1");
  });

  it("keys SSH targets only by their stable connection ID", () => {
    const target = new SshConnectionTarget({
      environmentId: ENVIRONMENT_ID,
      label: "/Users/alice/.ssh/config",
      connectionId: "ssh-connection-1",
    });

    expect(storageIdentityTargetKey(target)).toBe("ssh:ssh-connection-1");
  });
});

describe("decideStorageIdentity", () => {
  it("bootstraps the first reported storage identity", () => {
    expect(decideStorageIdentity(null, "store-a")).toEqual({
      _tag: "Bootstrap",
      reported: "store-a",
    });
  });

  it("accepts a matching storage identity", () => {
    expect(decideStorageIdentity("store-a", "store-a")).toEqual({
      _tag: "Accepted",
      value: "store-a",
    });
  });

  it("reports a changed storage identity without accepting it", () => {
    expect(decideStorageIdentity("store-a", "store-b")).toEqual({
      _tag: "Changed",
      accepted: "store-a",
      reported: "store-b",
    });
  });

  it("keeps the accepted identity when an older server cannot report one", () => {
    expect(decideStorageIdentity("store-a", null)).toEqual({
      _tag: "Unverifiable",
      accepted: "store-a",
    });
  });

  it("remains unverifiable when neither side has an identity", () => {
    expect(decideStorageIdentity(null, null)).toEqual({
      _tag: "Unverifiable",
      accepted: null,
    });
  });
});
