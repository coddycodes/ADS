// @effect-diagnostics nodeBuiltinImport:off - Repository skill parity checks read the actual files.
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";
import { expect, it } from "vite-plus/test";

const REPOSITORY_ROOT = NodePath.resolve(import.meta.dirname, "..");

function skillFiles(directory: string): Array<string> {
  if (!NodeFS.existsSync(directory)) return [];
  return NodeFS.globSync("*/SKILL.md", { cwd: directory }).sort();
}

function frontmatter(text: string): string | undefined {
  return /^---\r?\n[\s\S]*?\r?\n---\r?\n/.exec(text)?.[0];
}

it("keeps Codex and Claude repository skills as identical plain files in both directions", () => {
  const codexRoot = NodePath.join(REPOSITORY_ROOT, ".agents", "skills");
  const claudeRoot = NodePath.join(REPOSITORY_ROOT, ".claude", "skills");
  const codexFiles = skillFiles(codexRoot);
  const claudeFiles = skillFiles(claudeRoot);

  expect(codexFiles, "Repository skills must exist").not.toEqual([]);
  expect(claudeFiles, "Every skill must be present for both agents").toEqual(codexFiles);

  for (const relativePath of codexFiles) {
    const codexPath = NodePath.join(codexRoot, relativePath);
    const claudePath = NodePath.join(claudeRoot, relativePath);
    expect(NodeFS.lstatSync(codexPath).isFile(), `${codexPath} must be a plain file`).toBe(true);
    expect(NodeFS.lstatSync(claudePath).isFile(), `${claudePath} must be a plain file`).toBe(true);
    expect(NodeFS.readFileSync(claudePath), `${relativePath} must match byte for byte`).toEqual(
      NodeFS.readFileSync(codexPath),
    );
  }
});

it("keeps every skill stub pointing at its docs/skills source with the same frontmatter", () => {
  const codexRoot = NodePath.join(REPOSITORY_ROOT, ".agents", "skills");

  for (const relativePath of skillFiles(codexRoot)) {
    const name = NodePath.dirname(relativePath);
    const source = `docs/skills/${name}/SKILL.md`;
    const sourcePath = NodePath.join(REPOSITORY_ROOT, source);
    const stub = NodeFS.readFileSync(NodePath.join(codexRoot, relativePath), "utf8");
    expect(stub, `${relativePath} must point at ${source}`).toContain(source);
    expect(NodeFS.existsSync(sourcePath), `${source} must exist`).toBe(true);
    expect(NodeFS.lstatSync(sourcePath).isFile(), `${source} must be a plain file`).toBe(true);
    const stubFrontmatter = frontmatter(stub);
    expect(stubFrontmatter, `${relativePath} must name the skill`).toContain(`\nname: ${name}\n`);
    expect(stubFrontmatter, `${relativePath} must repeat the frontmatter of ${source}`).toEqual(
      frontmatter(NodeFS.readFileSync(sourcePath, "utf8")),
    );
  }
});
