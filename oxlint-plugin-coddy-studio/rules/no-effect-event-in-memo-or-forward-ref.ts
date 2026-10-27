import { defineRule, type Context, type ESTree, type Variable } from "@oxlint/plugins";
import * as Option from "effect/Option";

import {
  getReferenceBinding,
  isUnresolvedIdentifierReference,
  resolveReferenceOrigin,
  unwrapExpression,
} from "../utils.ts";

type FunctionNode = ESTree.Function | ESTree.ArrowFunctionExpression;

const resolveFunction = (
  context: Context,
  node: unknown,
  resolving: ReadonlySet<Variable> = new Set(),
): FunctionNode | undefined => {
  const expression = unwrapExpression(node);
  if (Option.isNone(expression)) return undefined;
  if (
    expression.value.type === "FunctionDeclaration" ||
    expression.value.type === "FunctionExpression" ||
    expression.value.type === "ArrowFunctionExpression"
  ) {
    return expression.value;
  }

  const binding = getReferenceBinding(context, expression.value);
  if (Option.isNone(binding) || resolving.has(binding.value.variable)) return undefined;
  for (const definition of binding.value.variable.defs) {
    if (definition.type === "FunctionName" && definition.node.type === "FunctionDeclaration") {
      return definition.node;
    }
    if (
      definition.type === "Variable" &&
      definition.node.type === "VariableDeclarator" &&
      definition.node.id.type === "Identifier" &&
      definition.node.parent.type === "VariableDeclaration" &&
      definition.node.parent.kind === "const"
    ) {
      return resolveFunction(
        context,
        binding.value.initializer,
        new Set(resolving).add(binding.value.variable),
      );
    }
  }
  return undefined;
};

const isNoComparator = (context: Context, node: unknown): boolean => {
  if (node === undefined) return true;

  const resolving = new Set<Variable>();
  let expression = unwrapExpression(node);
  while (Option.isSome(expression)) {
    if (
      (expression.value.type === "Literal" && expression.value.value === null) ||
      (expression.value.type === "UnaryExpression" && expression.value.operator === "void") ||
      isUnresolvedIdentifierReference(context, expression.value, "undefined")
    ) {
      return true;
    }

    const binding = getReferenceBinding(context, expression.value);
    if (Option.isNone(binding) || resolving.has(binding.value.variable)) return false;
    resolving.add(binding.value.variable);
    expression = unwrapExpression(binding.value.initializer);
  }
  return false;
};

const reactExport = (context: Context, callee: ESTree.CallExpression["callee"]) =>
  resolveReferenceOrigin(context, callee).pipe(
    Option.flatMap((origin) =>
      origin.kind === "module" && origin.source === "react" && origin.path.length === 1
        ? Option.some(origin.path[0]!)
        : Option.none(),
    ),
    Option.getOrUndefined,
  );

interface HookCall {
  readonly node: ESTree.CallExpression;
  readonly target: FunctionNode;
  readonly name: string;
}

const message = (hook?: string) =>
  `An Effect Event${hook ? ` reached through ${hook}` : ""} in a memo component without a compare function, or in a forwardRef component, keeps its first render's values on react-dom 19.2; use a plain function component, or pass what the handler needs as arguments and read callbacks through a ref.`;

export default defineRule({
  meta: {
    type: "problem",
    docs: {
      description:
        "Prevent stale React Effect Events in memo and forwardRef components, following same-file components and custom hooks only; imported components and hooks are not followed.",
    },
  },
  create(context) {
    const functions: FunctionNode[] = [];
    const prohibitedComponents = new Set<FunctionNode>();
    const effectEvents = new Map<FunctionNode, ESTree.CallExpression[]>();
    const hookCalls = new Map<FunctionNode, HookCall[]>();
    const hookCallers = new Map<FunctionNode, Set<FunctionNode>>();

    const enterFunction = (node: FunctionNode) => {
      functions.push(node);
    };
    const exitFunction = () => {
      functions.pop();
    };

    return {
      FunctionDeclaration: enterFunction,
      "FunctionDeclaration:exit": exitFunction,
      FunctionExpression: enterFunction,
      "FunctionExpression:exit": exitFunction,
      ArrowFunctionExpression: enterFunction,
      "ArrowFunctionExpression:exit": exitFunction,
      CallExpression(node) {
        const exported = reactExport(context, node.callee);
        // Visiting forwardRef itself also catches memo(forwardRef(C), compare).
        if (
          exported === "forwardRef" ||
          (exported === "memo" && isNoComparator(context, node.arguments[1]))
        ) {
          const component = resolveFunction(context, node.arguments[0]);
          if (component) prohibitedComponents.add(component);
        }

        // A nested function (including a JSX child) owns its own hook calls.
        const owner = functions.at(-1);
        if (!owner) return;
        if (exported === "useEffectEvent") {
          const calls = effectEvents.get(owner) ?? [];
          calls.push(node);
          effectEvents.set(owner, calls);
          return;
        }

        const callee = unwrapExpression(node.callee);
        if (
          Option.isNone(callee) ||
          callee.value.type !== "Identifier" ||
          !/^use[A-Z0-9]/u.test(callee.value.name)
        ) {
          return;
        }
        const target = resolveFunction(context, callee.value);
        if (!target) return;
        const calls = hookCalls.get(owner) ?? [];
        calls.push({ node, target, name: callee.value.name });
        hookCalls.set(owner, calls);
        const callers = hookCallers.get(target) ?? new Set<FunctionNode>();
        callers.add(owner);
        hookCallers.set(target, callers);
      },
      "Program:exit"() {
        // Propagate back through the hook graph once per function. Unlike caching
        // a recursive search's negative result, this remains correct across cycles.
        const reachesEffectEvent = new Set(effectEvents.keys());
        const pending = [...reachesEffectEvent];
        for (let index = 0; index < pending.length; index++) {
          for (const caller of hookCallers.get(pending[index]!) ?? []) {
            if (reachesEffectEvent.has(caller)) continue;
            reachesEffectEvent.add(caller);
            pending.push(caller);
          }
        }

        for (const component of prohibitedComponents) {
          for (const call of effectEvents.get(component) ?? []) {
            context.report({ node: call.callee, message: message() });
          }
          for (const call of hookCalls.get(component) ?? []) {
            if (reachesEffectEvent.has(call.target)) {
              context.report({ node: call.node.callee, message: message(call.name) });
            }
          }
        }
      },
    };
  },
});
