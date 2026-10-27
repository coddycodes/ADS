import { assert, describe } from "@effect/vitest";

import { createOxlintRuleHarness, runOxlintRuleTests } from "../test/utils.ts";
import namespaceNodeImports from "./namespace-node-imports.ts";

const rule = createOxlintRuleHarness("coddy/namespace-node-imports");

describe("coddy/namespace-node-imports", () => {
  rule.valid(
    "allows canonical Node namespaces",
    `
      import * as NodeFS from "node:fs";
      import * as NodeFSP from "node:fs/promises";
      import * as NodeAssert from "node:assert/strict";
      import * as NodeChildProcess from "node:child_process";
      import * as NodeTimersPromises from "node:timers/promises";
      import type * as NodeStream from "node:stream";

      NodeAssert.ok(NodeChildProcess.spawn && NodeTimersPromises.setTimeout);
      export const read = NodeFS.readFileSync;
      export const readAsync = NodeFSP.readFile;
      export type Input = NodeStream.Readable;
    `,
  );

  rule.valid(
    "does not apply to non-Node packages",
    `
      import { BrowserWindow } from "electron";
    `,
  );

  rule.invalid(
    "reports named imports",
    `
      import { readFile } from "node:fs/promises";
    `,
    (output) => {
      assert.match(output, /namespace named NodeFSP/);
    },
  );

  rule.invalid(
    "reports default imports",
    `
      import path from "node:path";
    `,
    (output) => {
      assert.match(output, /namespace named NodePath/);
    },
  );

  rule.invalid(
    "reports non-canonical namespace aliases",
    `
      import * as Crypto from "node:crypto";
      import * as NodeOs from "node:os";
    `,
    (output) => {
      assert.match(output, /namespace named NodeCrypto/);
      assert.match(output, /namespace named NodeOS/);
    },
  );
});

runOxlintRuleTests("namespace-node-imports", namespaceNodeImports, {
  valid: [
    {
      name: "accepts aliases for acronym and segmented built-ins",
      code: `
        import * as NodeFS from "node:fs";
        import * as NodeFSP from "node:fs/promises";
        import * as NodeOS from "node:os";
        import * as NodeURL from "node:url";
        import * as NodeVM from "node:vm";
        import * as NodeAssert from "node:assert/strict";
        import * as NodeTestReporters from "node:test/reporters";
      `,
    },
    {
      name: "accepts non-Node imports",
      code: `
        import value from "package";
        import { readFile } from "fs";
        void value;
      `,
    },
    {
      name: "accepts comments and strings that resemble imports",
      code: `
        // import path from "node:path";
        const source = 'import path from "node:path"';
        void source;
      `,
      languageOptions: { sourceType: "script" },
    },
    {
      name: "leaves re-exports and dynamic imports outside the static import policy",
      code: `
        export { readFile } from "node:fs";
        export * from "node:path";
        export const load = () => import("node:os");
      `,
    },
    {
      name: "leaves CommonJS requires outside the static import policy",
      filename: "fixture.cjs",
      languageOptions: { sourceType: "commonjs", parserOptions: { lang: "js" } },
      code: `
        const fs = require("node:fs");
        const { join } = require("node:path");
        void [fs, join];
      `,
    },
  ],
  invalid: [
    {
      name: "rejects side-effect Node imports",
      code: `import "node:test";`,
      errors: [{ message: /namespace named NodeTest/u }],
    },
    {
      name: "rejects named, default, mixed, and noncanonical namespace imports",
      code: `
        import { readFile } from "node:fs";
        import Path from "node:path";
        import NodeCrypto, * as Crypto from "node:crypto";
        import * as NodeOs from "node:os";
      `,
      errors: [
        /namespace named NodeFS/u,
        /namespace named NodePath/u,
        /namespace named NodeCrypto/u,
        /namespace named NodeOS/u,
      ],
    },
    {
      name: "rejects type-only named imports",
      code: `import type { Readable } from "node:stream";`,
      errors: [{ message: /namespace named NodeStream/u }],
      filename: "fixture.ts",
    },
  ],
});
