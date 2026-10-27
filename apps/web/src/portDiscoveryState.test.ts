import { EnvironmentId, ThreadId, type DiscoveredLocalServer } from "@coddy/contracts";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  query: {} as { data?: { servers: ReadonlyArray<DiscoveredLocalServer> } },
  descriptors: [] as unknown[],
}));

vi.mock("react", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react")>()),
  useMemo: (factory: () => unknown) => factory(),
}));
vi.mock("./state/preview", () => ({
  previewEnvironment: {
    discoveredServers: (input: unknown) => {
      harness.descriptors.push(input);
      return { descriptor: input };
    },
  },
}));
vi.mock("./state/query", () => ({
  useEnvironmentQuery: (descriptor: unknown) => {
    harness.descriptors.push(descriptor);
    return harness.query;
  },
}));

import {
  useDiscoveredPorts,
  useTerminalDiscoveredPorts,
  useThreadDiscoveredPorts,
} from "./portDiscoveryState";

const environmentId = EnvironmentId.make("environment-1");
const threadId = ThreadId.make("thread-1");
const ports = [
  {
    host: "localhost",
    port: 3000,
    url: "http://localhost:3000",
    processName: "node",
    pid: 1,
    terminal: { threadId, terminalId: "terminal-1" },
  },
  {
    host: "localhost",
    port: 4000,
    url: "http://localhost:4000",
    processName: null,
    pid: null,
    terminal: null,
  },
] as ReadonlyArray<DiscoveredLocalServer>;

beforeEach(() => {
  harness.query = {};
  harness.descriptors.length = 0;
});

describe("port discovery hooks", () => {
  it("returns a stable empty list without an environment or query data", () => {
    const first = useDiscoveredPorts(null);
    const second = useDiscoveredPorts(environmentId);
    expect(first).toEqual([]);
    expect(second).toBe(first);
    expect(harness.descriptors).toContain(null);
  });

  it("returns discovered ports and builds an environment descriptor", () => {
    harness.query = { data: { servers: ports } };
    expect(useDiscoveredPorts(environmentId)).toBe(ports);
    expect(harness.descriptors).toContainEqual({ environmentId, input: {} });
  });

  it("filters by thread and terminal while guarding incomplete selections", () => {
    harness.query = { data: { servers: ports } };
    expect(useThreadDiscoveredPorts({ environmentId, threadId })).toEqual([ports[0]]);
    expect(useThreadDiscoveredPorts({ environmentId, threadId: null })).toEqual([]);
    expect(
      useTerminalDiscoveredPorts({ environmentId, threadId, terminalId: "terminal-1" }),
    ).toEqual([ports[0]]);
    expect(useTerminalDiscoveredPorts({ environmentId, threadId, terminalId: "other" })).toEqual(
      [],
    );
    expect(
      useTerminalDiscoveredPorts({ environmentId, threadId: null, terminalId: "terminal-1" }),
    ).toEqual([]);
    expect(useTerminalDiscoveredPorts({ environmentId, threadId, terminalId: null })).toEqual([]);
  });
});
