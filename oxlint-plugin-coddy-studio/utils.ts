import type { Context, ESTree, Reference, Scope, Variable } from "@oxlint/plugins";
import * as Option from "effect/Option";

type ExpressionWrapper =
  | ESTree.ChainExpression
  | ESTree.ParenthesizedExpression
  | ESTree.TSNonNullExpression
  | ESTree.TSAsExpression
  | ESTree.TSSatisfiesExpression
  | ESTree.TSTypeAssertion;

type AstNode = ESTree.Node;

export type ReferenceOrigin =
  | {
      readonly kind: "global";
      readonly name: string;
      readonly path: ReadonlyArray<string>;
    }
  | {
      readonly kind: "module";
      readonly source: string;
      readonly path: ReadonlyArray<string>;
    };

const asAstNode = (node: unknown): Option.Option<AstNode> =>
  typeof node === "object" && node !== null && "type" in node && typeof node.type === "string"
    ? Option.some(node as AstNode)
    : Option.none();

const isExpressionWrapper = (node: AstNode): node is ExpressionWrapper =>
  node.type === "ChainExpression" ||
  node.type === "ParenthesizedExpression" ||
  node.type === "TSNonNullExpression" ||
  node.type === "TSAsExpression" ||
  node.type === "TSSatisfiesExpression" ||
  node.type === "TSTypeAssertion";

export function unwrapExpression(node: unknown): Option.Option<AstNode> {
  let current = asAstNode(node);

  while (Option.isSome(current) && isExpressionWrapper(current.value)) {
    current = asAstNode(current.value.expression);
  }

  return current;
}

export function getPropertyName(node: unknown): Option.Option<string> {
  return Option.flatMap(asAstNode(node), (expression) => {
    if (expression.type === "Identifier" && typeof expression.name === "string") {
      return Option.some(expression.name);
    }
    if (expression.type === "PrivateIdentifier" && typeof expression.name === "string") {
      return Option.some(expression.name);
    }
    if (expression.type === "Literal" && typeof expression.value === "string") {
      return Option.some(expression.value);
    }
    return Option.none();
  });
}

export function isIdentifier(node: Option.Option<AstNode>, name?: string): boolean {
  if (Option.isNone(node)) return false;
  const expression = node.value;
  return (
    expression.type === "Identifier" &&
    typeof expression.name === "string" &&
    (name === undefined || expression.name === name)
  );
}

const findReference = (
  context: Context,
  expression: Reference["identifier"],
): Option.Option<Reference> => {
  for (const scope of context.sourceCode.scopeManager.scopes) {
    const reference = scope.references.find((candidate) => candidate.identifier === expression);
    if (reference !== undefined) return Option.some(reference);
  }

  return Option.none();
};

const getStaticMemberPropertyName = (node: ESTree.MemberExpression): Option.Option<string> => {
  const property = Option.getOrThrow(unwrapExpression(node.property));
  if (node.computed && property.type !== "Literal") return Option.none();
  return getPropertyName(property);
};

const findBindingPath = (
  pattern: AstNode,
  binding: AstNode,
): Option.Option<ReadonlyArray<string>> => {
  if (pattern.type === "Identifier") {
    return pattern === binding ? Option.some([]) : Option.none();
  }
  if (pattern.type === "AssignmentPattern") {
    return findBindingPath(pattern.left, binding);
  }
  if (pattern.type !== "ObjectPattern") return Option.none();

  for (const property of pattern.properties) {
    if (property.type !== "Property") continue;
    const childPath = findBindingPath(property.value, binding);
    if (Option.isNone(childPath)) continue;

    const key = property.computed
      ? unwrapExpression(property.key).pipe(
          Option.filter((node) => node.type === "Literal"),
          Option.flatMap(getPropertyName),
        )
      : getPropertyName(property.key);
    if (Option.isSome(key)) return Option.some([key.value, ...childPath.value]);
    return Option.none();
  }

  return Option.none();
};

const appendPath = (origin: ReferenceOrigin, path: ReadonlyArray<string>): ReferenceOrigin => ({
  ...origin,
  path: [...origin.path, ...path],
});

const hasModuleLifetime = (variable: Variable): boolean =>
  variable.scope.variableScope.type === "module" || variable.scope.variableScope.type === "global";

const scopeContains = (ancestor: Scope, descendant: Scope): boolean => {
  for (let current: Scope | null = descendant; current !== null; current = current.upper) {
    if (current === ancestor) return true;
  }
  return false;
};

const findDeclaredVariable = (
  context: Context,
  declaration: AstNode,
  identifier: AstNode,
): Option.Option<Variable> =>
  Option.fromNullishOr(
    context.sourceCode.scopeManager
      .getDeclaredVariables(declaration)
      .find((variable) => variable.identifiers.includes(identifier as Reference["identifier"])),
  );

const outerWrappedExpression = (node: AstNode): AstNode => {
  let expression = node;
  while (
    isExpressionWrapper(expression.parent as AstNode) &&
    (expression.parent as ExpressionWrapper).expression === expression
  ) {
    expression = expression.parent as ExpressionWrapper;
  }
  return expression;
};

const getDirectCall = (node: AstNode): Option.Option<ESTree.CallExpression> => {
  const expression = outerWrappedExpression(node);
  const parent = expression.parent as AstNode;
  return parent.type === "CallExpression" && parent.callee === expression
    ? Option.some(parent)
    : Option.none();
};

const getInitializerDeclarator = (node: AstNode): Option.Option<ESTree.VariableDeclarator> => {
  const expression = outerWrappedExpression(node);
  const parent = expression.parent as AstNode;
  return parent.type === "VariableDeclarator" && parent.init === expression
    ? Option.some(parent)
    : Option.none();
};

const isImmutableVariable = (variable: Variable): boolean =>
  variable.references.every((reference) => !reference.isWrite() || reference.init);

const getImmutableInitializerVariable = (
  context: Context,
  declarator: ESTree.VariableDeclarator,
): Option.Option<Variable> => {
  if (
    declarator.id.type !== "Identifier" ||
    declarator.init === null ||
    declarator.parent.type !== "VariableDeclaration" ||
    (declarator.parent.kind !== "const" && declarator.parent.kind !== "let")
  ) {
    return Option.none();
  }
  return findDeclaredVariable(context, declarator, declarator.id).pipe(
    Option.filter(isImmutableVariable),
  );
};

const getImmutableAliasVariable = (context: Context, node: AstNode): Option.Option<Variable> =>
  getInitializerDeclarator(node).pipe(
    Option.flatMap((declarator) => getImmutableInitializerVariable(context, declarator)),
  );

const getFunctionVariable = (context: Context, block: AstNode): Option.Option<Variable> => {
  const initializer = getInitializerDeclarator(block).pipe(
    Option.flatMap((declarator) => getImmutableInitializerVariable(context, declarator)),
  );
  if (Option.isSome(initializer)) return initializer;
  if (
    (block.type === "FunctionDeclaration" || block.type === "FunctionExpression") &&
    block.id !== null
  ) {
    return findDeclaredVariable(context, block, block.id).pipe(Option.filter(isImmutableVariable));
  }
  return Option.none();
};

interface InvocationSite {
  readonly node: AstNode;
  readonly scope: Scope;
}

const collectFunctionCallSites = (
  context: Context,
  variable: Variable,
): ReadonlyArray<InvocationSite> => {
  const sites: Array<InvocationSite> = [];

  for (const reference of variable.references) {
    if (!reference.isRead()) continue;
    const call = getDirectCall(reference.identifier);
    if (Option.isSome(call)) {
      sites.push({ node: call.value, scope: reference.from.variableScope });
    }
    const alias = getImmutableAliasVariable(context, reference.identifier);
    if (Option.isSome(alias)) {
      sites.push(...collectFunctionCallSites(context, alias.value));
    }
  }
  return sites;
};

interface ObjectMethodBinding {
  readonly variable: Variable;
  readonly property: string;
}

const getObjectMethodBinding = (
  context: Context,
  block: AstNode,
): Option.Option<ObjectMethodBinding> => {
  const value = outerWrappedExpression(block);
  const property = value.parent as AstNode;
  if (property.type !== "Property" || property.value !== value) return Option.none();
  const propertyName = property.computed
    ? unwrapExpression(property.key).pipe(
        Option.filter((node) => node.type === "Literal"),
        Option.flatMap(getPropertyName),
      )
    : getPropertyName(property.key);
  if (Option.isNone(propertyName)) return Option.none();
  const object = property.parent as ESTree.ObjectExpression;
  return getInitializerDeclarator(object).pipe(
    Option.flatMap((declarator) => getImmutableInitializerVariable(context, declarator)),
    Option.map((variable) => ({ variable, property: propertyName.value })),
  );
};

const getObjectMember = (
  node: AstNode,
  property: string,
): Option.Option<ESTree.MemberExpression> => {
  const object = outerWrappedExpression(node);
  const parent = object.parent as AstNode;
  if (parent.type !== "MemberExpression" || parent.object !== object) return Option.none();
  return getStaticMemberPropertyName(parent).pipe(
    Option.filter((name) => name === property),
    Option.map(() => parent),
  );
};

const isMemberMutation = (member: ESTree.MemberExpression): boolean => {
  const target = outerWrappedExpression(member);
  const parent = target.parent as AstNode;
  return (
    (parent.type === "AssignmentExpression" && parent.left === target) ||
    (parent.type === "UpdateExpression" && parent.argument === target) ||
    (parent.type === "UnaryExpression" &&
      parent.operator === "delete" &&
      parent.argument === target)
  );
};

interface ObjectMethodScan {
  readonly immutable: boolean;
  readonly sites: ReadonlyArray<InvocationSite>;
}

const collectObjectMethodCallSites = (
  context: Context,
  variable: Variable,
  property: string,
): ObjectMethodScan => {
  const sites: Array<InvocationSite> = [];

  for (const reference of variable.references) {
    if (!reference.isRead()) continue;
    const alias = getImmutableAliasVariable(context, reference.identifier);
    if (Option.isSome(alias)) {
      const nested = collectObjectMethodCallSites(context, alias.value, property);
      if (!nested.immutable) return nested;
      sites.push(...nested.sites);
    }

    const member = getObjectMember(reference.identifier, property);
    if (Option.isNone(member)) continue;
    if (isMemberMutation(member.value)) return { immutable: false, sites: [] };
    const call = getDirectCall(member.value);
    if (Option.isSome(call)) {
      sites.push({ node: call.value, scope: reference.from.variableScope });
    }
  }
  return { immutable: true, sites };
};

const findOuterExecutionScope = (scope: Scope): Scope => scope.upper!.variableScope;

const invocationSitesForFunction = (
  context: Context,
  scope: Scope,
): ReadonlyArray<InvocationSite> => {
  const block = scope.block;
  const sites: Array<InvocationSite> = [];
  const iife = getDirectCall(block);
  const outerScope = findOuterExecutionScope(scope);
  if (Option.isSome(iife)) {
    sites.push({ node: iife.value, scope: outerScope });
  }

  const variable = getFunctionVariable(context, block);
  if (Option.isSome(variable)) {
    sites.push(...collectFunctionCallSites(context, variable.value));
  }

  const objectMethod = getObjectMethodBinding(context, block);
  if (Option.isSome(objectMethod)) {
    const scan = collectObjectMethodCallSites(
      context,
      objectMethod.value.variable,
      objectMethod.value.property,
    );
    if (scan.immutable) sites.push(...scan.sites);
  }
  return sites;
};

const executionSitesForScope = (context: Context, scope: Scope): ReadonlyArray<InvocationSite> => {
  const block = scope.block;
  if (
    block.type === "FunctionDeclaration" ||
    block.type === "FunctionExpression" ||
    block.type === "ArrowFunctionExpression"
  ) {
    return invocationSitesForFunction(context, scope);
  }
  if (scope.type === "class-static-block") {
    return [{ node: block, scope: findOuterExecutionScope(scope) }];
  }
  return [];
};

function executionScopeRunsBefore(
  context: Context,
  scope: Scope,
  readReference: Reference,
  resolving: ReadonlySet<Scope>,
): boolean {
  if (resolving.has(scope)) return false;
  const nextResolving = new Set(resolving).add(scope);
  return executionSitesForScope(context, scope).some((site) =>
    invocationSiteRunsBefore(context, site, readReference, nextResolving),
  );
}

function invocationSiteRunsBefore(
  context: Context,
  site: InvocationSite,
  readReference: Reference,
  resolving: ReadonlySet<Scope>,
): boolean {
  return site.scope === readReference.from.variableScope
    ? site.node.start < readReference.identifier.start
    : executionScopeRunsBefore(context, site.scope, readReference, resolving);
}

const retainsInitialProvenance = (
  context: Context,
  variable: Variable,
  readReference: Reference,
): boolean =>
  variable.references.every((writeReference) => {
    if (!writeReference.isWrite() || writeReference.init) return true;

    const readScope = readReference.from.variableScope;
    const writeScope = writeReference.from.variableScope;
    if (writeScope === readScope) {
      return writeReference.identifier.start >= readReference.identifier.start;
    }
    if (scopeContains(writeScope, readScope)) {
      return writeReference.identifier.start >= readReference.identifier.start;
    }
    return !executionScopeRunsBefore(context, writeScope, readReference, new Set());
  });

export interface ReferenceBinding {
  readonly variable: Variable;
  readonly initializer: ESTree.Expression | null;
  readonly moduleLifetime: boolean;
}

export const getReferenceBinding = (
  context: Context,
  node: unknown,
): Option.Option<ReferenceBinding> => {
  const expression = unwrapExpression(node);
  if (Option.isNone(expression) || expression.value.type !== "Identifier") return Option.none();
  const reference = findReference(context, expression.value);
  if (Option.isNone(reference) || reference.value.resolved === null) return Option.none();
  const variable = reference.value.resolved;
  if (!retainsInitialProvenance(context, variable, reference.value)) return Option.none();

  let hasVariableDefinition = false;
  for (const definition of variable.defs) {
    if (definition.type === "Variable") hasVariableDefinition = true;
    if (
      definition.type === "Variable" &&
      definition.node.type === "VariableDeclarator" &&
      definition.node.parent.type === "VariableDeclaration" &&
      (definition.node.parent.kind === "const" || definition.node.parent.kind === "let") &&
      definition.node.init !== null
    ) {
      return Option.some({
        variable,
        initializer: definition.node.init,
        moduleLifetime: hasModuleLifetime(variable),
      });
    }
  }

  if (hasVariableDefinition) return Option.none();
  return Option.some({ variable, initializer: null, moduleLifetime: hasModuleLifetime(variable) });
};

const resolveReferenceOriginInternal = (
  context: Context,
  node: unknown,
  resolving: ReadonlySet<Variable>,
): Option.Option<ReferenceOrigin> => {
  const expression = unwrapExpression(node);
  if (Option.isNone(expression)) return Option.none();

  if (expression.value.type === "MemberExpression") {
    const property = getStaticMemberPropertyName(expression.value);
    if (Option.isNone(property)) return Option.none();
    return resolveReferenceOriginInternal(context, expression.value.object, resolving).pipe(
      Option.map((origin) => appendPath(origin, [property.value])),
    );
  }

  if (expression.value.type === "CallExpression") {
    if (!isUnresolvedIdentifierReference(context, expression.value.callee, "require")) {
      return Option.none();
    }
    const source = unwrapExpression(expression.value.arguments[0]);
    if (
      Option.isNone(source) ||
      source.value.type !== "Literal" ||
      typeof source.value.value !== "string"
    ) {
      return Option.none();
    }
    return Option.some({
      kind: "module",
      source: source.value.value,
      path: [],
    });
  }

  if (expression.value.type !== "Identifier") return Option.none();
  const reference = findReference(context, expression.value);
  if (Option.isNone(reference)) return Option.none();
  const variable = reference.value.resolved;
  if (variable === null || variable.defs.length === 0) {
    return Option.some({ kind: "global", name: expression.value.name, path: [] });
  }
  if (resolving.has(variable) || !retainsInitialProvenance(context, variable, reference.value)) {
    return Option.none();
  }

  const nextResolving = new Set(resolving).add(variable);
  for (const definition of variable.defs) {
    if (definition.type === "ImportBinding" && definition.parent?.type === "ImportDeclaration") {
      const source = definition.parent.source.value;
      const imported =
        definition.node.type === "ImportSpecifier"
          ? getPropertyName(definition.node.imported)
          : Option.some("");
      const importedName = Option.getOrThrow(imported);
      return Option.some({
        kind: "module",
        source,
        path: importedName === "" ? [] : [importedName],
      });
    }

    if (
      definition.type !== "Variable" ||
      definition.node.type !== "VariableDeclarator" ||
      definition.node.parent.type !== "VariableDeclaration" ||
      (definition.node.parent.kind !== "const" && definition.node.parent.kind !== "let") ||
      definition.node.init === null
    ) {
      continue;
    }

    const bindingPath = findBindingPath(definition.node.id, definition.name);
    if (Option.isNone(bindingPath)) continue;
    const origin = resolveReferenceOriginInternal(context, definition.node.init, nextResolving);
    if (Option.isSome(origin)) return Option.some(appendPath(origin.value, bindingPath.value));
  }

  return Option.none();
};

export const resolveReferenceOrigin = (
  context: Context,
  node: unknown,
): Option.Option<ReferenceOrigin> => resolveReferenceOriginInternal(context, node, new Set());

export const isUnresolvedIdentifierReference = (
  context: Context,
  node: unknown,
  name: string,
): boolean => {
  const expression = unwrapExpression(node);
  if (
    Option.isNone(expression) ||
    expression.value.type !== "Identifier" ||
    expression.value.name !== name
  ) {
    return false;
  }

  return findReference(context, expression.value).pipe(
    Option.exists(
      (reference) => reference.resolved === null || reference.resolved.defs.length === 0,
    ),
  );
};
