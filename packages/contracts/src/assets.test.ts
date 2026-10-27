import { Exit, Option, SchemaIssue } from "effect";
import * as Schema from "effect/Schema";
import { describe, expect, it } from "vite-plus/test";

import {
  AssetAccessError,
  AssetAttachmentNotFoundError,
  AssetCreateUrlInput,
  AssetCreateUrlResult,
  AssetPreviewTypeValidationError,
  AssetProjectFaviconInspectionError,
  AssetProjectFaviconNotFoundError,
  AssetProjectFaviconResolutionError,
  AssetResource,
  AssetSigningKeyLoadError,
  AssetWorkspaceAssetInspectionError,
  AssetWorkspaceAssetNotFoundError,
  AssetWorkspaceContextNotFoundError,
  AssetWorkspaceContextResolutionError,
  AssetWorkspacePathValidationError,
  AssetWorkspaceResolutionError,
  AssetWorkspaceRootNormalizationError,
} from "./assets.ts";

const decodeResource = Schema.decodeUnknownSync(AssetResource);
const encodeResource = Schema.encodeSync(AssetResource);
const decodeCreateUrlInput = Schema.decodeUnknownSync(AssetCreateUrlInput);
const decodeCreateUrlResult = Schema.decodeUnknownSync(AssetCreateUrlResult);
const encodeCreateUrlResult = Schema.encodeSync(AssetCreateUrlResult);
const decodeAccessError = Schema.decodeUnknownSync(AssetAccessError);
const encodeAccessError = Schema.encodeSync(AssetAccessError);

interface DecodeFailureExpectation {
  readonly rootTag: SchemaIssue.Issue["_tag"];
  readonly paths?: ReadonlyArray<ReadonlyArray<PropertyKey>>;
  readonly containsTag?: SchemaIssue.Issue["_tag"];
  readonly childIssueCount?: number;
}

const collectIssues = (issue: SchemaIssue.Issue): ReadonlyArray<SchemaIssue.Issue> => {
  switch (issue._tag) {
    case "Filter":
    case "Encoding":
    case "Pointer":
      return [issue, ...collectIssues(issue.issue)];
    case "Composite":
    case "AnyOf":
      return [issue, ...issue.issues.flatMap((child) => collectIssues(child))];
    default:
      return [issue];
  }
};

const expectDecodeFailure = (
  schema: Schema.Decoder<unknown, never>,
  input: unknown,
  expected: DecodeFailureExpectation,
): void => {
  const exit = Schema.decodeUnknownExit(schema)(input);
  expect(Exit.isFailure(exit)).toBe(true);
  if (!Exit.isFailure(exit)) return;

  const error = Exit.findErrorOption(exit);
  expect(Option.isSome(error)).toBe(true);
  if (!Option.isSome(error)) return;

  expect(Schema.isSchemaError(error.value)).toBe(true);
  if (!Schema.isSchemaError(error.value)) return;

  const issue = error.value.issue;
  const issues = collectIssues(issue);
  expect(issue._tag).toBe(expected.rootTag);
  for (const path of expected.paths ?? []) {
    const paths = issues.flatMap((nested) => (nested._tag === "Pointer" ? [[...nested.path]] : []));
    expect(paths).toContainEqual([...path]);
  }
  if (expected.containsTag !== undefined) {
    expect(issues.map((nested) => nested._tag)).toContain(expected.containsTag);
  }
  if (expected.childIssueCount !== undefined) {
    expect(issue._tag === "Composite" || issue._tag === "AnyOf").toBe(true);
    if (issue._tag === "Composite" || issue._tag === "AnyOf") {
      expect(issue.issues).toHaveLength(expected.childIssueCount);
    }
  }
};

describe("AssetResource", () => {
  it("decodes and encodes every resource alternative", () => {
    const resources = [
      { _tag: "workspace-file", threadId: "thread-1", path: "src/main.ts" },
      { _tag: "attachment", attachmentId: "attachment-1" },
      { _tag: "project-favicon", cwd: "C:/work/coddy-studio" },
    ] as const;

    for (const resource of resources) {
      expect(encodeResource(decodeResource(resource))).toEqual(resource);
    }
  });

  it("reports structured failures for tags and bounded identifiers", () => {
    expectDecodeFailure(
      AssetResource,
      { _tag: "workspace-file", threadId: "thread-1", path: "   " },
      { rootTag: "AnyOf", paths: [["path"]], containsTag: "InvalidValue" },
    );
    expectDecodeFailure(
      AssetResource,
      { _tag: "attachment", attachmentId: "a".repeat(257) },
      { rootTag: "AnyOf", paths: [["attachmentId"]], containsTag: "InvalidValue" },
    );
    expectDecodeFailure(
      AssetResource,
      { _tag: "project-favicon", cwd: "a".repeat(1025) },
      { rootTag: "AnyOf", paths: [["cwd"]], containsTag: "InvalidValue" },
    );
    expectDecodeFailure(
      AssetResource,
      { _tag: "unknown" },
      {
        rootTag: "AnyOf",
        childIssueCount: 0,
      },
    );
  });
});

describe("asset URL schemas", () => {
  it("decodes an input envelope and round-trips a result", () => {
    const resource = {
      _tag: "attachment" as const,
      attachmentId: "attachment-1",
    };
    expect(decodeCreateUrlInput({ resource })).toEqual({ resource });

    const result = decodeCreateUrlResult({
      relativeUrl: "/assets/signed/attachment-1",
      expiresAt: 1_800_000_000_000,
    });
    expect(encodeCreateUrlResult(result)).toEqual({
      relativeUrl: "/assets/signed/attachment-1",
      expiresAt: 1_800_000_000_000,
    });
  });

  it("rejects an invalid result with a structured failure", () => {
    expectDecodeFailure(
      AssetCreateUrlResult,
      { relativeUrl: "x".repeat(4097), expiresAt: 1_800_000_000_000 },
      { rootTag: "Composite", paths: [["relativeUrl"]], containsTag: "InvalidValue" },
    );
  });
});

describe("asset access errors", () => {
  it("constructs, encodes, and decodes every public failure", () => {
    const workspaceResource = decodeResource({
      _tag: "workspace-file",
      threadId: "thread-1",
      path: "src/main.ts",
    });
    const attachmentResource = decodeResource({
      _tag: "attachment",
      attachmentId: "attachment-1",
    });
    const faviconResource = decodeResource({
      _tag: "project-favicon",
      cwd: "C:/work/coddy-studio",
    });
    const cause = new Error("asset failure");
    const cases = [
      {
        error: new AssetWorkspaceContextNotFoundError({ resource: workspaceResource }),
        message: "Workspace context was not found.",
      },
      {
        error: new AssetWorkspaceContextResolutionError({ resource: workspaceResource, cause }),
        message: "Failed to resolve workspace context.",
      },
      {
        error: new AssetWorkspaceRootNormalizationError({ resource: workspaceResource, cause }),
        message: "Failed to normalize the workspace root.",
      },
      {
        error: new AssetWorkspacePathValidationError({ resource: workspaceResource, cause }),
        message: "Workspace file path must be relative to the project root.",
      },
      {
        error: new AssetPreviewTypeValidationError({ resource: workspaceResource }),
        message: "Only browser documents and images can be previewed.",
      },
      {
        error: new AssetWorkspaceAssetInspectionError({ resource: workspaceResource, cause }),
        message: "Failed to inspect the workspace asset.",
      },
      {
        error: new AssetWorkspaceAssetNotFoundError({ resource: workspaceResource }),
        message: "Workspace asset was not found.",
      },
      {
        error: new AssetWorkspaceResolutionError({ resource: workspaceResource, cause }),
        message: "Failed to resolve workspace.",
      },
      {
        error: new AssetAttachmentNotFoundError({ resource: attachmentResource }),
        message: "Attachment was not found.",
      },
      {
        error: new AssetProjectFaviconResolutionError({ resource: faviconResource, cause }),
        message: "Failed to resolve project favicon.",
      },
      {
        error: new AssetProjectFaviconInspectionError({ resource: faviconResource, cause }),
        message: "Failed to inspect the project favicon.",
      },
      {
        error: new AssetProjectFaviconNotFoundError({ resource: faviconResource }),
        message: "Project favicon was not found.",
      },
      {
        error: new AssetSigningKeyLoadError({ resource: attachmentResource, cause }),
        message: "Failed to load the asset signing key.",
      },
    ];

    for (const { error, message } of cases) {
      expect(error.message).toBe(message);
      const decoded = decodeAccessError(encodeAccessError(error));
      expect(decoded._tag).toBe(error._tag);
      expect(decoded.message).toBe(message);
    }
  });
});
