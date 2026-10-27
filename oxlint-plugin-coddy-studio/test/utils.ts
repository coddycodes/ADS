import * as NodeServices from "@effect/platform-node/NodeServices";
import type { Rule } from "@oxlint/plugins";
import { assert, describe, it } from "@effect/vitest";
import * as Data from "effect/Data";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";
import * as PlatformError from "effect/PlatformError";
import * as Predicate from "effect/Predicate";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";
import { ChildProcess, ChildProcessSpawner } from "effect/process";
import * as NodeProcess from "node:process";
import * as NodeModule from "node:module";
import * as NodeURL from "node:url";

interface RuleTesterCase {
  readonly code: string;
  readonly name?: string;
  readonly filename?: string;
  readonly languageOptions?: {
    readonly sourceType?: "script" | "module" | "commonjs" | "unambiguous";
    readonly parserOptions?: {
      readonly lang?: "js" | "jsx" | "ts" | "tsx" | "dts";
      readonly ecmaFeatures?: { readonly jsx?: boolean };
    };
  };
}

interface InvalidRuleTesterCase extends RuleTesterCase {
  readonly errors: number | ReadonlyArray<string | RegExp | { readonly message?: string | RegExp }>;
}

interface RuleTesterCases {
  readonly valid: ReadonlyArray<string | RuleTesterCase>;
  readonly invalid: ReadonlyArray<InvalidRuleTesterCase>;
}

interface RuleTesterConfig {
  readonly cwd?: string;
  readonly languageOptions?: RuleTesterCase["languageOptions"];
}

interface RuleTesterInstance {
  readonly run: (ruleName: string, rule: Rule, tests: RuleTesterCases) => void;
}

interface RuleTesterConstructor {
  new (config?: RuleTesterConfig): RuleTesterInstance;
  describe: (name: string, fn: () => void) => void;
  it: (name: string, fn: () => void) => void;
  itOnly: (name: string, fn: () => void) => void;
}

const require = NodeModule.createRequire(import.meta.url);
const vitePlusPackagePath = require.resolve("vite-plus/package.json");
const oxlintRequire = NodeModule.createRequire(vitePlusPackagePath);
const oxlintPluginsDevPath = oxlintRequire.resolve("oxlint/plugins-dev");
const { RuleTester } = (await import(NodeURL.pathToFileURL(oxlintPluginsDevPath).href)) as {
  readonly RuleTester: RuleTesterConstructor;
};

RuleTester.describe = describe as RuleTesterConstructor["describe"];
RuleTester.it = it as RuleTesterConstructor["it"];
RuleTester.itOnly = it.only as RuleTesterConstructor["itOnly"];

export const runOxlintRuleTests = (
  ruleName: string,
  rule: Rule,
  tests: RuleTesterCases,
  config?: RuleTesterConfig,
): void => {
  new RuleTester(config).run(ruleName, rule, tests);
};

class OxlintFixtureFailure extends Data.TaggedError("OxlintFixtureFailure")<{
  readonly exitCode: number;
  readonly stdout: string;
  readonly stderr: string;
}> {
  static readonly is = (u: unknown): u is OxlintFixtureFailure =>
    Predicate.isTagged(u, "OxlintFixtureFailure");
}

class OxlintFixtureExpectedFailure extends Data.TaggedError("OxlintFixtureExpectedFailure")<{
  readonly ruleName: string;
}> {
  override get message() {
    return `Expected oxlint to report a failure for rule ${this.ruleName}, but it passed.`;
  }
}

const encodeOxlintConfig = Schema.encodeEffect(Schema.fromJsonString(Schema.Unknown));

interface RuleHarness {
  readonly run: (
    source: string,
  ) => Effect.Effect<
    string,
    OxlintFixtureFailure | PlatformError.PlatformError | Schema.SchemaError,
    NodeServices.NodeServices
  >;
  readonly runAndExpectFailure: (
    source: string,
  ) => Effect.Effect<
    string,
    | OxlintFixtureExpectedFailure
    | OxlintFixtureFailure
    | PlatformError.PlatformError
    | Schema.SchemaError,
    NodeServices.NodeServices
  >;
  readonly valid: (name: string, source: string) => void;
  readonly invalid: (name: string, source: string, assertion?: (output: string) => void) => void;
}

interface RuleHarnessOptions {
  readonly filename?: string;
}

const collectStreamAsString = <E>(stream: Stream.Stream<Uint8Array, E>): Effect.Effect<string, E> =>
  stream.pipe(
    Stream.decodeText(),
    Stream.runFold(
      () => "",
      (acc, chunk) => acc + chunk,
    ),
  );

const spawnAndCollectOutput = Effect.fnUntraced(function* (command: ChildProcess.Command) {
  const spawner = yield* ChildProcessSpawner.ChildProcessSpawner;
  const child = yield* spawner.spawn(command);

  const [stdout, stderr, exitCode] = yield* Effect.all(
    [
      collectStreamAsString(child.stdout),
      collectStreamAsString(child.stderr),
      child.exitCode.pipe(Effect.map(Number)),
    ],
    { concurrency: "unbounded" },
  );

  return { exitCode, stdout, stderr };
}, Effect.scoped);

export const resolveOxlintInvocation = (
  hasOxlintJsBin: boolean,
  nodeExecutable: string,
  oxlintBin: string,
  oxlintJsBin: string,
  configPath: string,
  sourcePath: string,
): { readonly command: string; readonly args: ReadonlyArray<string> } => ({
  command: hasOxlintJsBin ? nodeExecutable : oxlintBin,
  args: hasOxlintJsBin
    ? [oxlintJsBin, "--config", configPath, sourcePath]
    : ["--config", configPath, sourcePath],
});

export const expectOxlintRuleFailure = <E, R>(
  effect: Effect.Effect<string, E, R>,
  ruleName: string,
): Effect.Effect<string, E | OxlintFixtureExpectedFailure, R> =>
  effect.pipe(
    Effect.matchEffect({
      onFailure: (error) =>
        OxlintFixtureFailure.is(error)
          ? Effect.succeed(
              `oxlint fixture failed with exit code ${error.exitCode}\n${error.stdout}\n${error.stderr}`,
            )
          : Effect.fail(error),
      onSuccess: () => Effect.fail(new OxlintFixtureExpectedFailure({ ruleName })),
    }),
  );

export const createOxlintRuleHarness = (
  ruleName: string,
  options: RuleHarnessOptions = {},
): RuleHarness => {
  const [pluginName, shortRuleName] = ruleName.split("/");
  const diagnosticRuleName =
    pluginName && shortRuleName ? `${pluginName}\\(${shortRuleName}\\)` : ruleName;
  const test = it.layer(NodeServices.layer);

  const run: RuleHarness["run"] = Effect.fnUntraced(function* (source: string) {
    const fs = yield* FileSystem.FileSystem;
    const path = yield* Path.Path;
    const fixtureDir = yield* fs.makeTempDirectoryScoped({ prefix: "coddy-studio-oxlint-" });
    const configPath = path.join(fixtureDir, ".oxlintrc.json");
    const sourcePath = path.join(fixtureDir, options.filename ?? "fixture.ts");
    const repoRoot = path.join(import.meta.dirname, "..", "..");
    const oxlintBin = path.join(
      repoRoot,
      "node_modules",
      ".pnpm",
      "node_modules",
      ".bin",
      "oxlint",
    );
    const oxlintJsBin = path.join(
      repoRoot,
      "node_modules",
      ".pnpm",
      "node_modules",
      "oxlint",
      "bin",
      "oxlint",
    );
    const hasOxlintJsBin = yield* fs.exists(oxlintJsBin);
    const { args, command } = resolveOxlintInvocation(
      hasOxlintJsBin,
      NodeProcess.execPath,
      oxlintBin,
      oxlintJsBin,
      configPath,
      sourcePath,
    );
    const pluginPath = path.join(repoRoot, "oxlint-plugin-coddy-studio", "index.ts");

    yield* fs.writeFileString(
      configPath,
      yield* encodeOxlintConfig({
        jsPlugins: [{ name: "coddy", specifier: pluginPath }],
        rules: { [ruleName]: "error" },
      }),
    );
    yield* fs.writeFileString(sourcePath, source);

    const output = yield* spawnAndCollectOutput(
      ChildProcess.make(command, args, { cwd: repoRoot }),
    );

    if (output.exitCode !== 0) {
      return yield* new OxlintFixtureFailure({
        exitCode: output.exitCode,
        stdout: output.stdout,
        stderr: output.stderr,
      });
    }

    return `${output.stdout}${output.stderr}`;
  }, Effect.scoped);

  const runAndExpectFailure: RuleHarness["runAndExpectFailure"] = (source) =>
    expectOxlintRuleFailure(run(source), ruleName);

  return {
    run,
    runAndExpectFailure,
    valid(name, source) {
      test(name, (it) => {
        it.effect("passes", () => run(source));
      });
    },
    invalid(name, source, assertion) {
      test(name, (it) => {
        it.effect("reports the rule diagnostic", () =>
          runAndExpectFailure(source).pipe(
            Effect.tap((output) =>
              Effect.sync(() => {
                assert.match(output, new RegExp(diagnosticRuleName));
                assertion?.(output);
              }),
            ),
          ),
        );
      });
    },
  };
};
