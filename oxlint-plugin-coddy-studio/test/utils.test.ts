import * as NodeServices from "@effect/platform-node/NodeServices";
import { defineRule } from "@oxlint/plugins";
import { assert, describe, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Option from "effect/Option";

import {
  createOxlintRuleHarness,
  expectOxlintRuleFailure,
  resolveOxlintInvocation,
  runOxlintRuleTests,
} from "./utils.ts";
import {
  getReferenceBinding,
  getPropertyName,
  isIdentifier,
  resolveReferenceOrigin,
  type ReferenceOrigin,
  unwrapExpression,
} from "../utils.ts";

describe("AST utilities", () => {
  it("rejects malformed unknown values that a parser cannot produce", () => {
    assert.isTrue(Option.isNone(unwrapExpression(undefined)));
    assert.isTrue(Option.isNone(unwrapExpression(null)));
    assert.isTrue(Option.isNone(unwrapExpression({})));
    assert.isTrue(Option.isNone(unwrapExpression({ type: 1 })));
    assert.isTrue(Option.isNone(getPropertyName({ type: "Literal", value: 1 })));
    assert.isFalse(isIdentifier(Option.none()));
  });

  it("unwraps the parser wrapper shape that cannot be retained in the Oxlint AST", () => {
    const identifier = { type: "Identifier", name: "value" };
    const wrapped = { type: "ParenthesizedExpression", expression: identifier };
    assert.strictEqual<unknown>(Option.getOrThrow(unwrapExpression(wrapped)), identifier);
  });
});

const utilityProbeRule = defineRule({
  createOnce(context) {
    return {
      PrivateIdentifier(node) {
        assert.equal(Option.getOrThrow(getPropertyName(node)), "platform");
      },
      TSTypeAssertion(node) {
        assert.equal(Option.getOrThrow(unwrapExpression(node)).type, "Identifier");
      },
      MemberExpression(node) {
        if (node.property.type === "Identifier") {
          assert.isTrue(Option.isNone(getReferenceBinding(context, node.property)));
          assert.isTrue(Option.isNone(resolveReferenceOrigin(context, node.property)));
          assert.isTrue(isIdentifier(Option.some(node.property)));
          assert.isTrue(isIdentifier(Option.some(node.property), node.property.name));
          assert.isFalse(isIdentifier(Option.some(node.property), "different"));
        }
      },
      Program(node) {
        assert.isTrue(Option.isNone(getReferenceBinding(context, node)));
        assert.isTrue(Option.isNone(getReferenceBinding(context, null)));
        assert.isTrue(Option.isNone(resolveReferenceOrigin(context, node)));
        assert.isTrue(Option.isNone(resolveReferenceOrigin(context, null)));
      },
    };
  },
});

runOxlintRuleTests(
  "utility-probe",
  utilityProbeRule,
  {
    valid: [
      {
        name: "uses parser-produced private, assertion, and property nodes",
        code: `
          class RuntimeInfo {
            #platform = "win32";
            read(value: unknown) {
              const asserted = <unknown>value;
              return [this.#platform, object.property, asserted];
            }
          }
          void RuntimeInfo;
        `,
      },
    ],
    invalid: [],
  },
  { languageOptions: { sourceType: "module", parserOptions: { lang: "ts" } } },
);

const originProbeRule = defineRule({
  createOnce(context) {
    return {
      CallExpression(node) {
        if (node.callee.type !== "Identifier" || !node.callee.name.startsWith("probe")) return;
        if (node.callee.name === "probeNoBinding") {
          assert.isTrue(Option.isNone(getReferenceBinding(context, node.arguments[0])));
          return;
        }
        if (node.callee.name === "probeStableBinding") {
          const binding = Option.getOrThrow(getReferenceBinding(context, node.arguments[0]));
          assert.isTrue(binding.moduleLifetime);
          assert.equal(binding.initializer?.type, "Identifier");
          return;
        }
        const origin = resolveReferenceOrigin(context, node.arguments[0]);
        if (node.callee.name === "probeNone") {
          assert.isTrue(Option.isNone(origin), context.sourceCode.getText(node.arguments[0]));
          return;
        }

        const expected: ReferenceOrigin | undefined = (
          {
            probeDefault: { kind: "module", source: "node:process", path: ["arch"] },
            probeGlobal: { kind: "global", name: "process", path: ["platform"] },
            probeImport: { kind: "module", source: "effect/Effect", path: ["runPromise"] },
            probeRequire: { kind: "module", source: "effect/Effect", path: ["runSync"] },
          } satisfies Record<string, ReferenceOrigin>
        )[node.callee.name];
        assert.deepEqual(Option.getOrThrow(origin), expected);
      },
    };
  },
});

runOxlintRuleTests(
  "origin-probe",
  originProbeRule,
  {
    valid: [
      {
        name: "resolves parser-backed global import and destructured require origins",
        code: `
          import processDefault from "node:process";
          import { runPromise as run } from "effect/Effect";
          const { ["runSync"]: sync = fallback } = require("effect/Effect");
          let stable = process;
          probeGlobal(process.platform);
          probeDefault(processDefault.arch);
          probeImport(run);
          probeRequire(sync);
          probeNoBinding(process);
          probeStableBinding((stable satisfies typeof stable)!);
        `,
      },
      {
        name: "rejects unstable or unknowable parser-backed provenance",
        code: `
          const local = { runPromise() {} };
          const key = "runPromise";
          const { [key]: dynamic, ...rest } = require("effect/Effect");
          const [first] = require("effect/Effect");
          const fromDynamicModule = require(moduleName);
          const firstAlias = secondAlias;
          const secondAlias = firstAlias;
          probeNone(local.runPromise);
          probeNone(dynamic);
          probeNone(rest.runPromise);
          probeNone(first);
          probeNone(fromDynamicModule.runPromise);
          probeNone(firstAlias.runPromise);
        `,
      },
      {
        name: "resolves initial provenance per reference site around writes and closure calls",
        code: `
          const local = { platform: "browser" };

          let direct = process;
          probeGlobal(direct.platform);
          direct = local;
          probeNone(direct.platform);

          let branch = process;
          if (condition) branch = local;
          probeNone(branch.platform);

          let uninvoked = process;
          const replaceUninvoked = () => { uninvoked = local; };
          probeGlobal(uninvoked.platform);

          let called = process;
          const replaceCalled = () => { called = local; };
          replaceCalled();
          probeNone(called.platform);

          void replaceUninvoked;
        `,
      },
    ],
    invalid: [],
  },
  { languageOptions: { sourceType: "module", parserOptions: { lang: "ts" } } },
);

describe("Oxlint harness utilities", () => {
  it("selects both supported Oxlint invocation forms", () => {
    assert.deepEqual(
      resolveOxlintInvocation(true, "node", "oxlint", "oxlint.js", "config", "source"),
      {
        command: "node",
        args: ["oxlint.js", "--config", "config", "source"],
      },
    );
    assert.deepEqual(
      resolveOxlintInvocation(false, "node", "oxlint", "oxlint.js", "config", "source"),
      {
        command: "oxlint",
        args: ["--config", "config", "source"],
      },
    );
  });

  it.effect("preserves foreign failures", () =>
    expectOxlintRuleFailure(Effect.fail("foreign"), "rule").pipe(
      Effect.flip,
      Effect.tap((error) => Effect.sync(() => assert.equal(error, "foreign"))),
    ),
  );

  it.effect("describes a fixture that unexpectedly passes", () =>
    expectOxlintRuleFailure(Effect.succeed(""), "rule").pipe(
      Effect.flip,
      Effect.tap((error) =>
        Effect.sync(() => {
          assert.match(String(error), /Expected oxlint to report a failure for rule rule/u);
        }),
      ),
    ),
  );

  it("constructs harnesses for names without plugin separators", () => {
    const harness = createOxlintRuleHarness("bare-rule");
    assert.isFunction(harness.run);
  });

  it.effect("surfaces the real harness unexpected-success path", () =>
    createOxlintRuleHarness("coddy/namespace-node-imports")
      .runAndExpectFailure("const value = 1;")
      .pipe(
        Effect.flip,
        Effect.tap((error) =>
          Effect.sync(() => {
            assert.match(String(error), /Expected oxlint to report a failure/u);
          }),
        ),
        Effect.provide(NodeServices.layer),
      ),
  );
});
