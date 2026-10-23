// @effect-diagnostics nodeBuiltinImport:off
import * as NodeCrypto from "node:crypto";
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";

import { describe, expect, it } from "@effect/vitest";
import * as Arbitrary from "effect/Arbitrary";
import * as DateTime from "effect/DateTime";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import type * as SchemaAST from "effect/SchemaAST";

import {
  OrchestrationEvent,
  OrchestrationReadModel,
  ORCHESTRATION_WS_METHODS,
} from "./orchestration.ts";
import { WS_METHODS, WsRpcGroup } from "./rpc.ts";
import {
  ServerProcessDiagnosticsResult,
  ServerProcessResourceHistoryResult,
  ServerSignalProcessInput,
} from "./server.ts";

const StreamSchemaTypeId = "~effect/rpc/RpcSchema/StreamSchema";
const decodeProcessDiagnostics = Schema.decodeUnknownSync(ServerProcessDiagnosticsResult);
const decodeProcessResourceHistory = Schema.decodeUnknownSync(ServerProcessResourceHistoryResult);
const decodeSignalProcessInput = Schema.decodeUnknownSync(ServerSignalProcessInput);
const decodeOrchestrationReadModel = Schema.decodeUnknownSync(OrchestrationReadModel);

interface Manifest {
  readonly methods: ReadonlyArray<{
    readonly name: string;
    readonly mode: "stream" | "unary";
  }>;
  readonly streamMethodCount: number;
  readonly expectedTopLevelStreamShapes: number;
  readonly expectedOrchestrationEventShapes: number;
  readonly streamShapeFixtures: ReadonlyArray<string>;
  readonly typedFailureFixtures: ReadonlyArray<string>;
  readonly schemaFingerprints: Readonly<Record<string, string>>;
  readonly staleMethodIdentifiers: ReadonlyArray<string>;
  readonly fixtures: ReadonlyArray<string>;
}

const countShapes = (ast: {
  readonly _tag: string;
  readonly types?: ReadonlyArray<unknown>;
}): number => (ast._tag === "Union" ? (ast.types?.length ?? 0) : 1);

const schemaMembers = (ast: SchemaAST.AST): ReadonlyArray<SchemaAST.AST> =>
  ast._tag === "Union" ? ast.types : [ast];

const safeMethodName = (method: string): string => method.replaceAll(".", "__");

describe("Rust RPC fixture parity", () => {
  const fixtureDirectory = NodePath.resolve(import.meta.dirname, "../fixtures/rpc-wire");
  const readFixture = (name: string): unknown =>
    JSON.parse(NodeFS.readFileSync(NodePath.join(fixtureDirectory, name), "utf8")) as unknown;

  it("decodes the populated HTTP read model emitted by the Rust snapshot projection", () => {
    const fixture: unknown = JSON.parse(
      NodeFS.readFileSync(
        NodePath.resolve(fixtureDirectory, "../http-orchestration/full-read-model.json"),
        "utf8",
      ),
    );
    const decoded = decodeOrchestrationReadModel(fixture);

    expect(decoded.snapshotSequence).toBe(42);
    expect(decoded.projects.map((project) => project.id)).toEqual([
      "project-live",
      "project-deleted",
    ]);
    expect(decoded.threads.map((thread) => thread.id)).toEqual([
      "thread-live",
      "thread-archived",
      "thread-deleted",
    ]);
    expect(decoded.threads[0]?.messages[0]?.text).toBe("Owned message");
    expect(decoded.threads[0]?.activities[0]?.tone).toBe("warning");
    expect(decoded.threads[0]?.proposedPlans[0]?.id).toBe("plan-live");
    expect(decoded.threads[0]?.checkpoints[0]?.turnId).toBe("turn-live");
    expect(decoded.threads[1]?.archivedAt).not.toBeNull();
    expect(decoded.threads[2]?.deletedAt).not.toBeNull();
    expect(decoded.projects[1]?.deletedAt).not.toBeNull();
    expect(decoded.threads[0]?.session).not.toHaveProperty("providerInstanceId");
    expect(decoded.threads[0]?.latestTurn).not.toHaveProperty("sourceProposedPlan");
    // The raw public payload carries additive shell fields; the full-thread
    // decoder strips them while the Rust golden test pins their wire values.
    for (const field of [
      "latestUserMessageAt",
      "hasPendingApprovals",
      "hasPendingUserInput",
      "hasActionableProposedPlan",
      "unresolvedDelivery",
    ]) {
      expect(decoded.threads[0]).not.toHaveProperty(field);
    }
  });

  it("pins the remote update snapshot wire shape the Rust mirror round-trips", () => {
    const fixture = readFixture("contract-shapes/updater__status-success.json") as {
      readonly exit: { readonly value: unknown };
    };
    expect(fixture.exit.value).toEqual({
      serverVersion: "0.6.2",
      latestVersion: "0.6.4",
      state: "downloading",
      error: null,
      support: { installMode: "interactive", reason: "available", installKind: "unknown" },
      downloadPercent: 42,
      targetVersion: "0.6.4",
      installStage: null,
    });
  });

  it("tracks bounded provider terminal activity command wire fixtures", () => {
    const hinted = readFixture("contract-shapes/terminal__open-provider-activity-request.json") as {
      readonly payload: {
        readonly env: Readonly<Record<string, string>>;
        readonly command: {
          readonly env: Readonly<Record<string, string>>;
          readonly activity: Readonly<Record<string, unknown>>;
        };
      };
    };
    const unhinted = readFixture(
      "contract-shapes/terminal__open-without-provider-activity-request.json",
    ) as {
      readonly payload: { readonly command: Readonly<Record<string, unknown>> };
    };

    expect(hinted.payload.command.activity).toEqual({
      driverKind: "codex",
      providerInstanceId: "codex_personal",
    });
    expect(hinted.payload.command.env).toEqual({
      COMMAND_ONLY: "command",
      SHARED: "command",
    });
    expect(hinted.payload.env).toEqual({
      COMMAND_ONLY: "command",
      RUNTIME_ONLY: "runtime",
      SHARED: "runtime",
    });
    expect(hinted.payload.command.activity).not.toHaveProperty("strategy");
    expect(hinted.payload.command.activity).not.toHaveProperty("url");
    expect(hinted.payload.command.activity).not.toHaveProperty("port");
    expect(hinted.payload.command.activity).not.toHaveProperty("socket");
    expect(hinted.payload.command.activity).not.toHaveProperty("token");
    expect(hinted.payload.command.activity).not.toHaveProperty("nativeSessionId");
    expect(unhinted.payload.command).not.toHaveProperty("activity");
  });

  it("tracks client-only targeted cancellation wire fixtures", () => {
    // Mutation caught: serializing provider-native identities or caller-computed descendant sets.
    const cancel = readFixture("contract-shapes/activity__cancelSubtree-request.json") as {
      readonly tag: string;
      readonly payload: Readonly<Record<string, unknown>>;
    };
    const retry = readFixture(
      "contract-shapes/activity__retrySubtreeCancellation-request.json",
    ) as {
      readonly tag: string;
      readonly payload: Readonly<Record<string, unknown>>;
    };

    expect(cancel.tag).toBe("activity.cancelSubtree");
    expect(cancel.payload).toEqual({
      scope: { _tag: "thread", threadId: "thread-1" },
      scopeId: "thread:thread-1",
      actorId: "actor:child-1",
      expectedControlRevision: 3,
    });
    expect(retry.tag).toBe("activity.retrySubtreeCancellation");
    expect(retry.payload).toEqual({
      scope: { _tag: "thread", threadId: "thread-1" },
      scopeId: "thread:thread-1",
      rootActorId: "actor:child-1",
      expectedOperationRevision: 4,
    });
    for (const payload of [cancel.payload, retry.payload]) {
      expect(payload).not.toHaveProperty("nativeThreadId");
      expect(payload).not.toHaveProperty("nativeActorId");
      expect(payload).not.toHaveProperty("descendantIds");
    }
  });

  it("tracks attributed diagnostics and identity-bound signal wire fixtures", () => {
    const diagnostics = readFixture(
      "contract-shapes/server__getProcessDiagnostics-success.json",
    ) as {
      readonly exit: { readonly value: unknown };
    };
    const history = readFixture(
      "contract-shapes/server__getProcessResourceHistory-success.json",
    ) as {
      readonly exit: { readonly value: unknown };
    };
    const signal = readFixture("contract-shapes/server__signalProcess-request.json") as {
      readonly payload: unknown;
    };

    expect(diagnostics.exit.value).toMatchObject({
      totals: {
        combined: { cpuPercent: 3, rssBytes: 30, processCount: 2 },
        core: { cpuPercent: 1, rssBytes: 10, processCount: 1 },
        external: { cpuPercent: 2, rssBytes: 20, processCount: 1 },
      },
      uiCoverage: { status: "notApplicable", message: { _tag: "None" } },
      processes: [
        {
          processKey: "42:100",
          scope: "external",
          kind: "provider",
          label: "Codex",
          confidence: "exact",
        },
      ],
    });
    expect(diagnostics.exit.value).not.toHaveProperty("processCount");
    expect(diagnostics.exit.value).not.toHaveProperty("totalRssBytes");
    expect(diagnostics.exit.value).not.toHaveProperty("totalCpuPercent");
    expect(history.exit.value).toMatchObject({
      cpuSecondsApprox: { combined: 1.5, core: 1, external: 0.5 },
      uiCoverage: { status: "partial", message: { _tag: "Some" } },
      buckets: [
        {
          cpuPercent: {
            average: { combined: 3, core: 1, external: 2 },
            peak: { combined: 6, core: 2, external: 4 },
          },
          rssBytes: {
            average: { combined: 30, core: 10, external: 20 },
            peak: { combined: 60, core: 20, external: 40 },
          },
          maxProcessCount: { combined: 2, core: 1, external: 1 },
        },
      ],
      processes: [{ processKey: "42:100", scope: "external", kind: "provider" }],
    });
    expect(history.exit.value).not.toHaveProperty("totalCpuSecondsApprox");
    expect(history.exit.value).not.toHaveProperty("topProcesses");
    expect(signal.payload).toEqual({
      pid: 42,
      processKey: "42:100",
      signal: "SIGINT",
    });
  });

  it("accepts attributed diagnostics and identity-bound signal payloads", () => {
    const startedAt = DateTime.makeUnsafe("1970-01-01T00:00:00Z");
    const endedAt = DateTime.makeUnsafe("1970-01-01T00:00:01Z");
    const totals = {
      combined: { cpuPercent: 3, rssBytes: 30, processCount: 2 },
      core: { cpuPercent: 1, rssBytes: 10, processCount: 1 },
      external: { cpuPercent: 2, rssBytes: 20, processCount: 1 },
    };
    const attribution = {
      processKey: "42:100",
      scope: "external",
      kind: "provider",
      label: "Codex",
      confidence: "exact",
    };

    expect(() =>
      decodeProcessDiagnostics({
        serverPid: 1,
        readAt: startedAt,
        totals,
        uiCoverage: { status: "notApplicable", message: Option.none() },
        processes: [
          {
            pid: 42,
            ppid: 1,
            pgid: Option.none(),
            status: "Run",
            cpuPercent: 2,
            rssBytes: 20,
            elapsed: "00:00:01",
            command: "codex",
            depth: 1,
            childPids: [],
            ...attribution,
          },
        ],
        error: Option.none(),
      }),
    ).not.toThrow();

    expect(() =>
      decodeProcessResourceHistory({
        readAt: endedAt,
        windowMs: 60_000,
        bucketMs: 1_000,
        sampleIntervalMs: 500,
        retainedSampleCount: 2,
        cpuSecondsApprox: { combined: 1.5, core: 1, external: 0.5 },
        uiCoverage: { status: "partial", message: Option.some("UI coverage") },
        buckets: [
          {
            startedAt,
            endedAt,
            cpuPercent: {
              average: { combined: 3, core: 1, external: 2 },
              peak: { combined: 6, core: 2, external: 4 },
            },
            rssBytes: {
              average: { combined: 30, core: 10, external: 20 },
              peak: { combined: 60, core: 20, external: 40 },
            },
            maxProcessCount: { combined: 2, core: 1, external: 1 },
          },
        ],
        processes: [
          {
            pid: 42,
            ppid: 1,
            command: "codex",
            depth: 1,
            ...attribution,
            firstSeenAt: startedAt,
            lastSeenAt: endedAt,
            currentCpuPercent: 2,
            avgCpuPercent: 2,
            maxCpuPercent: 4,
            cpuSecondsApprox: 0.5,
            currentRssBytes: 20,
            maxRssBytes: 40,
            sampleCount: 2,
          },
        ],
        error: Option.none(),
      }),
    ).not.toThrow();

    expect(
      decodeSignalProcessInput({
        pid: 42,
        processKey: "42:100",
        signal: "SIGINT",
      }),
    ).toEqual({
      pid: 42,
      processKey: "42:100",
      signal: "SIGINT",
    });
  });

  it("tracks the executable TypeScript RPC schemas without stale manifests", () => {
    const manifest = JSON.parse(
      NodeFS.readFileSync(NodePath.join(fixtureDirectory, "manifest.json"), "utf8"),
    ) as Manifest;
    const methods = [...WsRpcGroup.requests.values()]
      .map((rpc) => ({
        name: rpc._tag,
        mode:
          StreamSchemaTypeId in (rpc.successSchema as object)
            ? ("stream" as const)
            : ("unary" as const),
      }))
      .toSorted((left, right) => left.name.localeCompare(right.name));
    const topLevelStreamShapeCount = [...WsRpcGroup.requests.values()]
      .filter((rpc) => StreamSchemaTypeId in (rpc.successSchema as object))
      .reduce((count, rpc) => {
        const streamSchema = rpc.successSchema as unknown as {
          readonly success: {
            readonly ast: { readonly _tag: string; readonly types?: ReadonlyArray<unknown> };
          };
        };
        return count + countShapes(streamSchema.success.ast);
      }, 0);
    const activeNames = new Set<string>(methods.map(({ name }) => name));
    const staleMethodIdentifiers = [
      ...new Set([...Object.values(WS_METHODS), ...Object.values(ORCHESTRATION_WS_METHODS)]),
    ]
      .filter((name) => !activeNames.has(name))
      .toSorted();
    const expectedStreamFixtures: Array<string> = [];
    const expectedTypedFailureFixtures: Array<string> = [];
    const expectedSchemaFingerprints: Record<string, string> = {};
    for (const rpc of [...WsRpcGroup.requests.values()].toSorted((left, right) =>
      left._tag.localeCompare(right._tag),
    )) {
      if (StreamSchemaTypeId in (rpc.successSchema as object)) {
        const streamSchema = rpc.successSchema as unknown as {
          readonly success: { readonly ast: SchemaAST.AST };
        };
        for (const [index, member] of schemaMembers(streamSchema.success.ast).entries()) {
          const fixture = `stream-shapes/${safeMethodName(rpc._tag)}-${String(index).padStart(2, "0")}.json`;
          expectedStreamFixtures.push(fixture);
          expectedSchemaFingerprints[fixture] = NodeCrypto.createHash("sha256")
            .update(JSON.stringify(member))
            .digest("hex");
        }
      }

      for (const [index, member] of schemaMembers(rpc.errorSchema.ast).entries()) {
        try {
          Arbitrary.schema(Schema.make(member));
        } catch (cause) {
          // Mirrors the exporter: members with no finite generation path, such
          // as ones containing Schema.Never, have no typed-failure fixture.
          if (
            cause instanceof Error &&
            cause.message.includes("without a finite generation path")
          ) {
            continue;
          }
          throw cause;
        }
        const fixture = `typed-failures/${safeMethodName(rpc._tag)}-${String(index).padStart(2, "0")}.json`;
        expectedTypedFailureFixtures.push(fixture);
        expectedSchemaFingerprints[fixture] = NodeCrypto.createHash("sha256")
          .update(JSON.stringify(member))
          .digest("hex");
      }
    }

    expect(manifest.methods).toEqual(methods);
    expect(manifest.methods).toEqual(
      expect.arrayContaining([
        { name: "activity.getSnapshot", mode: "unary" },
        { name: "activity.listRoster", mode: "unary" },
        { name: "activity.listDetail", mode: "unary" },
        { name: "activity.cancelSubtree", mode: "unary" },
        { name: "activity.retrySubtreeCancellation", mode: "unary" },
        { name: "subscribeActivity", mode: "stream" },
        { name: "subscribeVcsStatusSummary", mode: "stream" },
        { name: "subscribeWorktreeCatalog", mode: "stream" },
        { name: "vcs.refreshWorktreeCatalog", mode: "unary" },
        { name: "worktree.updateDiscoveryPolicy", mode: "unary" },
      ]),
    );
    expect(manifest.streamMethodCount).toBe(methods.filter(({ mode }) => mode === "stream").length);
    expect(manifest.expectedTopLevelStreamShapes).toBe(topLevelStreamShapeCount);
    expect(manifest.expectedOrchestrationEventShapes).toBe(countShapes(OrchestrationEvent.ast));
    expect(manifest.streamShapeFixtures).toEqual(expectedStreamFixtures);
    expect(manifest.typedFailureFixtures).toEqual(expectedTypedFailureFixtures);
    expect(manifest.schemaFingerprints).toEqual(expectedSchemaFingerprints);
    expect(manifest.staleMethodIdentifiers).toEqual(staleMethodIdentifiers);
    expect(new Set(manifest.fixtures).size).toBe(manifest.fixtures.length);
    for (const fixture of manifest.fixtures) {
      expect(NodeFS.existsSync(NodePath.join(fixtureDirectory, fixture))).toBe(true);
    }
  });
});
