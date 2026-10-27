import * as Schema from "effect/Schema";
import * as SchemaTransformation from "effect/SchemaTransformation";
import { describe, expect, it } from "vite-plus/test";
import { YAMLParseError } from "yaml";

import { fromYaml, fromYamlString, parseYaml, stringifyYaml } from "./schemaYaml.ts";

const ProjectConfig = Schema.Struct({
  name: Schema.String,
  enabled: Schema.Boolean,
  tags: Schema.Array(Schema.String),
});

const yamlSchemaWithOptions = (options: Parameters<typeof parseYaml>[0]) =>
  Schema.String.pipe(
    Schema.decodeTo(
      Schema.Unknown,
      new SchemaTransformation.Transformation(parseYaml(options), stringifyYaml()),
    ),
  );

const parserOptionsThrowing = (failure: Error): Parameters<typeof parseYaml>[0] =>
  Object.defineProperty({}, "prettyErrors", {
    get() {
      throw failure;
    },
  });

describe("schemaYaml helpers", () => {
  it("decodes YAML through a schema", () => {
    const decodeConfig = Schema.decodeUnknownSync(fromYaml(ProjectConfig));

    expect(
      decodeConfig(`name: coddy-studio
enabled: true
tags:
  - codex
  - effect
`),
    ).toEqual({
      name: "coddy",
      enabled: true,
      tags: ["codex", "effect"],
    });
  });

  it("encodes values as YAML text", () => {
    const encodeConfig = Schema.encodeSync(fromYaml(ProjectConfig));

    expect(
      encodeConfig({
        name: "coddy",
        enabled: true,
        tags: ["codex"],
      }),
    ).toBe(`name: coddy-studio
enabled: true
tags:
  - codex
`);
  });

  it("can be used as a schema transformation directly", () => {
    const schema = Schema.String.pipe(Schema.decodeTo(Schema.Unknown, fromYamlString));
    const decodeYaml = Schema.decodeUnknownSync(schema);

    expect(decodeYaml("answer: 42\n")).toEqual({ answer: 42 });
  });

  it("reports malformed YAML with safe structural diagnostics", () => {
    const decodeYaml = Schema.decodeUnknownSync(fromYaml(Schema.Unknown));
    const secret = "credential=secret-value";
    let error: unknown;

    try {
      decodeYaml(`name: ${secret}\n  bad-indent: nope\n`);
    } catch (cause) {
      error = cause;
    }

    expect(Schema.isSchemaError(error)).toBe(true);
    if (!Schema.isSchemaError(error)) {
      throw new Error("Expected a schema error");
    }
    expect(error.message).toBe("Invalid YAML (code=BLOCK_AS_IMPLICIT_KEY, line=1, column=7).");
    expect(error.message).not.toContain(secret);
  });

  it("maps YAML parse errors without source positions to a stable diagnostic", () => {
    const decodeYaml = Schema.decodeUnknownSync(
      yamlSchemaWithOptions(
        parserOptionsThrowing(
          new YAMLParseError([-1, -1], "TAG_RESOLVE_FAILED", "dependency detail"),
        ),
      ),
    );

    expect(() => decodeYaml("value\n")).toThrowError("Invalid YAML (code=TAG_RESOLVE_FAILED).");
  });

  it("maps non-YAML parser failures without exposing dependency details", () => {
    const secret = "credential=secret-value";
    const decodeYaml = Schema.decodeUnknownSync(
      yamlSchemaWithOptions(parserOptionsThrowing(new Error(secret))),
    );
    let error: unknown;

    try {
      decodeYaml("value\n");
    } catch (cause) {
      error = cause;
    }

    expect(Schema.isSchemaError(error)).toBe(true);
    if (!Schema.isSchemaError(error)) {
      throw new Error("Expected a schema error");
    }
    expect(error.message).toBe("Invalid YAML.");
    expect(error.message).not.toContain(secret);
  });

  it("does not expose stringify failure details", () => {
    const encodeYaml = Schema.encodeSync(fromYaml(Schema.Unknown));
    const secret = "credential=secret-value";
    let error: unknown;

    try {
      encodeYaml({
        toJSON() {
          throw new Error(secret);
        },
      });
    } catch (cause) {
      error = cause;
    }

    expect(Schema.isSchemaError(error)).toBe(true);
    if (!Schema.isSchemaError(error)) {
      throw new Error("Expected a schema error");
    }
    expect(error.message).toBe("Failed to stringify YAML.");
    expect(error.message).not.toContain(secret);
  });
});
