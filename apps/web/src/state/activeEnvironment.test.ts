import { EnvironmentId } from "@coddy/contracts";
import { afterEach, describe, expect, it } from "vite-plus/test";

import { appAtomRegistry, resetAppAtomRegistryForTests } from "../rpc/atomRegistry";
import {
  activeEnvironmentIdAtom,
  readActiveEnvironmentId,
  setActiveEnvironmentId,
} from "./activeEnvironment";

afterEach(resetAppAtomRegistryForTests);

describe("active environment", () => {
  it("starts unknown and reads writes from the shared registry", () => {
    expect(readActiveEnvironmentId()).toBeNull();
    const selected = EnvironmentId.make("remote");
    setActiveEnvironmentId(selected);
    expect(readActiveEnvironmentId()).toBe(selected);
    expect(appAtomRegistry.get(activeEnvironmentIdAtom)).toBe(selected);
    setActiveEnvironmentId(null);
    expect(readActiveEnvironmentId()).toBeNull();
  });
  it("uses the new registry after a test reset", () => {
    setActiveEnvironmentId(EnvironmentId.make("local"));
    resetAppAtomRegistryForTests();
    expect(readActiveEnvironmentId()).toBeNull();
  });
});
