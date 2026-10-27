import { defineRule, type ESTree, type Node } from "@oxlint/plugins";
import * as Option from "effect/Option";

import {
  getPropertyName,
  resolveReferenceOrigin,
  type ReferenceOrigin,
  unwrapExpression,
} from "../utils.ts";

const RUNTIME_PROPERTIES = new Set(["platform", "arch"]);
const HOST_PROCESS_REFERENCE_FILE = "packages/shared/src/hostProcess.ts";
const NODE_OS_MODULES = new Set(["node:os", "os"]);
const NODE_PROCESS_MODULES = new Set(["node:process", "process"]);

const normalizePath = (path: string) => path.replaceAll("\\", "/");

export const toRepoPath = (filename: string, cwd: string) => {
  const normalizedFilename = normalizePath(filename);
  const normalizedCwd = normalizePath(cwd).replace(/\/+$/u, "");
  const prefix = `${normalizedCwd}/`;
  return normalizedFilename.startsWith(prefix)
    ? normalizedFilename.slice(prefix.length)
    : normalizedFilename;
};

const isHostProcessReferenceFile = (filename: string, cwd: string) =>
  toRepoPath(filename, cwd) === HOST_PROCESS_REFERENCE_FILE;

interface RuntimeTarget {
  readonly kind: "process" | "os";
  readonly property: string;
}

const runtimeTarget = (
  origin: ReferenceOrigin,
  properties: ReadonlySet<string> = RUNTIME_PROPERTIES,
): Option.Option<RuntimeTarget> => {
  if (
    origin.kind === "global" &&
    origin.name === "process" &&
    origin.path.length === 1 &&
    properties.has(origin.path[0]!)
  ) {
    return Option.some({ kind: "process", property: origin.path[0]! });
  }
  if (
    origin.kind === "global" &&
    origin.name === "globalThis" &&
    origin.path.length === 2 &&
    origin.path[0] === "process" &&
    properties.has(origin.path[1]!)
  ) {
    return Option.some({ kind: "process", property: origin.path[1]! });
  }
  if (origin.kind !== "module" || origin.path.length !== 1) return Option.none();
  const property = origin.path[0]!;
  if (!properties.has(property)) return Option.none();
  if (NODE_PROCESS_MODULES.has(origin.source)) {
    return Option.some({ kind: "process", property });
  }
  return NODE_OS_MODULES.has(origin.source) ? Option.some({ kind: "os", property }) : Option.none();
};

const message = (property: string) =>
  `Use HostProcess${property === "arch" ? "Architecture" : "Platform"} instead of process.${property}; inject the runtime reference in Effect code and provide it explicitly in tests.`;

export default defineRule({
  meta: {
    type: "problem",
    docs: {
      description:
        "Disallow direct host runtime platform/architecture reads outside the shared host process references.",
    },
  },
  create(context) {
    const report = (node: Node, property: string) => {
      context.report({ node, message: message(property) });
    };

    const inspectPattern = (
      pattern: ESTree.BindingPattern | ESTree.BindingRestElement,
      origin: ReferenceOrigin,
    ): void => {
      if (pattern.type === "AssignmentPattern") {
        inspectPattern(pattern.left, origin);
        return;
      }
      if (pattern.type === "ArrayPattern") {
        for (const [index, element] of pattern.elements.entries()) {
          if (element !== null) {
            inspectPattern(element, { ...origin, path: [...origin.path, String(index)] });
          }
        }
        return;
      }
      if (pattern.type !== "ObjectPattern") return;

      const removedKeys = new Set<string>();
      for (const propertyNode of pattern.properties) {
        if (propertyNode.type === "RestElement") continue;
        const key = propertyNode.computed
          ? unwrapExpression(propertyNode.key).pipe(
              Option.filter((node) => node.type === "Literal"),
              Option.flatMap(getPropertyName),
            )
          : getPropertyName(propertyNode.key);
        if (Option.isSome(key)) removedKeys.add(key.value);
      }

      for (const propertyNode of pattern.properties) {
        if (propertyNode.type === "RestElement") {
          for (const property of RUNTIME_PROPERTIES) {
            if (removedKeys.has(property)) continue;
            const target = runtimeTarget({ ...origin, path: [...origin.path, property] });
            if (Option.isSome(target) && target.value.kind === "process") {
              report(propertyNode, property);
            }
          }
          continue;
        }

        const key = propertyNode.computed
          ? unwrapExpression(propertyNode.key).pipe(
              Option.filter((node) => node.type === "Literal"),
              Option.flatMap(getPropertyName),
            )
          : getPropertyName(propertyNode.key);
        if (Option.isNone(key)) continue;
        const childOrigin = { ...origin, path: [...origin.path, key.value] };
        const target = runtimeTarget(childOrigin);
        if (
          Option.isSome(target) &&
          target.value.kind === "process" &&
          RUNTIME_PROPERTIES.has(target.value.property)
        ) {
          report(propertyNode, target.value.property);
          continue;
        }
        inspectPattern(propertyNode.value, childOrigin);
      }
    };

    return {
      ImportDeclaration(node) {
        if (isHostProcessReferenceFile(context.filename, context.cwd)) return;
        if (!NODE_PROCESS_MODULES.has(node.source.value) || node.importKind === "type") return;
        for (const specifier of node.specifiers) {
          if (specifier.type !== "ImportSpecifier" || specifier.importKind === "type") continue;
          const imported = getPropertyName(specifier.imported);
          if (Option.isSome(imported) && RUNTIME_PROPERTIES.has(imported.value)) {
            report(specifier, imported.value);
          }
        }
      },
      MemberExpression(node) {
        if (isHostProcessReferenceFile(context.filename, context.cwd)) return;
        const target = resolveReferenceOrigin(context, node).pipe(Option.flatMap(runtimeTarget));
        if (Option.isSome(target) && target.value.kind === "process") {
          report(node, target.value.property);
        }
      },
      VariableDeclarator(node) {
        if (isHostProcessReferenceFile(context.filename, context.cwd)) return;
        if (node.init === null) return;
        const origin = resolveReferenceOrigin(context, node.init);
        if (Option.isNone(origin)) return;
        inspectPattern(node.id, origin.value);
      },
      CallExpression(node) {
        if (isHostProcessReferenceFile(context.filename, context.cwd)) return;
        const target = resolveReferenceOrigin(context, node.callee).pipe(
          Option.flatMap(runtimeTarget),
        );
        if (Option.isSome(target) && target.value.kind === "os") {
          report(node, target.value.property);
        }
      },
    };
  },
});
