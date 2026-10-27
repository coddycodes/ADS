import { assert, describe } from "@effect/vitest";

import { createOxlintRuleHarness, runOxlintRuleTests } from "../test/utils.ts";
import noManualEffectRuntimeInTests from "./no-manual-effect-runtime-in-tests.ts";

const rule = createOxlintRuleHarness("coddy/no-manual-effect-runtime-in-tests", {
  filename: "fixture.test.ts",
});

describe("coddy/no-manual-effect-runtime-in-tests", () => {
  rule.valid(
    "allows @effect/vitest effect tests",
    `
      import { it } from "@effect/vitest";
      import * as Effect from "effect/Effect";

      it.effect("runs an Effect", () => Effect.succeed("ok"));
    `,
  );

  const runtimeMethods = [
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
  ] as const;

  for (const method of runtimeMethods) {
    rule.invalid(
      `reports Effect.${method}`,
      `
        import * as Effect from "effect/Effect";

        test("runs an Effect", () => {
          Effect.${method}(Effect.succeed("ok"));
        });
      `,
      (output) => {
        assert.match(output, /Use @effect\/vitest with it\.effect/);
      },
    );
  }

  rule.invalid(
    "reports ManagedRuntime.make",
    `
      import * as Layer from "effect/Layer";
      import * as ManagedRuntime from "effect/ManagedRuntime";

      test("makes a runtime", () => {
        ManagedRuntime.make(Layer.empty);
      });
    `,
  );
});

const productionRule = createOxlintRuleHarness("coddy/no-manual-effect-runtime-in-tests");

productionRule.valid(
  "allows production runtime boundaries",
  `
    import * as Effect from "effect/Effect";

    export const main = () => Effect.runPromise(Effect.void);
  `,
);

const repoRoot = import.meta.dirname.replace(/[/\\]oxlint-plugin-coddy-studio[/\\]rules$/u, "");

runOxlintRuleTests(
  "no-manual-effect-runtime-in-tests",
  noManualEffectRuntimeInTests,
  {
    valid: [
      {
        name: "allows manual runtime boundaries outside test filenames",
        filename: "runtime.ts",
        code: `
          import * as Effect from "effect/Effect";
          export const main = () => Effect.runPromise(Effect.void);
        `,
      },
      {
        name: "allows shadowed Effect and ManagedRuntime bindings",
        filename: "fixture.test.ts",
        code: `
          import * as Effect from "effect/Effect";
          import * as ManagedRuntime from "effect/ManagedRuntime";
          function exercise(
            Effect: { runPromise(value: unknown): unknown },
            ManagedRuntime: { make(value: unknown): unknown },
          ) {
            return [Effect.runPromise("ok"), ManagedRuntime.make("layer")];
          }
          void exercise;
        `,
      },
      {
        name: "allows unrelated and mutable runtime aliases",
        filename: "fixture.test.ts",
        code: `
          import * as Effect from "effect/Effect";
          import { runPromise } from "other";
          let run = Effect.runPromise;
          run = runPromise;
          function exercise(Effect: { runSync(value: unknown): unknown }) {
            const local = Effect.runSync;
            return local(program);
          }
          run(program);
          void exercise;
        `,
      },
      {
        name: "allows aliases read only after direct or invoked closure writes",
        filename: "fixture.test.ts",
        code: `
          import * as Effect from "effect/Effect";
          const local = (value: unknown) => value;
          let assigned = Effect.runPromise;
          assigned = local;
          let destructured = Effect.runSync;
          [destructured] = [local];
          let updated: unknown = Effect.runFork;
          updated++;
          let closureWritten = Effect.runCallback;
          const replace = () => { closureWritten = local; };
          replace();
          assigned(program);
          destructured(program);
          (updated as typeof local)(program);
          closureWritten(program);
          {
            let assigned = local;
            assigned(program);
          }
        `,
      },
      {
        name: "allows runtime aliases after provable aliased object and nested immediate writer calls",
        filename: "fixture.test.ts",
        code: `
          import * as Effect from "effect/Effect";
          const local = (value: unknown) => value;

          let aliased = Effect.runPromise;
          const writer = () => { aliased = local; };
          const writerAlias = writer;
          writerAlias();
          aliased(program);

          let objectMethod = Effect.runSync;
          const writers = { replace() { objectMethod = local; } };
          const writersAlias = writers;
          writersAlias.replace();
          objectMethod(program);

          let nestedIife = Effect.runFork;
          const nestedWriter = () => { nestedIife = local; };
          (() => { nestedWriter(); })();
          nestedIife(program);

          let staticBlock = Effect.runCallback;
          const staticWriter = () => { staticBlock = local; };
          class InvokeWriter { static { staticWriter(); } }
          staticBlock(program);
          void InvokeWriter;
        `,
      },
      {
        name: "allows unrelated methods, dynamic members, comments, and strings",
        filename: "fixture.spec.tsx",
        code: `
          import * as Effect from "effect/Effect";
          import * as EffectRoot from "effect";
          import { Layer } from "effect";
          const key = "runPromise";
          const source = "Effect.runPromise(program)";
          // ManagedRuntime.make(layer)
          Effect.succeed("ok");
          Effect[key](program);
          Effect[0](program);
          helper();
          getEffect().runPromise(program);
          void [source, EffectRoot, Layer];
        `,
      },
      {
        name: "allows the exact legacy budget in an existing debt file",
        filename: `${repoRoot}/apps/web/src/cloud/dpop.test.ts`,
        code: `
          import * as Effect from "effect/Effect";
          Effect.runPromise(first);
          Effect.runPromise(second);
        `,
      },
    ],
    invalid: [
      {
        name: "reports namespace and named aliases in test files",
        filename: "fixture.test.mts",
        code: `
          import * as Fx from "effect/Effect";
          import { Effect as RootEffect } from "effect";
          import { ManagedRuntime as Runtime } from "effect";
          Fx.runPromise(program);
          RootEffect.runSync(program);
          Runtime.make(layer);
        `,
        errors: [
          /Do not use Effect\.runPromise/u,
          /Do not use Effect\.runSync/u,
          /Do not use ManagedRuntime\.make/u,
        ],
      },
      {
        name: "reports computed, optional, non-null, and asserted runtime calls",
        filename: "fixture.spec.cts",
        code: `
          import * as Effect from "effect/Effect";
          import * as ManagedRuntime from "effect/ManagedRuntime";
          Effect["runSync"](program);
          Effect.runFork?.(program);
          (Effect.runPromise!)(program);
          (ManagedRuntime as typeof ManagedRuntime).make(layer);
        `,
        errors: 4,
      },
      {
        name: "reports only occurrences beyond the legacy budget",
        filename: `${repoRoot}/apps/web/src/cloud/dpop.test.ts`,
        code: `
          import * as Effect from "effect/Effect";
          Effect.runPromise(first);
          Effect.runPromise(second);
          Effect.runPromise(third);
        `,
        errors: 1,
      },
      {
        name: "reports test JSX filenames",
        filename: "fixture.test.tsx",
        languageOptions: { parserOptions: { lang: "tsx", ecmaFeatures: { jsx: true } } },
        code: `
          import * as Effect from "effect/Effect";
          export const View = () => <button onClick={() => Effect.runSync(program)}>Run</button>;
        `,
        errors: 1,
      },
      {
        name: "reports CommonJS runtime aliases",
        filename: "fixture.test.cjs",
        languageOptions: { sourceType: "commonjs", parserOptions: { lang: "js" } },
        code: `
          const Fx = require("effect/Effect");
          const Runtime = require("effect/ManagedRuntime");
          Fx.runPromise(program);
          Runtime.make(layer);
        `,
        errors: [/Do not use Effect\.runPromise/u, /Do not use ManagedRuntime\.make/u],
      },
      {
        name: "reports named imports root namespaces and immutable runner aliases",
        filename: "fixture.test.ts",
        code: `
          import { runPromise, runSync as sync } from "effect/Effect";
          import * as Root from "effect";
          const fork = Root.Effect.runFork;
          runPromise(program);
          sync(program);
          fork(program);
        `,
        errors: [
          /Do not use Effect\.runPromise/u,
          /Do not use Effect\.runSync/u,
          /Do not use Effect\.runFork/u,
        ],
      },
      {
        name: "reports CommonJS destructured and root runtime APIs",
        filename: "fixture.test.cjs",
        languageOptions: { sourceType: "commonjs", parserOptions: { lang: "js" } },
        code: `
          const { runPromise: run } = require("effect/Effect");
          const { Effect, ManagedRuntime } = require("effect");
          const sync = Effect.runSync;
          run(program);
          sync(program);
          ManagedRuntime.make(layer);
        `,
        errors: [
          /Do not use Effect\.runPromise/u,
          /Do not use Effect\.runSync/u,
          /Do not use ManagedRuntime\.make/u,
        ],
      },
      {
        name: "reports satisfies as non-null optional and alias wrapper combinations",
        filename: "fixture.test.ts",
        code: `
          import * as Effect from "effect/Effect";
          (((Effect satisfies typeof Effect).runPromise as typeof Effect.runPromise)!)(program);
          ((Effect satisfies typeof Effect).runSync)?.(program);
          const run = ((((Effect satisfies typeof Effect) as typeof Effect)!).runFork)!;
          run(program);
        `,
        errors: 3,
      },
      {
        name: "reports stable let runtime aliases across function and closure scopes",
        filename: "fixture.test.ts",
        code: `
          import * as Effect from "effect/Effect";
          let run = Effect.runPromise;
          run(program);
          function nested() {
            let sync = Effect.runSync;
            sync(program);
          }
          const closure = () => {
            let fork = Effect.runFork;
            return () => fork(program);
          };
          void [nested, closure];
        `,
        errors: 3,
      },
      {
        name: "reports runtime provenance only at reads before preceding writes",
        filename: "fixture.test.ts",
        code: `
          import * as Effect from "effect/Effect";
          const local = (value: unknown) => value;

          let assigned = Effect.runPromise;
          assigned(program);
          assigned = local;
          assigned(program);

          let updated: unknown = Effect.runSync;
          (updated as typeof Effect.runSync)(program);
          updated++;
          (updated as typeof Effect.runSync)(program);

          let destructured = Effect.runFork;
          destructured(program);
          ({ destructured } = { destructured: local });
          destructured(program);

          let branchBefore = Effect.runPromiseExit;
          if (condition) branchBefore = local;
          branchBefore(program);

          let branchAfter = Effect.runSyncExit;
          branchAfter(program);
          if (condition) branchAfter = local;

          let uninvoked = Effect.runCallback;
          const replaceUninvoked = () => { uninvoked = local; };
          uninvoked(program);

          let calledBefore = Effect.runPromiseWith;
          const replaceBefore = () => { calledBefore = local; };
          replaceBefore();
          calledBefore(program);

          let calledAfter = Effect.runForkWith;
          const replaceAfter = () => { calledAfter = local; };
          calledAfter(program);
          replaceAfter();

          void [replaceUninvoked, replaceAfter];
        `,
        errors: 6,
      },
      {
        name: "does not invent runtime writes from references callbacks or unresolved aliases",
        filename: "fixture.test.ts",
        code: `
          import * as Effect from "effect/Effect";
          const local = (value: unknown) => value;

          let referenced = Effect.runPromise;
          const referencedWriter = () => { referenced = local; };
          void referencedWriter;
          register(referencedWriter);
          const storedWriter = referencedWriter;
          function returnWriter() { return referencedWriter; }
          Promise.resolve().then(referencedWriter);
          setTimeout(referencedWriter, 0);
          referenced(program);

          let anonymousCallback = Effect.runSync;
          Promise.resolve().then(() => { anonymousCallback = local; });
          anonymousCallback(program);

          let namedCallback = Effect.runFork;
          register(function callback() { namedCallback = local; });
          namedCallback(program);

          let shadowed = Effect.runCallback;
          const shadowedWriter = () => { shadowed = local; };
          {
            const shadowedWriter = () => undefined;
            shadowedWriter();
          }
          shadowed(program);

          let mutableAlias = Effect.runPromiseExit;
          const originalWriter = () => { mutableAlias = local; };
          let mutableWriter = originalWriter;
          mutableWriter = () => undefined;
          mutableWriter();
          mutableAlias(program);

          void [storedWriter, returnWriter, shadowedWriter];
        `,
        errors: 5,
      },
    ],
  },
  {
    cwd: repoRoot,
    languageOptions: { sourceType: "module", parserOptions: { lang: "ts" } },
  },
);
