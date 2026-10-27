import { defineRule } from "@oxlint/plugins";
import * as Option from "effect/Option";

import { resolveReferenceOrigin } from "../utils.ts";

const TEST_FILE_PATTERN = /\.(?:test|spec)\.[cm]?[jt]sx?$/u;
const EFFECT_RUNTIME_METHODS = new Set([
  "runCallback",
  "runCallbackWith",
  "runFork",
  "runForkWith",
  "runPromise",
  "runPromiseExit",
  "runPromiseExitWith",
  "runPromiseWith",
  "runSync",
  "runSyncExit",
  "runSyncExitWith",
  "runSyncWith",
]);

// Existing manual runners are tracked as debt. The rule permits no net-new
// occurrences in these files, while unlisted test files must have zero.
const LEGACY_BASELINE = new Map<string, number>([
  ["apps/web/src/cloud/dpop.test.ts", 2],
  ["apps/web/src/environments/runtime/service.addSavedEnvironment.test.ts", 1],
  ["oxlint-plugin-coddy-studio/rules/no-manual-effect-runtime-in-tests.test.ts", 7],
  ["packages/client-runtime/src/relay/managedRelayState.test.ts", 1],
  ["packages/client-runtime/src/wsTransport.test.ts", 2],
]);

const baselineFor = (filename: string): number => {
  const normalized = filename.replaceAll("\\", "/");
  for (const [suffix, count] of LEGACY_BASELINE) {
    if (normalized.endsWith(suffix)) return count;
  }
  return 0;
};

const manualRunnerName = (
  context: Parameters<typeof resolveReferenceOrigin>[0],
  callee: unknown,
): Option.Option<string> => {
  return resolveReferenceOrigin(context, callee).pipe(
    Option.flatMap((origin) => {
      if (origin.kind !== "module") return Option.none();
      const effectMethod =
        origin.source === "effect/Effect" && origin.path.length === 1
          ? origin.path[0]
          : origin.source === "effect" && origin.path.length === 2 && origin.path[0] === "Effect"
            ? origin.path[1]
            : undefined;
      if (effectMethod !== undefined && EFFECT_RUNTIME_METHODS.has(effectMethod)) {
        return Option.some(`Effect.${effectMethod}`);
      }

      const managedRuntime =
        (origin.source === "effect/ManagedRuntime" &&
          origin.path.length === 1 &&
          origin.path[0] === "make") ||
        (origin.source === "effect" &&
          origin.path.length === 2 &&
          origin.path[0] === "ManagedRuntime" &&
          origin.path[1] === "make");
      return managedRuntime ? Option.some("ManagedRuntime.make") : Option.none();
    }),
  );
};

export default defineRule({
  meta: {
    type: "problem",
    docs: {
      description:
        "Disallow manually creating or running Effect runtimes in tests; use @effect/vitest.",
    },
  },
  create(context) {
    if (!TEST_FILE_PATTERN.test(context.filename)) return {};

    const allowedCount = baselineFor(context.filename);
    let occurrenceCount = 0;

    return {
      CallExpression(node) {
        const runner = manualRunnerName(context, node.callee);
        if (Option.isNone(runner)) return;

        occurrenceCount++;
        if (occurrenceCount <= allowedCount) return;

        context.report({
          node: node.callee,
          message: `Do not use ${runner.value} in tests. Use @effect/vitest with it.effect(...) and test layers instead.`,
        });
      },
    };
  },
});
