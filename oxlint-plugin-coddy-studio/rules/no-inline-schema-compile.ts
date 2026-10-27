import { defineRule } from "@oxlint/plugins";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";

import {
  getReferenceBinding,
  resolveReferenceOrigin,
  type ReferenceBinding,
  unwrapExpression,
} from "../utils.ts";

// Effect Schema decoder/encoder APIs allocate compiled functions. Keep them
// outside function bodies so hot paths do not rebuild compilers per call.
const COMPILER_METHODS = new Set<keyof typeof Schema>([
  "is",
  "asserts",
  "decodeEffect",
  "decodeExit",
  "decodeOption",
  "decodePromise",
  "decodeResult",
  "decodeSync",
  "decodeUnknownExit",
  "decodeUnknownEffect",
  "decodeUnknownOption",
  "decodeUnknownPromise",
  "decodeUnknownResult",
  "decodeUnknownSync",

  "encodeExit",
  "encodeEffect",
  "encodeOption",
  "encodePromise",
  "encodeResult",
  "encodeSync",
  "encodeUnknownExit",
  "encodeUnknownEffect",
  "encodeUnknownOption",
  "encodeUnknownPromise",
  "encodeUnknownResult",
  "encodeUnknownSync",
]);

const getSchemaApiMethod = (
  context: Parameters<typeof resolveReferenceOrigin>[0],
  callee: unknown,
): Option.Option<string> => {
  return resolveReferenceOrigin(context, callee).pipe(
    Option.flatMap((origin) => {
      if (origin.kind !== "module") return Option.none();
      if (origin.source === "effect/Schema" && origin.path.length === 1) {
        return Option.some(origin.path[0]!);
      }
      if (origin.source === "effect" && origin.path.length === 2 && origin.path[0] === "Schema") {
        return Option.some(origin.path[1]!);
      }
      return Option.none();
    }),
  );
};

const getSchemaCompilerMethod = (
  context: Parameters<typeof resolveReferenceOrigin>[0],
  callee: unknown,
): Option.Option<string> =>
  getSchemaApiMethod(context, callee).pipe(
    Option.filter((method) => COMPILER_METHODS.has(method as keyof typeof Schema)),
  );

const isStaticSchemaReference = (
  context: Parameters<typeof resolveReferenceOrigin>[0],
  node: unknown,
  resolving: ReadonlySet<ReferenceBinding["variable"]> = new Set(),
): boolean => {
  const expression = unwrapExpression(node);
  if (Option.isNone(expression)) return false;

  if (expression.value.type === "CallExpression") {
    return isNestedStaticSchemaCall(context, expression.value, resolving);
  }

  if (
    resolveReferenceOrigin(context, expression.value).pipe(
      Option.exists((origin) => origin.kind === "module"),
    )
  ) {
    return true;
  }

  if (expression.value.type === "MemberExpression") {
    return isStaticSchemaReference(context, expression.value.object, resolving);
  }

  if (expression.value.type !== "Identifier") return false;
  const binding = getReferenceBinding(context, expression.value);
  if (Option.isNone(binding) || resolving.has(binding.value.variable)) return false;
  if (binding.value.moduleLifetime) return true;
  if (binding.value.initializer === null) return false;
  return isStaticSchemaReference(
    context,
    binding.value.initializer,
    new Set(resolving).add(binding.value.variable),
  );
};

const isNestedStaticSchemaCall = (
  context: Parameters<typeof resolveReferenceOrigin>[0],
  node: unknown,
  resolving: ReadonlySet<ReferenceBinding["variable"]> = new Set(),
): boolean => {
  const expression = unwrapExpression(node);
  if (Option.isNone(expression) || expression.value.type !== "CallExpression") return false;

  const method = getSchemaApiMethod(context, expression.value.callee);
  if (Option.isNone(method)) return false;
  if (method.value === "fromJsonString") {
    const firstArg = expression.value.arguments[0];
    return isStaticSchemaReference(context, firstArg, resolving);
  }

  return true;
};

const messageHigh = (method: string) =>
  `Hoist Schema.${method}(...) to module scope: both the inline schema literal and the compiled function are rebuilt on every call. Move the compiled function to a module-level const.`;

const messageMedium = (method: string) =>
  `Hoist Schema.${method}(...) to module scope: the compiled function is rebuilt on every call. Move it to a module-level const.`;

export default defineRule({
  meta: {
    type: "problem",
    docs: {
      description:
        "Disallow Schema decoder/encoder compiler calls inside function bodies; hoist them to module scope.",
    },
  },
  createOnce(context) {
    let functionDepth = 0;

    const resetFunctionDepth = () => {
      functionDepth = 0;
    };

    const enterFunction = () => {
      functionDepth++;
    };

    const exitFunction = () => {
      functionDepth--;
    };

    return {
      before: resetFunctionDepth,
      FunctionDeclaration: enterFunction,
      "FunctionDeclaration:exit": exitFunction,
      FunctionExpression: enterFunction,
      "FunctionExpression:exit": exitFunction,
      ArrowFunctionExpression: enterFunction,
      "ArrowFunctionExpression:exit": exitFunction,
      CallExpression(node) {
        if (functionDepth === 0) return;

        const method = getSchemaCompilerMethod(context, node.callee);
        if (Option.isNone(method)) return;

        const firstArg = node.arguments[0];
        const high = firstArg && isNestedStaticSchemaCall(context, firstArg);
        if (!high && !isStaticSchemaReference(context, firstArg)) return;

        context.report({
          node: node.callee,
          message: high ? messageHigh(method.value) : messageMedium(method.value),
        });
      },
    };
  },
});
