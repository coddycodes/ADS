// @effect-diagnostics nodeBuiltinImport:off - Fixture exporter tests exercise Node-only tooling boundaries directly.
import * as NodePath from "node:path";

import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const io = vi.hoisted(() => ({
  directories: [] as string[],
  formatCalls: [] as Array<{
    command: string;
    args: ReadonlyArray<string>;
    options: Readonly<Record<string, unknown>>;
  }>,
  formatStatus: 0 as number | null,
  removals: [] as string[],
  writes: new Map<string, string>(),
}));

vi.mock("node:fs/promises", () => ({
  mkdir: async (path: string) => {
    io.directories.push(path);
  },
  rm: async (path: string) => {
    io.removals.push(path);
  },
  writeFile: async (path: string, contents: string) => {
    io.writes.set(path, contents);
  },
}));

vi.mock("node:child_process", () => ({
  spawnSync: (
    command: string,
    args: ReadonlyArray<string>,
    options: Readonly<Record<string, unknown>>,
  ) => {
    io.formatCalls.push({ command, args, options });
    return { status: io.formatStatus };
  },
}));

const outputDirectory = NodePath.resolve(import.meta.dirname, "../fixtures/rpc-wire");
const manifestPath = NodePath.join(outputDirectory, "manifest.json");

interface RpcManifest {
  readonly protocolVersion: string;
  readonly methods: ReadonlyArray<{ readonly name: string; readonly mode: string }>;
  readonly streamMethodCount: number;
  readonly expectedTopLevelStreamShapes: number;
  readonly expectedOrchestrationEventShapes: number;
  readonly streamShapeFixtures: ReadonlyArray<string>;
  readonly typedFailureFixtures: ReadonlyArray<string>;
  readonly staleMethodIdentifiers: ReadonlyArray<string>;
  readonly schemaFingerprints: Readonly<Record<string, string>>;
  readonly fixtures: ReadonlyArray<string>;
}

async function runExporter(): Promise<void> {
  await import("./export-rust-rpc-fixtures.ts");
}

function readManifest(): RpcManifest {
  const contents = io.writes.get(manifestPath);
  if (contents === undefined) {
    throw new Error("RPC fixture exporter did not write its manifest.");
  }
  return JSON.parse(contents) as RpcManifest;
}

beforeEach(() => {
  vi.resetModules();
  io.directories = [];
  io.formatCalls = [];
  io.formatStatus = 0;
  io.removals = [];
  io.writes.clear();
});

describe("RPC wire fixture exporter", () => {
  it("writes a deterministic, complete fixture tree", async () => {
    await runExporter();

    const manifest = readManifest();
    expect(io.removals).toEqual([outputDirectory]);
    expect(io.directories).toContain(outputDirectory);
    const repositoryRoot = NodePath.resolve(import.meta.dirname, "../../..");
    expect(io.formatCalls).toEqual([
      {
        command: process.execPath,
        args: [
          NodePath.join(repositoryRoot, "scripts", "run-local-vp.mjs"),
          "fmt",
          "--write",
          outputDirectory,
        ],
        options: {
          cwd: repositoryRoot,
          stdio: "inherit",
        },
      },
    ]);
    expect(manifest.methods).toHaveLength(158);
    expect(manifest.protocolVersion).toBe("effect-4.0.0-beta.107");
    expect(manifest.methods.filter(({ mode }) => mode === "stream")).toHaveLength(24);
    expect(manifest.streamMethodCount).toBe(24);
    expect(manifest.expectedTopLevelStreamShapes).toBe(79);
    expect(manifest.expectedOrchestrationEventShapes).toBe(24);
    expect(manifest.streamShapeFixtures).toHaveLength(79);
    expect(manifest.typedFailureFixtures).toHaveLength(335);
    expect(manifest.staleMethodIdentifiers).toEqual([
      "projects.add",
      "projects.list",
      "projects.remove",
    ]);
    expect(manifest.fixtures).toHaveLength(450);
    expect(manifest.fixtures).toContain("exit-response-too-large.json");
    expect(manifest.typedFailureFixtures).toContain("typed-failures/vcs__clone-04.json");
    expect(manifest.typedFailureFixtures).toEqual(
      expect.arrayContaining([
        "typed-failures/vcs__cancelClone-00.json",
        "typed-failures/vcs__cancelClone-01.json",
      ]),
    );
    expect(manifest.fixtures).toEqual([...manifest.fixtures].toSorted());
    expect(Object.keys(manifest.schemaFingerprints)).toHaveLength(414);

    for (const relativePath of manifest.fixtures) {
      const contents = io.writes.get(NodePath.join(outputDirectory, relativePath));
      expect(contents, relativePath).toBeDefined();
      expect(contents, relativePath).toMatch(/\n$/);
      expect(() => JSON.parse(contents!), relativePath).not.toThrow();
    }

    const firstRun = new Map(io.writes);
    vi.resetModules();
    io.directories = [];
    io.formatCalls = [];
    io.removals = [];
    io.writes.clear();
    await runExporter();

    expect(io.writes).toEqual(firstRun);
  });

  it.each([
    ["live", "The Git status stream fell behind. Subscribe again."],
    [
      "setup",
      "The Git status subscription fell behind while it was being set up. Subscribe again.",
    ],
  ] as const)(
    "exports the %s Git status retry failure shared by Rust and the client",
    async (kind, detail) => {
      await runExporter();
      const relativePath = `contract-shapes/subscribeVcsStatus__fell-behind-${kind}-failure.json`;
      const contents = io.writes.get(NodePath.join(outputDirectory, relativePath));
      expect(contents, "the explicit retry failure must be generated").toBeDefined();
      expect(readManifest().fixtures).toContain(relativePath);
      expect(JSON.parse(contents!)).toEqual({
        _tag: "Exit",
        requestId: "900719925474099312345",
        exit: {
          _tag: "Failure",
          cause: [
            {
              _tag: "Fail",
              error: {
                _tag: "GitCommandError",
                operation: "GitStatusBroadcaster.fellBehind",
                command: "git",
                cwd: "/repo",
                detail,
              },
            },
          ],
        },
      });
    },
  );

  it("writes a named-provider summary with a matching populated pull request", async () => {
    await runExporter();

    const contents = io.writes.get(
      NodePath.join(outputDirectory, "stream-shapes/subscribeVcsStatusSummary-02.json"),
    );
    expect(contents).toBeDefined();
    const fixture = JSON.parse(contents!) as {
      readonly values: ReadonlyArray<{
        readonly sourceControlProvider: { readonly kind: string } | null;
        readonly pr: { readonly provider: string } | null;
      }>;
    };
    const summary = fixture.values[0];

    expect(summary?.sourceControlProvider).not.toBeNull();
    expect(summary?.pr).not.toBeNull();
    expect(summary?.pr?.provider).toBe(summary?.sourceControlProvider?.kind);
  });

  it("exports the required terminal attachment sequence on the begin request", async () => {
    await runExporter();
    const contents = io.writes.get(
      NodePath.join(outputDirectory, "contract-shapes/terminal__beginInput-request.json"),
    );
    expect(contents).toBeDefined();
    expect(JSON.parse(contents!)).toMatchObject({
      tag: "terminal.beginInput",
      payload: { threadId: "thread-1", terminalId: "terminal-1", attachmentSequence: 0 },
    });
  });

  it("exports queued commands and delivery events with their optional fields", async () => {
    await runExporter();
    for (const [name, payload] of [
      ["thread__turn__start", { type: "thread.turn.start", queued: true }],
      ["thread__turn__steer", { type: "thread.turn.steer", messageId: "message-1" }],
      ["thread__turn__promote", { type: "thread.turn.promote", messageId: "message-1" }],
      [
        "thread__turn-delivery__resolve",
        { type: "thread.turn-delivery.resolve", action: "cancel" },
      ],
    ] as const) {
      const contents = io.writes.get(
        NodePath.join(outputDirectory, `contract-shapes/orchestration__${name}-request.json`),
      );
      expect(contents).toBeDefined();
      expect(JSON.parse(contents!)).toMatchObject({
        tag: "orchestration.dispatchCommand",
        payload,
      });
    }
    for (const [name, event] of [
      ["steer-requested", { type: "thread.turn-steer-requested", payload: { turnId: "turn-1" } }],
      ["delivery-held", { payload: { held: true, mode: "start", delivery: { state: "queued" } } }],
      ["delivery-steered", { payload: { turnId: "turn-1", held: false, mode: "steer" } }],
      ["delivery-withdrawn", { payload: { withdrawn: true } }],
    ] as const) {
      const contents = io.writes.get(
        NodePath.join(outputDirectory, `contract-shapes/orchestration__${name}-event.json`),
      );
      expect(contents).toBeDefined();
      expect(JSON.parse(contents!)).toMatchObject({ exit: { _tag: "Success", value: [event] } });
    }
  });

  it("exports the accepted fresh conversation notice in the delivery event", async () => {
    await runExporter();
    const path = "contract-shapes/orchestration__delivery-new-conversation-event.json";
    const contents = io.writes.get(NodePath.join(outputDirectory, path));
    expect(contents).toBeDefined();
    expect(readManifest().fixtures).toContain(path);
    expect(JSON.parse(contents!)).toMatchObject({
      _tag: "Exit",
      exit: {
        value: [
          {
            type: "thread.turn-delivery-updated",
            payload: {
              delivery: { state: "delivered", reason: "startedNewConversation" },
            },
          },
        ],
      },
    });
  });

  it.each([
    [7, "7"],
    [null, "unknown"],
  ] as const)("rejects formatter status %s", async (status, message) => {
    io.formatStatus = status;

    await expect(runExporter()).rejects.toThrow(`Failed to format RPC fixtures (exit ${message}).`);
    expect(io.writes.has(manifestPath)).toBe(true);
  });
});
