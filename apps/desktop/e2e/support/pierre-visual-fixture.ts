// @effect-diagnostics nodeBuiltinImport:off - The packaged UI fixture owns disposable host files.
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";

const DIFF_BASELINE = [
  'export const first = "original one";',
  ...Array.from(
    { length: 8 },
    (_, index) => `export const firstSpacer${String(index + 1)} = ${String(index + 1)};`,
  ),
  'export const second = "original two";',
  ...Array.from(
    { length: 8 },
    (_, index) => `export const secondSpacer${String(index + 1)} = ${String(index + 11)};`,
  ),
  'export const third = "original three";',
  "",
].join("\n");

const DIFF_MODIFIED = DIFF_BASELINE.replace('first = "original one"', 'first = "changed one"')
  .replace('second = "original two"', 'second = "changed two"')
  .replace('third = "original three"', 'third = "changed three"');

const EDIT_BASELINE = 'export const editableMessage = "original packaged text";\n';

export const pierreVisualFixture = {
  diffFileName: "pierre-step5.ts",
  editFileName: "pierre-edit.ts",
  originalFileContents: EDIT_BASELINE,
  editedFileContents: 'export const editableMessage = "edited packaged text";\n',
} as const;

function runGit(projectPath: string, args: ReadonlyArray<string>): string {
  const result = NodeChildProcess.spawnSync(
    "git",
    [
      "-C",
      projectPath,
      "-c",
      "user.name=Coddy Studio UI Fixture",
      "-c",
      "user.email=fixture@example.test",
      ...args,
    ],
    { encoding: "utf8", shell: false },
  );
  if (result.error !== undefined) throw result.error;
  if (result.status !== 0) {
    throw new Error(`Pierre visual fixture Git command failed: ${result.stderr}`);
  }
  return result.stdout;
}

export function preparePierreVisualFixture(projectPath: string): void {
  const diffPath = NodePath.join(projectPath, pierreVisualFixture.diffFileName);
  const editPath = NodePath.join(projectPath, pierreVisualFixture.editFileName);

  NodeFS.writeFileSync(diffPath, DIFF_BASELINE);
  NodeFS.writeFileSync(editPath, EDIT_BASELINE);
  runGit(projectPath, [
    "add",
    "--",
    pierreVisualFixture.diffFileName,
    pierreVisualFixture.editFileName,
  ]);

  const staged = NodeChildProcess.spawnSync(
    "git",
    [
      "-C",
      projectPath,
      "diff",
      "--cached",
      "--quiet",
      "--",
      pierreVisualFixture.diffFileName,
      pierreVisualFixture.editFileName,
    ],
    { encoding: "utf8", shell: false },
  );
  if (staged.error !== undefined) throw staged.error;
  if (staged.status === 1) {
    runGit(projectPath, [
      "commit",
      "-m",
      "Pierre visual fixture baseline",
      "--",
      pierreVisualFixture.diffFileName,
      pierreVisualFixture.editFileName,
    ]);
  } else if (staged.status !== 0) {
    throw new Error(`Could not inspect the Pierre visual fixture index: ${staged.stderr}`);
  }

  NodeFS.writeFileSync(diffPath, DIFF_MODIFIED);
}
