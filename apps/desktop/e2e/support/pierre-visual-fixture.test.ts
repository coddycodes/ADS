// @effect-diagnostics nodeBuiltinImport:off - Desktop UI fixture tests inspect disposable host files.
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";

import { afterEach, describe, expect, it } from "vite-plus/test";

import {
  archiveAndCleanupDesktopUiTestContext,
  prepareDesktopUiTestContext,
  type DesktopUiTestContext,
} from "./test-project.ts";
import { pierreVisualFixture, preparePierreVisualFixture } from "./pierre-visual-fixture.ts";

const contexts: DesktopUiTestContext[] = [];

function git(projectPath: string, args: ReadonlyArray<string>): string {
  const result = NodeChildProcess.spawnSync("git", ["-C", projectPath, ...args], {
    encoding: "utf8",
    shell: false,
  });
  expect(result.error).toBeUndefined();
  expect(result.status, result.stderr).toBe(0);
  return result.stdout;
}

afterEach(() => {
  for (const context of contexts.splice(0)) {
    archiveAndCleanupDesktopUiTestContext(context);
  }
});

describe("preparePierreVisualFixture", () => {
  it("creates three independent working-tree hunks and a committed editable file", () => {
    const environment: NodeJS.ProcessEnv = { CODDY_STUDIO_E2E_PLATFORM: "mac" };
    const context = prepareDesktopUiTestContext(environment);
    contexts.push(context);

    preparePierreVisualFixture(context.projectPath);

    const diff = git(context.projectPath, [
      "diff",
      "--unified=0",
      "--",
      pierreVisualFixture.diffFileName,
    ]);
    expect(diff.match(/^@@/gm)).toHaveLength(3);
    expect(diff).toContain('export const first = "changed one";');
    expect(diff).toContain('export const second = "changed two";');
    expect(diff).toContain('export const third = "changed three";');
    expect(
      git(context.projectPath, ["status", "--short", "--", pierreVisualFixture.diffFileName]),
    ).toBe(` M ${pierreVisualFixture.diffFileName}\n`);
    expect(git(context.projectPath, ["show", `HEAD:${pierreVisualFixture.editFileName}`])).toBe(
      'export const editableMessage = "original packaged text";\n',
    );
    expect(
      NodeFS.readFileSync(
        NodePath.join(context.projectPath, pierreVisualFixture.editFileName),
        "utf8",
      ),
    ).toBe('export const editableMessage = "original packaged text";\n');
  });
});
