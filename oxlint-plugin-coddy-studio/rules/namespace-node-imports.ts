import { defineRule } from "@oxlint/plugins";

const NODE_MODULE_ALIASES = new Map([
  ["assert/strict", "Assert"],
  ["fs/promises", "FSP"],
]);

const NODE_SEGMENT_ALIASES = new Map([
  ["fs", "FS"],
  ["os", "OS"],
  ["url", "URL"],
  ["vm", "VM"],
]);

const toPascalCase = (value: string) =>
  value
    .split(/[_-]/u)
    .filter((segment) => segment.length > 0)
    .map((segment) => segment.charAt(0).toUpperCase() + segment.slice(1))
    .join("");

const expectedNamespaceAlias = (source: string) => {
  const moduleName = source.slice("node:".length);
  const knownAlias = NODE_MODULE_ALIASES.get(moduleName);
  if (knownAlias !== undefined) return `Node${knownAlias}`;

  return `Node${moduleName
    .split("/")
    .map((segment) => NODE_SEGMENT_ALIASES.get(segment) ?? toPascalCase(segment))
    .join("")}`;
};

export default defineRule({
  meta: {
    type: "problem",
    docs: {
      description:
        "Require canonical namespace syntax and aliases for static ESM imports from node: modules; re-exports, dynamic imports, and CommonJS require are outside this rule.",
    },
  },
  create(context) {
    return {
      ImportDeclaration(node) {
        const source = node.source.value;
        if (!source.startsWith("node:")) return;

        const expectedAlias = expectedNamespaceAlias(source);
        const namespaceImport =
          node.specifiers.length === 1 && node.specifiers[0]?.type === "ImportNamespaceSpecifier"
            ? node.specifiers[0]
            : undefined;
        const actualAlias = namespaceImport?.local.name;

        if (actualAlias === expectedAlias) return;

        context.report({
          node,
          message: `Import ${source} as a namespace named ${expectedAlias}.`,
        });
      },
    };
  },
});
