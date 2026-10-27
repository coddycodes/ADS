import { assert, describe, it } from "@effect/vitest";

import { createOxlintRuleHarness, runOxlintRuleTests } from "../test/utils.ts";
import noGlobalProcessRuntime, { toRepoPath } from "./no-global-process-runtime.ts";

const rule = createOxlintRuleHarness("coddy/no-global-process-runtime");

describe("coddy/no-global-process-runtime", () => {
  it("preserves normalized filenames outside the repository root", () => {
    assert.equal(toRepoPath("Z:\\external\\fixture.ts", "X:\\repo"), "Z:/external/fixture.ts");
  });

  rule.valid(
    "allows injected host process references",
    `
      import { HostProcessPlatform } from "@coddy/shared/hostProcess";
      import * as Effect from "effect/Effect";

      export const isWindows = Effect.map(HostProcessPlatform, (platform) => platform === "win32");
    `,
  );

  rule.valid(
    "allows unrelated process members",
    `
      process.exitCode = 1;
      const nodeEnv = process.env.NODE_ENV;
    `,
  );

  rule.valid(
    "allows unrelated node os imports",
    `
      import { tmpdir } from "node:os";

      export const tempDirectory = tmpdir();
    `,
  );

  rule.invalid(
    "reports direct platform reads",
    `
      export const isWindows = process.platform === "win32";
    `,
    (output) => {
      assert.match(output, /Use HostProcessPlatform/);
    },
  );

  rule.invalid(
    "reports direct architecture reads",
    `
      export const isArm = process.arch === "arm64";
    `,
    (output) => {
      assert.match(output, /Use HostProcessArchitecture/);
    },
  );

  rule.invalid(
    "reports globalThis process platform reads",
    `
      export const terminalName = globalThis.process.platform === "win32" ? "xterm-color" : "xterm-256color";
    `,
  );

  rule.invalid(
    "reports node os namespace platform reads",
    `
      import * as NodeOS from "node:os";

      export const isWindows = NodeOS.platform() === "win32";
    `,
    (output) => {
      assert.match(output, /Use HostProcessPlatform/);
    },
  );

  rule.invalid(
    "reports renamed node os architecture imports",
    `
      import { arch as hostArch } from "node:os";

      export const isArm = hostArch() === "arm64";
    `,
    (output) => {
      assert.match(output, /Use HostProcessArchitecture/);
    },
  );

  rule.invalid(
    "reports default node os platform reads",
    `
      import os from "node:os";

      export const isWindows = os.platform() === "win32";
    `,
  );
});

const repoRoot = import.meta.dirname.replace(/[/\\]oxlint-plugin-coddy-studio[/\\]rules$/u, "");

runOxlintRuleTests(
  "no-global-process-runtime",
  noGlobalProcessRuntime,
  {
    valid: [
      {
        name: "allows the dedicated host process reference module",
        filename: `${repoRoot}/packages/shared/src/hostProcess.ts`,
        code: `
          import * as NodeOS from "node:os";
          export const platform = process.platform;
          export const architecture = globalThis.process.arch;
          export const osPlatform = NodeOS.platform();
        `,
      },
      {
        name: "allows shadowed global and imported binding names",
        code: `
          import * as NodeOS from "node:os";
          import { platform as osPlatform } from "node:os";

          export function inspect(
            process: { platform: string },
            globalThis: { process: { arch: string } },
            NodeOS: { platform(): string },
            osPlatform: () => string,
          ) {
            return [process.platform, globalThis.process.arch, NodeOS.platform(), osPlatform()];
          }
        `,
      },
      {
        name: "allows unrelated properties and mutable or shadowed aliases",
        code: `
          const config = { process: { platform: "browser" } };
          const localConfig = { platform: "browser" };
          const { platform: configuredPlatform } = localConfig;
          let host = process;
          host = config.process;
          function inspect(process: { platform: string }) {
            const local = process;
            return local.platform;
          }
          const os = { platform: () => "browser" };
          os.platform();
          void [config.process.platform, configuredPlatform, host.platform, inspect];
        `,
      },
      {
        name: "allows aliases read only after direct or invoked closure writes",
        code: `
          import * as NodeOS from "node:os";
          const localProcess = { platform: "browser", arch: "wasm" };
          const localOS = { platform: () => "browser" };
          let assigned = process;
          assigned = localProcess;
          let destructured = NodeOS;
          ({ destructured } = { destructured: localOS });
          let updated: unknown = process;
          updated++;
          let closureWritten = process;
          const replace = () => { closureWritten = localProcess; };
          replace();
          assigned.platform;
          destructured.platform();
          (updated as typeof localProcess).arch;
          closureWritten.arch;
        `,
      },
      {
        name: "allows reads after invoked function declarations named expressions and IIFEs",
        code: `
          const localProcess = { platform: "browser", arch: "wasm" };

          let declarationWritten = process;
          function replaceDeclaration() { declarationWritten = localProcess; }
          replaceDeclaration();
          declarationWritten.platform;

          let namedExpressionWritten = process;
          const replaceNamed = function replaceInner() {
            namedExpressionWritten = localProcess;
          };
          replaceNamed();
          namedExpressionWritten.arch;

          let immediatelyWritten = process;
          (() => { immediatelyWritten = localProcess; })();
          immediatelyWritten.platform;

          let functionImmediatelyWritten = process;
          (function () { functionImmediatelyWritten = localProcess; })();
          functionImmediatelyWritten.arch;

          let wrappedImmediatelyWritten = process;
          (((() => { wrappedImmediatelyWritten = localProcess; }) satisfies () => void)! as () => void)();
          wrappedImmediatelyWritten.arch;

          let staticBlockWritten = process;
          class ReplaceInStaticBlock {
            static { staticBlockWritten = localProcess; }
          }
          staticBlockWritten.platform;
          void ReplaceInStaticBlock;
        `,
      },
      {
        name: "allows reads after provable aliased object and nested immediate writer calls",
        code: `
          const localProcess = { platform: "browser", arch: "wasm" };

          let aliasedWritten = process;
          const writer = () => { aliasedWritten = localProcess; };
          const writerAlias = writer;
          writerAlias();
          aliasedWritten.platform;

          let objectWritten = process;
          const writers = {
            replace() { objectWritten = localProcess; },
          };
          const writersAlias = writers;
          void writersAlias.replace;
          writersAlias.replace();
          objectWritten.arch;

          let computedObjectWritten = process;
          const computedWriters = {
            ["replace"]() { computedObjectWritten = localProcess; },
          };
          computedWriters["replace"]();
          computedObjectWritten.platform;

          let nestedIifeWritten = process;
          const nestedIifeWriter = () => { nestedIifeWritten = localProcess; };
          (() => { nestedIifeWriter(); })();
          nestedIifeWritten.platform;

          let staticCallWritten = process;
          const staticWriter = () => { staticCallWritten = localProcess; };
          class InvokeWriter {
            static { staticWriter(); }
          }
          staticCallWritten.arch;
          void InvokeWriter;

          let recursiveWritten = process;
          const recursiveWriter = () => {
            if (condition) recursiveWriter();
            recursiveWritten = localProcess;
          };
          recursiveWriter();
          recursiveWritten.platform;
        `,
      },
      {
        name: "allows unrelated nested destructuring and shadowed globalThis",
        code: `
          const {
            config: { platform = "browser" },
            argv: [, arch],
            versions: { ...environment },
          } = process;
          const key = "platform";
          const { [key]: dynamicPlatform } = process;
          const { env } = process;
          let uninitialized;
          function inspect(globalThis: { process: { platform: string } }) {
            const { process: { platform: localPlatform } } = globalThis;
            return localPlatform;
          }
          void [platform, arch, environment, dynamicPlatform, env, uninitialized, inspect];
        `,
      },
      {
        name: "allows type-only node process imports",
        code: `
          import type { platform as PlatformType } from "node:process";
          export type HostPlatform = typeof PlatformType;
        `,
      },
      {
        name: "allows unrelated named node process imports",
        code: `
          import { env } from "node:process";
          void env;
        `,
      },
      {
        name: "allows unrelated and dynamic process or os properties",
        code: `
          import * as NodeOS from "node:os";
          import { tmpdir } from "node:os";
          import * as Other from "other";
          const key = "platform";
          process.exitCode = 1;
          process.env.NODE_ENV;
          process[key];
          process[0];
          NodeOS.tmpdir();
          NodeOS[key]();
          tmpdir();
          Other.platform();
          getOs().platform();
          (() => "win32")();
          const { platform: configuredPlatform } = config;
          const { cwd, 0: first } = process;
          void [configuredPlatform, cwd, first];
        `,
      },
      {
        name: "allows filenames outside the configured repository root",
        filename: "../external/fixture.ts",
        code: `const platform = "win32"; void platform;`,
      },
      {
        name: "allows private members that share runtime property names",
        code: `
          class RuntimeInfo {
            #platform = "win32";
            read() { return this.#platform; }
          }
          void RuntimeInfo;
        `,
      },
      {
        name: "ignores comments and strings",
        code: `
          // process.platform
          const source = "globalThis.process.arch and NodeOS.platform()";
          void source;
        `,
        languageOptions: { sourceType: "script" },
      },
    ],
    invalid: [
      {
        name: "reports direct, wrapped, computed, and optional global reads",
        code: `
          process.platform;
          (process as typeof process)["arch"];
          globalThis.process.platform;
          globalThis["process"]?.["arch"];
        `,
        errors: [
          /Use HostProcessPlatform/u,
          /Use HostProcessArchitecture/u,
          /Use HostProcessPlatform/u,
          /Use HostProcessArchitecture/u,
        ],
      },
      {
        name: "reports namespace, default, named, aliased, and computed node os calls",
        code: `
          import * as NodeOS from "node:os";
          import os from "os";
          import { arch, platform as hostPlatform } from "node:os";
          NodeOS.platform();
          os["arch"]();
          arch();
          hostPlatform?.();
        `,
        errors: 4,
      },
      {
        name: "reports globals in script source files",
        filename: "fixture.cjs",
        languageOptions: { sourceType: "script", parserOptions: { lang: "js" } },
        code: `process.arch;`,
        errors: [{ message: /Use HostProcessArchitecture/u }],
      },
      {
        name: "reports destructured process runtime reads",
        code: `
          const { platform, arch: architecture } = process;
          const { platform: globalPlatform } = globalThis.process;
          void [platform, architecture, globalPlatform];
        `,
        errors: [
          /Use HostProcessPlatform/u,
          /Use HostProcessArchitecture/u,
          /Use HostProcessPlatform/u,
        ],
      },
      {
        name: "reports immutable aliases and destructured globalThis process reads",
        code: `
          const host = process;
          const nested = host;
          const { process: globalProcess } = globalThis;
          const { platform: globalPlatform, arch: globalArch } = globalProcess;
          host.platform;
          nested["arch"];
          void [globalPlatform, globalArch];
        `,
        errors: 4,
      },
      {
        name: "reports node process default namespace named and aliased forms",
        code: `
          import processDefault from "node:process";
          import * as NodeProcess from "node:process";
          import { platform, arch as architecture } from "node:process";
          const host = processDefault;
          host.platform;
          NodeProcess["arch"];
          void [platform, architecture];
        `,
        errors: 4,
      },
      {
        name: "reports CommonJS process and os destructuring with safe aliases",
        filename: "fixture.cts",
        languageOptions: { sourceType: "commonjs", parserOptions: { lang: "ts" } },
        code: `
          const NodeProcess = require("node:process");
          const { arch: processArch } = require("node:process");
          const { platform: osPlatform } = require("node:os");
          const os = require("os");
          const readPlatform = osPlatform;
          NodeProcess.platform;
          void processArch;
          readPlatform();
          os.arch();
        `,
        errors: 4,
      },
      {
        name: "reports satisfies and nested TypeScript wrapper combinations",
        code: `
          (process satisfies typeof process).platform;
          (((globalThis satisfies typeof globalThis).process as typeof process)!).arch;
          ((globalThis satisfies typeof globalThis).process as typeof process)?.platform;
        `,
        errors: 3,
      },
      {
        name: "reports stable let aliases in module function and closure scopes",
        code: `
          let host = process;
          host.platform;
          function nested() {
            let scoped = globalThis.process;
            return scoped.arch;
          }
          const closure = () => {
            let captured = process;
            return () => captured.platform;
          };
          void [nested, closure];
        `,
        errors: 3,
      },
      {
        name: "reports initial provenance only at reads before syntactically preceding writes",
        code: `
          const localProcess = { platform: "browser", arch: "wasm" };

          let assigned = process;
          assigned.platform;
          assigned = localProcess;
          assigned.arch;

          let updated: unknown = process;
          (updated as typeof process).arch;
          updated++;
          (updated as typeof process).platform;

          let destructured = process;
          destructured.arch;
          ({ destructured } = { destructured: localProcess });
          destructured.platform;

          let branchBefore = process;
          if (condition) branchBefore = localProcess;
          branchBefore.platform;

          let branchAfter = process;
          branchAfter.arch;
          if (condition) branchAfter = localProcess;

          let uninvokedClosure = process;
          const replaceUninvoked = () => { uninvokedClosure = localProcess; };
          uninvokedClosure.platform;

          let calledBefore = process;
          const replaceBefore = () => { calledBefore = localProcess; };
          replaceBefore();
          calledBefore.arch;

          let calledAfter = process;
          const replaceAfter = () => { calledAfter = localProcess; };
          calledAfter.platform;
          replaceAfter();

          void [replaceUninvoked, replaceAfter];
        `,
        errors: 6,
      },
      {
        name: "does not invent execution from writer references callbacks or unresolved aliases",
        code: `
          const localProcess = { platform: "browser", arch: "wasm" };

          let referenced = process;
          const referencedWriter = () => { referenced = localProcess; };
          void referencedWriter;
          register(referencedWriter);
          const storedWriter = referencedWriter;
          function returnWriter() { return referencedWriter; }
          Promise.resolve().then(referencedWriter);
          setTimeout(referencedWriter, 0);
          referenced.platform;

          let anonymousCallback = process;
          Promise.resolve().then(() => { anonymousCallback = localProcess; });
          anonymousCallback.arch;

          let namedCallback = process;
          register(function callback() { namedCallback = localProcess; });
          namedCallback.platform;

          let shadowed = process;
          const shadowedWriter = () => { shadowed = localProcess; };
          {
            const shadowedWriter = () => undefined;
            shadowedWriter();
          }
          shadowed.arch;

          let mutableAlias = process;
          const originalWriter = () => { mutableAlias = localProcess; };
          let mutableWriter = originalWriter;
          mutableWriter = () => undefined;
          mutableWriter();
          mutableAlias.platform;

          let nestedStatic = process;
          function declareClass() {
            class DeferredClass { static { nestedStatic = localProcess; } }
            return DeferredClass;
          }
          nestedStatic.arch;

          let changedMethod = process;
          const changedWriters = {
            replace() { changedMethod = localProcess; },
          };
          changedWriters.replace = () => undefined;
          changedWriters.replace();
          changedMethod.platform;

          let varAliased = process;
          const varWriter = () => { varAliased = localProcess; };
          var varWriterAlias = varWriter;
          varWriterAlias();
          varAliased.arch;

          let dynamicMethod = process;
          const methodName = "replace";
          const dynamicWriters = {
            [methodName]() { dynamicMethod = localProcess; },
          };
          dynamicWriters[methodName]();
          dynamicMethod.platform;

          let aliasMutatedMethod = process;
          const aliasMutationWriters = {
            replace() { aliasMutatedMethod = localProcess; },
          };
          const aliasMutation = aliasMutationWriters;
          aliasMutation.replace = () => undefined;
          aliasMutationWriters.replace();
          aliasMutatedMethod.arch;

          let updatedMethod = process;
          const updateWriters = { replace() { updatedMethod = localProcess; } };
          updateWriters.replace++;
          updateWriters.replace();
          updatedMethod.platform;

          let deletedMethod = process;
          const deleteWriters = { replace() { deletedMethod = localProcess; } };
          delete deleteWriters.replace;
          deleteWriters.replace();
          deletedMethod.arch;

          let instanceField = process;
          class DeferredInstance {
            field = (instanceField = localProcess);
          }
          instanceField.platform;

          void [storedWriter, returnWriter, shadowedWriter, declareClass, DeferredInstance];
        `,
        errors: 13,
      },
      {
        name: "keeps provenance across uninvoked unbound function bodies",
        code: `
          const localProcess = { platform: "browser", arch: "wasm" };
          let objectMethodWritten = process;
          const holder = {
            replace() { objectMethodWritten = localProcess; },
          };
          objectMethodWritten.platform;
          holder.replace();

          let anonymousDeclarationWritten = process;
          export default function () { anonymousDeclarationWritten = localProcess; }
          anonymousDeclarationWritten.arch;
          void holder;
        `,
        errors: 2,
      },
      {
        name: "reports recursive default array and precisely subtracted rest process destructuring",
        code: `
          const {
            process: {
              platform: hostPlatform = "browser",
              env: [firstEnvironment] = [],
              config: { arch: unrelatedArchitecture },
              ...globalSnapshot
            }
          } = globalThis;
          const {
            arch: [firstArchitecture],
            env: { PATH = "" },
            ...processSnapshot
          } = process;
          void [
            hostPlatform,
            firstEnvironment,
            unrelatedArchitecture,
            globalSnapshot,
            firstArchitecture,
            PATH,
            processSnapshot,
          ];
        `,
        errors: 4,
      },
      {
        name: "subtracts explicit platform and architecture keys from object rest diagnostics",
        code: `
          const { platform, arch, ...withoutRuntime } = process;
          const { platform: hostPlatform, ...architectureOnly } = process;
          const { arch: hostArchitecture, ...platformOnly } = globalThis.process;
          void [
            platform,
            arch,
            withoutRuntime,
            hostPlatform,
            architectureOnly,
            hostArchitecture,
            platformOnly,
          ];
        `,
        errors: 6,
      },
      {
        name: "reports only documented static computed process destructuring keys",
        code: `
          const { ["platform"]: platform, ["env"]: environment } = process;
          void [platform, environment];
        `,
        errors: 1,
      },
    ],
  },
  {
    cwd: repoRoot,
    languageOptions: { sourceType: "module", parserOptions: { lang: "ts" } },
  },
);
