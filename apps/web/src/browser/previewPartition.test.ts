import { EnvironmentId } from "@bibcode/contracts";
import { describe, expect, it, vi } from "vite-plus/test";

const primary = vi.hoisted(() => ({ id: null as string | null }));
vi.mock("~/rpc/atomRegistry", () => ({ appAtomRegistry: { get: () => primary.id } }));
vi.mock("~/state/primaryEnvironment", () => ({ primaryEnvironmentIdAtom: "primary" }));

import { previewPartitionFor } from "./previewPartition";

describe("previewPartitionFor", () => {
  it("keeps the local environment on the shared legacy profile", () => {
    primary.id = "env-local";
    expect(previewPartitionFor(EnvironmentId.make("env-local"))).toBeNull();
  });

  it("partitions every other environment by its id", () => {
    primary.id = "env-local";
    expect(previewPartitionFor(EnvironmentId.make("env-ssh"))).toBe("env-ssh");
  });

  it("partitions every environment while the primary one is unknown", () => {
    primary.id = null;
    expect(previewPartitionFor(EnvironmentId.make("env-ssh"))).toBe("env-ssh");
  });
});
