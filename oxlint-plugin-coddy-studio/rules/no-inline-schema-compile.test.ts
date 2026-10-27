import { assert, describe } from "@effect/vitest";

import { createOxlintRuleHarness, runOxlintRuleTests } from "../test/utils.ts";
import noInlineSchemaCompile from "./no-inline-schema-compile.ts";

const rule = createOxlintRuleHarness("coddy/no-inline-schema-compile");

describe("coddy/no-inline-schema-compile", () => {
  rule.valid(
    "allows schema compilers hoisted to module scope",
    `
      import { Schema } from "effect";

      const User = Schema.Struct({ name: Schema.String });
      const decodeUser = Schema.decodeUnknownEffect(User);

      export const parseUser = (input: unknown) => decodeUser(input);
    `,
  );

  rule.valid(
    "allows factory helpers that return a precompiled decoder",
    `
      import { Schema } from "effect";

      export const makeParser = <A, I>(schema: Schema.Codec<A, I>) => {
        const decode = Schema.decodeUnknownEffect(schema);
        return (input: unknown) => decode(input);
      };
    `,
  );

  rule.valid(
    "allows schema construction helpers that use encode transformations",
    `
      import { Schema } from "effect";

      export const makePrettyJson = <S extends Schema.Top>(schema: S) =>
        Schema.fromJsonString(schema).pipe(
          Schema.encode({
            decode: Schema.String,
            encode: Schema.String,
          }),
        );
    `,
  );

  rule.valid(
    "allows dynamic schema parameters that cannot be hoisted to module scope",
    `
      import { Schema } from "effect";

      export const parseWith = <A, I>(schema: Schema.Codec<A, I>, input: unknown) =>
        Schema.decodeUnknownEffect(schema)(input);
    `,
  );

  rule.invalid(
    "reports schema compilers inside function bodies",
    `
      import { Schema } from "effect";

      const User = Schema.Struct({ name: Schema.String });

      export const parseUser = (input: unknown) => Schema.decodeUnknownEffect(User)(input);
    `,
    (output) => {
      assert.match(output, /Hoist Schema\.decodeUnknownEffect/);
    },
  );

  rule.invalid(
    "reports inline schema literals as high confidence findings",
    `
      import { Schema } from "effect";

      export const parseUser = (input: unknown) =>
        Schema.decodeUnknownEffect(Schema.Struct({ name: Schema.String }))(input);
    `,
    (output) => {
      assert.match(output, /inline schema literal and the compiled function/);
    },
  );
});

runOxlintRuleTests(
  "no-inline-schema-compile",
  noInlineSchemaCompile,
  {
    valid: [
      {
        name: "allows compilers created at module scope",
        code: `
          import * as Schema from "effect/Schema";
          const User = Schema.Struct({ name: Schema.String });
          const decode = Schema.decodeUnknownEffect(User);
          decode({ name: "Ada" });
        `,
      },
      {
        name: "allows dynamic schemas and non-immediate compiler factories",
        code: `
          import * as Schema from "effect/Schema";
          export const compile = <A, I>(schema: Schema.Codec<A, I>) =>
            Schema.decodeUnknownEffect(schema);
          export const parse = <A, I>(schema: Schema.Codec<A, I>, input: unknown) =>
            Schema.decodeUnknownEffect(schema)(input);
        `,
      },
      {
        name: "allows unrelated or shadowed Schema objects",
        code: `
          import * as Schema from "effect/Schema";
          import { String as StringSchema } from "effect/Schema";
          import * as EffectRoot from "effect";
          import { Effect } from "effect";
          import * as Other from "other";
          const User = Schema.String;
          function parse(Schema: { decodeSync(value: unknown): (input: unknown) => unknown }) {
            return Schema.decodeSync(User)("input");
          }
          const codec = OtherSchema.decodeSync(User)(input);
          const Local = { decodeSync: (_schema: unknown) => (_input: unknown) => "ok" };
          const local = (input: unknown) => Local.decodeSync(User)(input);
          const Factory = makeSchemaModule();
          const factoryModule = (input: unknown) => Factory.decodeSync(User)(input);
          const dynamic = (input: unknown) => Schema.decodeSync(Other.Struct())(input);
          const factory = (input: unknown) => Schema.decodeSync(makeSchema())(input);
          const missing = (input: unknown) => Schema.decodeSync()(input);
          function withShadowedRequire(require: (source: string) => typeof Schema, input: unknown) {
            const Required = require("effect/Schema");
            return Required.decodeSync(User)(input);
          }
          void [
            parse,
            codec,
            local,
            factoryModule,
            dynamic,
            factory,
            missing,
            withShadowedRequire,
            StringSchema,
            EffectRoot,
            Effect,
            Other,
          ];
        `,
      },
      {
        name: "allows mutable aliases because their provenance is not stable",
        code: `
          import * as Schema from "effect/Schema";
          const User = Schema.String;
          let compile = Schema.decodeSync;
          compile = (_schema: unknown) => (_input: unknown) => "local";
          function parse(input: unknown) { return compile(User)(input); }
          void parse;
        `,
      },
      {
        name: "allows uppercase parameters local dynamic members and written module bindings",
        code: `
          import * as Schema from "effect/Schema";
          var VarSchema = Schema.String;
          let UninitializedSchema: Schema.Top;
          let MutableSchema = Schema.String;
          MutableSchema = makeSchema();
          function parameter(InputSchema: Schema.Top, input: unknown) {
            return Schema.decodeSync(InputSchema)(input);
          }
          function member(input: { Schema: Schema.Top }, value: unknown) {
            return Schema.decodeSync(input.Schema)(value);
          }
          function mutable(input: unknown) {
            return Schema.decodeSync(MutableSchema)(input);
          }
          function mutableDeclarations(input: unknown) {
            Schema.decodeSync(VarSchema)(input);
            Schema.decodeSync(UninitializedSchema)(input);
          }
          function literal(input: unknown) {
            return Schema.decodeSync("not-a-schema")(input);
          }
          void [parameter, member, mutable, mutableDeclarations, literal];
        `,
      },
      {
        name: "allows compilers after provable aliased object and nested immediate writer calls",
        code: `
          import * as Schema from "effect/Schema";
          const User = Schema.String;
          const localCompiler = (_schema: unknown) => (_input: unknown) => "local";

          function aliased(input: unknown) {
            let compile = Schema.decodeSync;
            const writer = () => { compile = localCompiler; };
            const writerAlias = writer;
            writerAlias();
            return compile(User)(input);
          }

          function objectMethod(input: unknown) {
            let compile = Schema.decodeSync;
            const writers = { replace() { compile = localCompiler; } };
            const writersAlias = writers;
            writersAlias.replace();
            return compile(User)(input);
          }

          function nestedIife(input: unknown) {
            let compile = Schema.decodeSync;
            const writer = () => { compile = localCompiler; };
            (() => { writer(); })();
            return compile(User)(input);
          }

          function staticBlock(input: unknown) {
            let compile = Schema.decodeSync;
            const writer = () => { compile = localCompiler; };
            class InvokeWriter { static { writer(); } }
            void InvokeWriter;
            return compile(User)(input);
          }

          void [aliased, objectMethod, nestedIife, staticBlock];
        `,
      },
      {
        name: "ignores comments and strings",
        code: `
          const source = "Schema.decodeSync(User)(input)";
          // Schema.decodeSync(User)(input)
          void source;
        `,
        languageOptions: { sourceType: "script", parserOptions: { lang: "js" } },
      },
    ],
    invalid: [
      {
        name: "reports namespace and named Schema import aliases",
        code: `
          import * as S from "effect/Schema";
          import { Schema as EffectSchema } from "effect";
          const User = S.Struct({ name: S.String });
          function first(input: unknown) { return S.decodeSync(User)(input); }
          const second = (input: unknown) => EffectSchema["encodeSync"](User)(input);
        `,
        errors: [/Hoist Schema\.decodeSync/u, /Hoist Schema\.encodeSync/u],
      },
      {
        name: "reports function declarations, expressions, arrows, and nested functions",
        code: `
          import * as Schema from "effect/Schema";
          const User = Schema.String;
          function declaration(input: unknown) { return Schema.decodeSync(User)(input); }
          const expression = function(input: unknown) { return Schema.decodeOption(User)(input); };
          const arrow = (input: unknown) => Schema.encodeEffect(User)(input);
          function outer() {
            return function inner(input: unknown) { return Schema.decodeResult(User)(input); };
          }
          void [declaration, expression, arrow, outer];
        `,
        errors: 4,
      },
      {
        name: "reports static member references and nested schema literals with confidence levels",
        code: `
          import * as Schema from "effect/Schema";
          const Namespace = { User: Schema.String };
          const low = (input: unknown) => Schema.decodeUnknownSync(Namespace.User)(input);
          const high = (input: unknown) =>
            Schema.decodeUnknownSync(Schema.fromJsonString(Schema.Struct({ value: Schema.String })))(input);
          void [low, high];
        `,
        errors: [
          /compiled function is rebuilt/u,
          /inline schema literal and the compiled function/u,
        ],
      },
      {
        name: "reports optional and TypeScript-wrapped compiler invocations",
        code: `
          import * as Schema from "effect/Schema";
          const User = Schema.String;
          const parse = (input: unknown) =>
            (Schema.decodeUnknownEffect!)(User)?.(input);
          void parse;
        `,
        errors: 1,
      },
      {
        name: "reports CommonJS Schema aliases",
        filename: "fixture.cts",
        languageOptions: { sourceType: "commonjs", parserOptions: { lang: "ts" } },
        code: `
          const S = require("effect/Schema");
          const User = S.String;
          function parse(input: unknown) { return S.decodeSync(User)(input); }
          void parse;
        `,
        errors: [{ message: /Hoist Schema\.decodeSync/u }],
      },
      {
        name: "reports compiler functions assigned inside functions",
        code: `
          import * as Schema from "effect/Schema";
          const User = Schema.String;
          function parse(input: unknown) {
            const decode = Schema.decodeSync(User);
            return decode(input);
          }
          void parse;
        `,
        errors: [{ message: /Hoist Schema\.decodeSync/u }],
      },
      {
        name: "reports named compilers root namespaces and safe aliases",
        code: `
          import { decodeSync as decode } from "effect/Schema";
          import * as Root from "effect";
          const User = Root.Schema.String;
          const compile = Root.Schema.decodeUnknownSync;
          function first(input: unknown) { return decode(User)(input); }
          function second(input: unknown) { return compile(User)(input); }
          void [first, second];
        `,
        errors: [/Hoist Schema\.decodeSync/u, /Hoist Schema\.decodeUnknownSync/u],
      },
      {
        name: "reports CommonJS namespace destructured root and alias compilers",
        filename: "fixture.cts",
        languageOptions: { sourceType: "commonjs", parserOptions: { lang: "ts" } },
        code: `
          const Schema = require("effect/Schema");
          const { encodeSync } = require("effect/Schema");
          const { Schema: RootSchema } = require("effect");
          const decode = RootSchema.decodeSync;
          const User = Schema.String;
          function first(input: unknown) { return encodeSync(User)(input); }
          function second(input: unknown) { return decode(User)(input); }
          void [first, second];
        `,
        errors: [/Hoist Schema\.encodeSync/u, /Hoist Schema\.decodeSync/u],
      },
      {
        name: "reports lowercase module schemas imports aliases and stable lets",
        code: `
          import * as Schema from "effect/Schema";
          import { String as importedSchema } from "effect/Schema";
          const lowerCaseSchema = Schema.Struct({ value: Schema.String });
          const moduleAlias = lowerCaseSchema;
          let stableLetSchema = importedSchema;
          let compile = Schema.decodeSync;
          function first(input: unknown) { return Schema.decodeSync(lowerCaseSchema)(input); }
          function second(input: unknown) { return Schema.decodeSync(moduleAlias)(input); }
          function third(input: unknown) { return Schema.decodeSync(importedSchema)(input); }
          function fourth(input: unknown) { return Schema.decodeSync(stableLetSchema)(input); }
          function fifth(input: unknown) {
            const localAlias = lowerCaseSchema;
            return compile(localAlias)(input);
          }
          void [first, second, third, fourth, fifth];
        `,
        errors: 5,
      },
      {
        name: "reports satisfies as non-null optional and nested schema wrappers",
        code: `
          import * as Schema from "effect/Schema";
          const lower = Schema.String;
          function first(input: unknown) {
            return ((((Schema satisfies typeof Schema) as typeof Schema)!).decodeSync)(lower)?.(input);
          }
          function second(input: unknown) {
            return Schema.decodeSync((((lower satisfies typeof lower) as typeof lower)!))(input);
          }
          function third(input: unknown) {
            return Schema.decodeSync((Schema.Struct({ value: Schema.String }) satisfies Schema.Top)!)(input);
          }
          void [first, second, third];
        `,
        errors: 3,
      },
      {
        name: "reports compiler and schema provenance only before preceding writes",
        code: `
          import * as Schema from "effect/Schema";
          const User = Schema.String;
          const localCompiler = (_schema: unknown) => (_input: unknown) => "local";

          function assignment(input: unknown) {
            let compile = Schema.decodeSync;
            compile(User)(input);
            compile = localCompiler;
            compile(User)(input);

            let schema = Schema.String;
            Schema.decodeSync(schema)(input);
            schema = makeSchema();
            Schema.decodeSync(schema)(input);
          }

          function updates(input: unknown) {
            let compile: unknown = Schema.decodeSync;
            (compile as typeof Schema.decodeSync)(User)(input);
            compile++;
            (compile as typeof Schema.decodeSync)(User)(input);
          }

          function destructuring(input: unknown) {
            let compile = Schema.decodeSync;
            compile(User)(input);
            [compile] = [localCompiler];
            compile(User)(input);
          }

          function branches(input: unknown) {
            let before = Schema.decodeSync;
            if (condition) before = localCompiler;
            before(User)(input);

            let after = Schema.decodeSync;
            after(User)(input);
            if (condition) after = localCompiler;
          }

          function closures(input: unknown) {
            let uninvoked = Schema.decodeSync;
            const replaceUninvoked = () => { uninvoked = localCompiler; };
            uninvoked(User)(input);

            let calledBefore = Schema.decodeSync;
            const replaceBefore = () => { calledBefore = localCompiler; };
            replaceBefore();
            calledBefore(User)(input);

            let calledAfter = Schema.decodeSync;
            const replaceAfter = () => { calledAfter = localCompiler; };
            calledAfter(User)(input);
            replaceAfter();
            void replaceUninvoked;
          }

          void [assignment, updates, destructuring, branches, closures];
        `,
        errors: 7,
      },
      {
        name: "does not invent compiler writes from references callbacks or unresolved aliases",
        code: `
          import * as Schema from "effect/Schema";
          const User = Schema.String;
          const localCompiler = (_schema: unknown) => (_input: unknown) => "local";

          function referenced(input: unknown) {
            let compile = Schema.decodeSync;
            const writer = () => { compile = localCompiler; };
            void writer;
            register(writer);
            const stored = writer;
            function expose() { return writer; }
            Promise.resolve().then(writer);
            setTimeout(writer, 0);
            void [stored, expose];
            return compile(User)(input);
          }

          function anonymousCallback(input: unknown) {
            let compile = Schema.decodeSync;
            Promise.resolve().then(() => { compile = localCompiler; });
            return compile(User)(input);
          }

          function namedCallback(input: unknown) {
            let compile = Schema.decodeSync;
            register(function callback() { compile = localCompiler; });
            return compile(User)(input);
          }

          function shadowed(input: unknown) {
            let compile = Schema.decodeSync;
            const writer = () => { compile = localCompiler; };
            {
              const writer = () => undefined;
              writer();
            }
            void writer;
            return compile(User)(input);
          }

          function mutableAlias(input: unknown) {
            let compile = Schema.decodeSync;
            const writer = () => { compile = localCompiler; };
            let alias = writer;
            alias = () => undefined;
            alias();
            return compile(User)(input);
          }

          void [referenced, anonymousCallback, namedCallback, shadowed, mutableAlias];
        `,
        errors: 5,
      },
    ],
  },
  { languageOptions: { sourceType: "module", parserOptions: { lang: "ts" } } },
);
