// @effect-diagnostics nodeBuiltinImport:off
import * as NodeFS from "node:fs";
import { assert, describe, it } from "@effect/vitest";
import * as NodeServices from "@effect/platform-node/NodeServices";
import * as Arr from "effect/Array";
import * as Cause from "effect/Cause";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as FileSystem from "effect/FileSystem";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Logger from "effect/Logger";
import * as Option from "effect/Option";
import * as Order from "effect/Order";
import * as Path from "effect/Path";
import * as References from "effect/References";
import * as Result from "effect/Result";
import * as Schema from "effect/Schema";
import * as Tracer from "effect/Tracer";

import type { RotatingFileSinkFileSystem } from "./logging.ts";

import {
  causeErrorTag,
  compactTraceAttributes,
  DEFAULT_MAX_PENDING_TRACE_BYTES,
  DEFAULT_MAX_PENDING_TRACE_RECORDS,
  decodeOtlpTraceRecords,
  errorTag,
  makeLocalFileTracer,
  makeTraceSink,
  sanitizeTraceText,
  spanToTraceRecord,
  type EffectTraceRecord,
  TraceSinkConfigurationError,
  type TraceSinkDiagnostic,
  type TraceSink,
  type TraceRecord,
} from "./observability.ts";

describe("errorTag", () => {
  it("reports structural tags without retaining arbitrary values", () => {
    assert.equal(errorTag({ _tag: "AcpRequestError" }), "AcpRequestError");
    assert.equal(errorTag(new TypeError("secret-token-value")), "TypeError");
    assert.equal(errorTag({ _tag: "secret token value" }), "TaggedError");
  });

  it("uses safe fallbacks for invalid names, primitives, and hostile objects", () => {
    const invalidName = new Error("private message");
    invalidName.name = "private error name";
    const hostile = new Proxy(
      {},
      {
        has() {
          throw new Error("proxy failure");
        },
      },
    );

    assert.equal(errorTag(invalidName), "Error");
    assert.equal(errorTag("plain failure"), "string");
    assert.equal(errorTag(hostile), "UnknownError");
  });
});

describe("causeErrorTag", () => {
  it("reports the tagged failure value instead of the Cause reason wrapper", () => {
    assert.equal(
      causeErrorTag(Cause.fail({ _tag: "ServerAuthInvalidCredentialError" })),
      "ServerAuthInvalidCredentialError",
    );
  });

  it("reports structural cause kinds when no typed failure exists", () => {
    assert.equal(causeErrorTag(Cause.die(new Error("unexpected"))), "Die");
    assert.equal(causeErrorTag(Cause.interrupt()), "Interrupt");
    assert.equal(causeErrorTag(Cause.empty), "Empty");
  });
});

const TraceRecordLine = Schema.Struct({
  name: Schema.String,
  spanId: Schema.String,
  parentSpanId: Schema.optional(Schema.String),
  attributes: Schema.Record(Schema.String, Schema.Unknown),
  events: Schema.Array(
    Schema.Struct({
      name: Schema.String,
      attributes: Schema.Record(Schema.String, Schema.Unknown),
    }),
  ),
  exit: Schema.optional(
    Schema.Struct({
      _tag: Schema.String,
    }),
  ),
});

const decodeTraceRecordLine = Schema.decodeUnknownSync(Schema.fromJsonString(TraceRecordLine));

const makeRecord = (name: string, suffix = ""): TraceRecord => ({
  type: "effect-span",
  name,
  traceId: `trace-${name}-${suffix}`,
  spanId: `span-${name}-${suffix}`,
  sampled: true,
  kind: "internal",
  startTimeUnixNano: "1",
  endTimeUnixNano: "2",
  durationMs: 1,
  attributes: {
    payload: suffix,
  },
  events: [],
  links: [],
  exit: {
    _tag: "Success",
  },
});

const makeMemoryTraceSink = () => {
  const records: Array<TraceRecord> = [];
  const sink = {
    filePath: "memory://trace.ndjson",
    push: (record) => records.push(record),
    flush: Effect.void,
    close: () => Effect.void,
    stats: () => ({
      pendingRecords: 0,
      pendingBytes: 0,
      droppedRecords: 0,
      consecutiveFailures: 0,
      maxPendingRecords: 0,
      maxPendingBytes: 0,
    }),
  } satisfies TraceSink;
  return { records, sink } as const;
};

const makeEndedSpan = (
  overrides: Partial<Parameters<typeof spanToTraceRecord>[0]> = {},
): Parameters<typeof spanToTraceRecord>[0] =>
  ({
    name: "bounded-span",
    traceId: "0123456789abcdef0123456789abcdef",
    spanId: "0123456789abcdef",
    parent: Option.none(),
    status: {
      _tag: "Ended",
      startTime: 1n,
      endTime: 2n,
      exit: Exit.succeed(undefined),
    },
    sampled: true,
    kind: "internal",
    attributes: new Map(),
    events: [],
    links: [],
    ...overrides,
  }) as Parameters<typeof spanToTraceRecord>[0];

const readTraceRecords = Effect.fn("readTraceRecords")(function* (tracePath: string) {
  const fileSystem = yield* FileSystem.FileSystem;
  return (yield* fileSystem.readFileString(tracePath))
    .trim()
    .split("\n")
    .filter((line) => line.length > 0)
    .map((line) => decodeTraceRecordLine(line));
});

const makeTestLayer = (tracePath: string, minimumLogLevel: "Trace" | "Info" = "Info") =>
  Layer.mergeAll(
    Layer.effect(
      Tracer.Tracer,
      makeLocalFileTracer({
        filePath: tracePath,
        maxBytes: 1024 * 1024,
        maxFiles: 2,
        batchWindowMs: 10_000,
      }),
    ),
    Logger.layer([Logger.tracerLogger], { mergeWithExisting: false }),
    Layer.succeed(References.MinimumLogLevel, minimumLogLevel),
  );

const nodeServicesIt = it.layer(NodeServices.layer);

describe("observability", () => {
  it("redacts sensitive and unsafe trace attribute values", () => {
    const secret = "coverage-secret-value";
    const unserializable = Object.defineProperty({}, "value", {
      enumerable: true,
      get() {
        throw new Error(`getter exposed ${secret}`);
      },
    });

    const attributes = compactTraceAttributes({
      authorization: `Bearer ${secret}`,
      nested: {
        password: secret,
        credential: secret,
        prompt: `private prompt ${secret}`,
        safe: "visible",
      },
      oversized: "x".repeat(4_097),
      error: new Error(`token=${secret}`),
      unserializable,
    });
    const serialized = JSON.stringify(attributes);

    assert.equal(serialized.includes(secret), false);
    assert.deepStrictEqual(attributes["nested"], {
      password: "[REDACTED]",
      credential: "[REDACTED]",
      prompt: "[REDACTED]",
      safe: "visible",
    });
    assert.equal(attributes["authorization"], "[REDACTED]");
    assert.equal(attributes["oversized"], "[Oversized string: 4097 characters]");
    assert.deepStrictEqual(attributes["unserializable"], {
      value: "[Unserializable]",
    });
    const normalizedError = attributes["error"] as {
      readonly name: string;
      readonly message: string;
      readonly stack: string;
    };
    assert.equal(normalizedError.name, "Error");
    assert.equal(normalizedError.message, "token=[REDACTED]");
    assert.equal(normalizedError.stack.includes("token=[REDACTED]"), true);
  });

  it("redacts sensitive properties without evaluating their getters", () => {
    let reads = 0;
    const attributes: Record<string, unknown> = { visible: "safe" };
    Object.defineProperty(attributes, "AcCeSs-ToKeN", {
      enumerable: true,
      get() {
        reads += 1;
        throw new Error("sensitive getter must not run");
      },
    });

    assert.deepStrictEqual(compactTraceAttributes(attributes), {
      visible: "safe",
      "AcCeSs-ToKeN": "[REDACTED]",
    });
    assert.equal(reads, 0);
  });

  it("redacts compound credential keys before reading values while preserving token metrics", () => {
    let sensitiveReads = 0;
    const attributes: Record<string, unknown> = {
      tokenCount: 7,
      countTokens: 9,
      tokensUsed: 10,
      tokenizerCount: 11,
      authorizationHeader: "Basic opaque-basic-value",
      "cookie-header": "session=opaque-session-value",
      "client_secret.value": "opaque-client-secret-value",
    };
    const sensitiveKeys = [
      "cookieHeader",
      "SetCookieHeader",
      "AUTHORIZATION_header",
      "accessTokenValue",
      "refresh-token-value",
      "ID.TOKEN.VALUE",
      "api_key_value",
      "ClientSecretValue",
      "password.value",
    ] as const;
    for (const key of sensitiveKeys) {
      Object.defineProperty(attributes, key, {
        enumerable: true,
        get() {
          sensitiveReads += 1;
          throw new Error(`must not read ${key}`);
        },
      });
    }

    const normalized = compactTraceAttributes(attributes);

    assert.equal(sensitiveReads, 0);
    for (const key of sensitiveKeys) {
      assert.equal(normalized[key], "[REDACTED]");
    }
    assert.equal(normalized["tokenCount"], 7);
    assert.equal(normalized["countTokens"], 9);
    assert.equal(normalized["tokensUsed"], 10);
    assert.equal(normalized["tokenizerCount"], 11);
    assert.equal(normalized["authorizationHeader"], "[REDACTED]");
    assert.equal(normalized["cookie-header"], "[REDACTED]");
    assert.equal(normalized["client_secret.value"], "[REDACTED]");
    assert.equal(JSON.stringify(normalized).includes("opaque-"), false);
  });

  it("sanitizes every string shape while preserving useful context and safe siblings", () => {
    const secrets = {
      password: "url-password-secret",
      access: "access-token-secret",
      refresh: "refresh-token-secret",
      id: "id-token-secret",
      cookie: "cookie-secret",
      custom: "custom-string-secret",
    } as const;
    const url = new URL(
      `https://reader:${secrets.password}@example.test/relay/status?access_token=${secrets.access}&safe=visible`,
    );
    const params = new URLSearchParams(
      `refresh-token=${secrets.refresh}&ID_TOKEN=${secrets.id}&ordinary=tokenization-is-useful`,
    );
    const hostileObject = {
      before: "preserved-before",
      get hostile() {
        throw new Error(`getter exposed ${secrets.custom}`);
      },
      after: "preserved-after",
    };
    const hostileArray: Array<unknown> = ["array-before", "placeholder", "array-after"];
    Object.defineProperty(hostileArray, 1, {
      enumerable: true,
      get() {
        throw new Error(`array getter exposed ${secrets.custom}`);
      },
    });
    const error = new Error(
      `relay failed at https://reader:${secrets.password}@example.test/relay/status?refresh_token=${secrets.refresh}`,
      {
        cause: {
          status: 503,
          setCookie: secrets.cookie,
          safe: "cause-visible",
        },
      },
    ) as Error & { status: number };
    error.status = 503;
    error.stack = `${error.stack ?? "Error"}\nCookie: session=${secrets.cookie}`;

    class CustomTraceValue {
      toString(): string {
        return `https://reader:${secrets.password}@example.test/custom?access-token=${secrets.custom}&safe=custom-visible`;
      }
    }

    class OversizedTraceValue {
      toString(): string {
        return "x".repeat(4_097);
      }
    }

    const attributes = compactTraceAttributes({
      url,
      params,
      custom: new CustomTraceValue(),
      error,
      hostileObject,
      hostileArray,
      headers: {
        Authorization: `Bearer ${secrets.access}`,
        COOKIE: `session=${secrets.cookie}`,
        "Set-Cookie": `session=${secrets.cookie}`,
        accessToken: secrets.access,
        "REFRESH-TOKEN": secrets.refresh,
        id_token: secrets.id,
      },
      tokenCount: 7,
      tokenizer: "ordinary tokenization remains visible",
      symbolic: Symbol(`token=${secrets.custom}`),
      oversizedCustom: new OversizedTraceValue(),
    });
    const serialized = JSON.stringify(attributes);

    for (const secret of Object.values(secrets)) {
      assert.equal(serialized.includes(secret), false);
    }
    assert.equal(serialized.includes("example.test/relay/status"), true);
    assert.equal(serialized.includes("safe=visible"), true);
    assert.equal(serialized.includes("ordinary=tokenization-is-useful"), true);
    assert.equal(serialized.includes("example.test/custom"), true);
    assert.equal(serialized.includes("safe=custom-visible"), true);
    assert.equal(serialized.includes("cause-visible"), true);
    assert.equal(serialized.includes("503"), true);
    assert.deepStrictEqual(attributes["hostileObject"], {
      before: "preserved-before",
      hostile: "[Unserializable]",
      after: "preserved-after",
    });
    assert.deepStrictEqual(attributes["hostileArray"], [
      "array-before",
      "[Unserializable]",
      "array-after",
    ]);
    assert.equal(attributes["tokenCount"], 7);
    assert.equal(attributes["tokenizer"], "ordinary tokenization remains visible");
    assert.equal(attributes["symbolic"], "Symbol(token=[REDACTED])");
    assert.equal(attributes["oversizedCustom"], "[Oversized string: 4097 characters]");
  });

  it("redacts complete cookie header values without masking ordinary cookie words", () => {
    const secrets = ["first-cookie-secret", "second-cookie-secret", "response-cookie-secret"];
    const sanitized = sanitizeTraceText(
      [
        "before cookie policy remains visible",
        `cOoKiE: first=${secrets[0]}; second=${secrets[1]}; theme=dark`,
        "status=204",
        `SET-cookie: session=${secrets[2]}; refresh=${secrets[1]}; Path=/; HttpOnly`,
        `inline Cookie=first=${secrets[0]}; second=${secrets[1]}, request=visible`,
        "cookieJar=visible and cookie-cutter=visible",
      ].join("\n"),
    );

    for (const secret of secrets) {
      assert.equal(sanitized.includes(secret), false);
    }
    assert.equal(sanitized.includes("theme=dark"), false);
    assert.equal(sanitized.includes("refresh="), false);
    assert.equal(sanitized.includes("status=204"), true);
    assert.equal(sanitized.includes("request=visible"), true);
    assert.equal(sanitized.includes("before cookie policy remains visible"), true);
    assert.equal(sanitized.includes("cookieJar=visible and cookie-cutter=visible"), true);
  });

  it("isolates hostile fields and handles every defensive normalization path", () => {
    const hostileError = new Error("replace me");
    Object.defineProperty(hostileError, "message", {
      configurable: true,
      get() {
        throw new Error("message getter failure");
      },
    });
    Object.defineProperty(hostileError, "stack", { value: "safe error stack" });
    const circularError = new Error("circular error") as Error & { cause?: unknown };
    circularError.cause = circularError;
    const hostileMapKey = {
      toString() {
        throw new Error("map key failure");
      },
    };
    class ThrowingTraceValue {
      toString(): string {
        throw new Error("custom value failure");
      }
    }
    const descriptorProxy = new Proxy(
      { safe: "proxy-safe", hostile: "hidden" },
      {
        getOwnPropertyDescriptor(target, key) {
          if (key === "hostile") {
            throw new Error("descriptor failure");
          }
          return Reflect.getOwnPropertyDescriptor(target, key);
        },
      },
    );
    const symbolKey = Symbol("safe-symbol-key");
    const symbolObject: Record<PropertyKey, unknown> = { visible: "symbol-sibling" };
    Object.defineProperty(symbolObject, symbolKey, { enumerable: true, value: "symbol-visible" });
    const topLevel = Object.defineProperties(
      { safe: "top-level-safe" },
      {
        hostile: {
          enumerable: true,
          get() {
            throw new Error("top-level getter failure");
          },
        },
        hidden: { enumerable: false, value: "not-emitted" },
      },
    );

    assert.deepStrictEqual(
      compactTraceAttributes({
        hostileError,
        circularError,
        hostileMap: new Map([[hostileMapKey, "map-value-safe"]]),
        throwingValue: new ThrowingTraceValue(),
        descriptorProxy,
        symbolObject,
        clientApiKey: "private-api-key",
      }),
      {
        hostileError: {
          name: "Error",
          message: "[Unserializable]",
          stack: "safe error stack",
        },
        circularError: {
          name: "Error",
          message: "circular error",
          stack: circularError.stack,
          cause: { name: "Error", message: "[Circular]" },
        },
        hostileMap: { "[Unserializable]": "map-value-safe" },
        throwingValue: "[Unserializable]",
        descriptorProxy: "[Unserializable]",
        symbolObject: {
          visible: "symbol-sibling",
        },
        clientApiKey: "[REDACTED]",
      },
    );
    assert.deepStrictEqual(compactTraceAttributes(topLevel), {
      safe: "top-level-safe",
      hostile: "[Unserializable]",
    });
    assert.equal(sanitizeTraceText("http://["), "[Unserializable]");
    assert.equal(
      sanitizeTraceText(`https://a:b@example.test/${"x".repeat(4_050)}`).startsWith(
        "[Oversized string:",
      ),
      true,
    );
  });

  it("enforces shared depth, node, and emitted-character traversal budgets", () => {
    let branchingGetterReads = 0;
    const makeBranchingObject = (depth: number): Record<string, unknown> => {
      if (depth === 0) {
        return { leaf: "visible" };
      }
      const value: Record<string, unknown> = {};
      for (let index = 0; index < 4; index += 1) {
        Object.defineProperty(value, `branch-${index}`, {
          enumerable: true,
          get() {
            branchingGetterReads += 1;
            return makeBranchingObject(depth - 1);
          },
        });
      }
      return value;
    };
    const deep: Record<string, unknown> = {};
    let cursor = deep;
    for (let depth = 0; depth < 32; depth += 1) {
      const child: Record<string, unknown> = {};
      cursor["child"] = child;
      cursor = child;
    }
    const hostileArray: Array<unknown> = Array.from({ length: 2_000 }, (_, index) => index);
    let lateGetterReads = 0;
    Object.defineProperty(hostileArray, 1_500, {
      get() {
        lateGetterReads += 1;
        return "must-not-be-read";
      },
    });
    const wide = Object.fromEntries(
      Array.from({ length: 2_000 }, (_, index) => [`field-${index}`, `value-${index}`]),
    );
    const hugeMap = new Map(
      Array.from({ length: 2_000 }, (_, index) => [`map-${index}`, `value-${index}`]),
    );
    const hugeSet = new Set(Array.from({ length: 2_000 }, (_, index) => `set-${index}`));
    const symbols = Array.from({ length: 2_000 }, (_, index) => Symbol(`symbol-${index}`));
    class CustomBudgetValue {
      readonly index: number;
      constructor(index: number) {
        this.index = index;
      }
      toString(): string {
        return `custom-${this.index}`;
      }
    }
    const customValues = Array.from({ length: 2_000 }, (_, index) => new CustomBudgetValue(index));
    const characterHeavy = Array.from({ length: 20 }, () => "x".repeat(4_000));
    const values = [
      deep,
      hostileArray,
      wide,
      hugeMap,
      hugeSet,
      symbols,
      customValues,
      characterHeavy,
      makeBranchingObject(7),
    ];

    for (const value of values) {
      const serialized = JSON.stringify(compactTraceAttributes({ value }));
      assert.equal(serialized.includes("[Truncated]"), true);
      assert.equal(serialized.length < 100_000, true);
    }
    assert.equal(lateGetterReads, 0);
    assert.equal(branchingGetterReads <= 600, true);

    const sharedRecordBudget = JSON.stringify(
      compactTraceAttributes({
        first: Array.from({ length: 400 }, (_, index) => `first-${index}`),
        second: Array.from({ length: 400 }, (_, index) => `second-${index}`),
        tail: "must-not-force-more-traversal",
      }),
    );
    assert.equal(sharedRecordBudget.includes("[Truncated]"), true);
  });

  it("stops Set traversal immediately when an emitted-character budget is exhausted", () => {
    const normalized = compactTraceAttributes({
      values: new Set(Array.from({ length: 20 }, (_, index) => `${index}-${"x".repeat(3_900)}`)),
    });
    const values = normalized["values"] as ReadonlyArray<unknown>;

    assert.equal(values.at(-1), "[Truncated]");
    assert.equal(values.length < 20, true);
  });

  it("places deterministic truncation markers at character-budget frontiers", () => {
    const objectWithHeavyKeys: Record<string, unknown> = {};
    const mapWithHeavyKeys = new Map<string, unknown>();
    for (let index = 0; index < 100; index += 1) {
      const key = `${index}-${"k".repeat(380)}`;
      objectWithHeavyKeys[key] = null;
      mapWithHeavyKeys.set(key, null);
    }
    const nameHeavyError = new Error("safe message");
    nameHeavyError.name = "n".repeat(4_000);
    const messageHeavyError = new Error("m".repeat(3_000));
    messageHeavyError.name = "n".repeat(1_000);
    const stackHeavyError = new Error("safe message");
    stackHeavyError.name = "SafeError";
    stackHeavyError.stack = "s".repeat(3_000);

    const serialized = [
      compactTraceAttributes({ value: objectWithHeavyKeys }),
      compactTraceAttributes({ value: mapWithHeavyKeys }),
      compactTraceAttributes({
        padding: [...Array.from({ length: 7 }, () => "p".repeat(4_000)), "p".repeat(1_000)],
        error: nameHeavyError,
      }),
      compactTraceAttributes({
        padding: [...Array.from({ length: 7 }, () => "p".repeat(4_000)), "p".repeat(1_000)],
        error: messageHeavyError,
      }),
      compactTraceAttributes({
        padding: [...Array.from({ length: 7 }, () => "p".repeat(4_000)), "p".repeat(2_000)],
        error: stackHeavyError,
      }),
      compactTraceAttributes({
        padding: [...Array.from({ length: 8 }, () => "p".repeat(4_000)), "p".repeat(744)],
        number: 12_345_678,
      }),
      compactTraceAttributes(
        new Proxy(
          {},
          {
            getPrototypeOf() {
              throw new Error("root proxy failure");
            },
          },
        ),
      ),
    ].map((value) => JSON.stringify(value));

    assert.equal(
      serialized.slice(0, 6).every((value) => value.includes("[Truncated]")),
      true,
    );
    assert.equal(serialized[6], '{"value":"[Unserializable]"}');
  });

  it("normalizes circular arrays, maps, and sets without recursing forever", () => {
    const array: Array<unknown> = ["alpha"];
    array.push(array);

    const map = new Map<string, unknown>();
    map.set("self", map);

    const set = new Set<unknown>();
    set.add(set);

    assert.deepStrictEqual(
      compactTraceAttributes({
        array,
        map,
        set,
      }),
      {
        array: ["alpha", "[Circular]"],
        map: { self: "[Circular]" },
        set: ["[Circular]"],
      },
    );
  });

  it("normalizes invalid dates without throwing", () => {
    // @effect-diagnostics-next-line globalDate:off
    const invalidDate = new Date("not-a-real-date");
    assert.deepStrictEqual(
      compactTraceAttributes({
        invalidDate,
      }),
      {
        invalidDate: "Invalid Date",
      },
    );
  });

  it("normalizes every supported value shape and omits undefined fields", () => {
    const object: Record<string, unknown> = { visible: true };
    object["self"] = object;
    const noStack = new Error("plain failure");
    Object.defineProperty(noStack, "stack", { value: "" });
    const map = new Map<string, unknown>([
      ["apiToken", "private"],
      ["safe", 1n],
    ]);

    assert.deepStrictEqual(
      compactTraceAttributes({
        omitted: undefined,
        nothing: null,
        text: "Bearer private-token",
        count: 2,
        enabled: false,
        big: 3n,
        // @effect-diagnostics-next-line globalDate:off
        date: new Date("2024-01-02T03:04:05.000Z"),
        noStack,
        custom: Symbol("custom-value"),
        object,
        map,
      }),
      {
        nothing: null,
        text: "Bearer [REDACTED]",
        count: 2,
        enabled: false,
        big: "3",
        date: "2024-01-02T03:04:05.000Z",
        noStack: { name: "Error", message: "plain failure" },
        custom: "Symbol(custom-value)",
        object: { visible: true, self: "[Circular]" },
        map: { apiToken: "[REDACTED]", safe: "1" },
      },
    );
  });

  it("shares one bounded construction budget across span attributes, events, links, and exit", () => {
    let attributeEntriesRead = 0;
    let sensitiveValuesRead = 0;
    let eventEntriesRead = 0;
    let linkEntriesRead = 0;
    const sensitiveValue = {};
    Object.defineProperty(sensitiveValue, "toString", {
      get() {
        sensitiveValuesRead += 1;
        throw new Error("sensitive value must not be inspected");
      },
    });
    const attributes = new Map<string, unknown>([["ACCESS_TOKEN", sensitiveValue]]);
    for (let index = 0; index < 10_000; index += 1) {
      attributes.set(`attribute-${index}`, { nested: [`value-${index}`] });
    }
    const originalEntries = attributes.entries.bind(attributes);
    attributes.entries = (() => {
      const iterator = originalEntries();
      return {
        next() {
          attributeEntriesRead += 1;
          return iterator.next();
        },
        [Symbol.iterator]() {
          return this;
        },
      };
    }) as typeof attributes.entries;
    const events = Array.from({ length: 10_000 }, (_, index) => [
      `event-${index}`,
      BigInt(index),
      { safe: index },
    ]) as Array<[string, bigint, Record<string, unknown>]>;
    const links = Array.from({ length: 10_000 }, (_, index) => ({
      span: Tracer.externalSpan({
        traceId: `trace-${index}`,
        spanId: `span-${index}`,
        sampled: true,
      }),
      attributes: { safe: index },
    }));
    const countedEvents = new Proxy(events, {
      get(target, key, receiver) {
        if (typeof key === "string" && /^\d+$/.test(key)) eventEntriesRead += 1;
        return Reflect.get(target, key, receiver);
      },
    });
    const countedLinks = new Proxy(links, {
      get(target, key, receiver) {
        if (typeof key === "string" && /^\d+$/.test(key)) linkEntriesRead += 1;
        return Reflect.get(target, key, receiver);
      },
    });

    const record = spanToTraceRecord(
      makeEndedSpan({ attributes, events: countedEvents, links: countedLinks }),
    );
    const serialized = JSON.stringify(record);

    assert.equal(sensitiveValuesRead, 0);
    assert.equal(attributeEntriesRead < 600, true);
    assert.equal(eventEntriesRead, 0);
    assert.equal(linkEntriesRead, 0);
    assert.equal(serialized.includes("[Truncated]"), true);
    assert.equal(serialized.length < 40_000, true);
    assert.equal(serialized.includes("must-not-read"), false);
  });

  it("bounds huge event and link collections before reading every entry", () => {
    let eventEntriesRead = 0;
    let linkEntriesRead = 0;
    const events = new Proxy(
      Array.from(
        { length: 10_000 },
        (_, index) =>
          [
            `event-${index}`,
            BigInt(index),
            { nested: new Map([[`map-${index}`, new Set([index])]]) },
          ] as [string, bigint, Record<string, unknown>],
      ),
      {
        get(target, key, receiver) {
          if (typeof key === "string" && /^\d+$/.test(key)) eventEntriesRead += 1;
          return Reflect.get(target, key, receiver);
        },
      },
    );
    const links = new Proxy(
      Array.from({ length: 10_000 }, (_, index) => ({
        span: Tracer.externalSpan({
          traceId: `trace-${index}`,
          spanId: `span-${index}`,
          sampled: true,
        }),
        attributes: { safe: index },
      })),
      {
        get(target, key, receiver) {
          if (typeof key === "string" && /^\d+$/.test(key)) linkEntriesRead += 1;
          return Reflect.get(target, key, receiver);
        },
      },
    );

    const eventRecord = spanToTraceRecord(makeEndedSpan({ events, links }));
    assert.equal(eventEntriesRead < 600, true);
    assert.equal(linkEntriesRead, 0);
    assert.equal(JSON.stringify(eventRecord).includes("[Truncated]"), true);

    linkEntriesRead = 0;
    const linkRecord = spanToTraceRecord(makeEndedSpan({ links }));
    assert.equal(linkEntriesRead < 600, true);
    assert.equal(JSON.stringify(linkRecord).includes("[Truncated]"), true);
  });

  it("treats unknown span collection containers conservatively without iterating them", () => {
    let reads = 0;
    const hostileContainer = new Proxy(
      {},
      {
        get() {
          reads += 1;
          throw new Error("unknown container must not be read");
        },
      },
    );
    const record = spanToTraceRecord(
      makeEndedSpan({
        attributes: hostileContainer as ReadonlyMap<string, unknown>,
        events: hostileContainer as Parameters<typeof spanToTraceRecord>[0]["events"],
        links: hostileContainer as Parameters<typeof spanToTraceRecord>[0]["links"],
      }),
    );

    assert.equal(reads, 0);
    assert.deepStrictEqual(record.attributes, { value: "[Unserializable]" });
    assert.equal(record.events[0]?.name, "[Truncated]");
    assert.equal(record.links[0]?.traceId, "[Truncated]");
  });

  it("contains hostile span entries without collapsing safe record fields", () => {
    const hostileKey = {
      toString() {
        throw new Error("hostile map key");
      },
    };
    const hostileEvent = ["event", 1n, {}] as [string, bigint, Record<string, unknown>];
    Object.defineProperty(hostileEvent, 0, {
      get() {
        throw new Error("hostile event name");
      },
    });
    const hostileLink = {
      get span(): Tracer.ExternalSpan {
        throw new Error("hostile link span");
      },
      attributes: {},
    };
    const record = spanToTraceRecord(
      makeEndedSpan({
        attributes: new Map([[hostileKey as unknown as string, "safe-value"]]),
        events: [hostileEvent],
        links: [hostileLink],
      }),
    );

    assert.equal(record.name, "bounded-span");
    assert.equal(record.attributes["[Unserializable]"], "safe-value");
    assert.equal(record.events[0]?.name, "[Unserializable]");
    assert.equal(record.links[0]?.traceId, "[Unserializable]");
  });

  it("shares exhaustion from a large Cause with the remaining record sections", () => {
    const hugeCause = new Map(
      Array.from({ length: 2_000 }, (_, index) => [`cause-${index}`, { nested: [index] }]),
    );
    const record = spanToTraceRecord(
      makeEndedSpan({
        status: {
          _tag: "Ended",
          startTime: 1n,
          endTime: 2n,
          exit: Exit.failCause(
            Cause.fromReasons([
              Cause.makeFailReason(hugeCause),
              Cause.makeDieReason(new Error("must not be normalized")),
            ]),
          ),
        },
        attributes: new Map([["safe", true]]),
      }),
    );

    assert.equal(record.exit._tag, "Failure");
    assert.equal(record.exit._tag === "Failure" && record.exit.cause.includes("[Truncated]"), true);
    assert.deepStrictEqual(record.attributes, { "[Truncated]": "[Truncated]" });
    assert.equal(record.events[0]?.name, "[Truncated]");
    assert.equal(record.links[0]?.traceId, "[Truncated]");
  });

  it("emits collection truncation entries before reading over-budget event and link slots", () => {
    const events = Array.from(
      { length: 127 },
      (_, index) =>
        [`event-${index}`, BigInt(index), {}] as [string, bigint, Record<string, unknown>],
    );
    const eventRecord = spanToTraceRecord(makeEndedSpan({ events }));
    assert.equal(eventRecord.events.at(-1)?.name, "[Truncated]");

    const emptySpan = (index: number) =>
      Tracer.externalSpan({ traceId: `trace-${index}`, spanId: `span-${index}`, sampled: true });
    const links = [
      { span: emptySpan(0), attributes: { one: 1, two: 2, three: 3 } },
      ...Array.from({ length: 125 }, (_, index) => ({
        span: emptySpan(index + 1),
        attributes: {},
      })),
    ];
    const linkRecord = spanToTraceRecord(makeEndedSpan({ links }));
    assert.equal(linkRecord.links.at(-1)?.traceId, "[Truncated]");
  });

  it("keeps legacy sink and exit consumers source- and runtime-compatible", () => {
    const records: Array<TraceRecord> = [];
    const legacySink = {
      filePath: "memory://legacy-trace.ndjson",
      push: (record: TraceRecord) => records.push(record),
      flush: Effect.void,
      close: () => Effect.void,
    } satisfies TraceSink;
    const consumeLegacyExit = (exit: EffectTraceRecord["exit"]): string => {
      switch (exit._tag) {
        case "Success":
          return "success";
        case "Interrupted":
          return exit.cause;
        case "Failure":
          return exit.cause;
        default: {
          const exhaustive: never = exit;
          return exhaustive;
        }
      }
    };

    legacySink.push(makeRecord("legacy"));
    assert.equal(records.length, 1);
    const defect = spanToTraceRecord(
      makeEndedSpan({
        status: { _tag: "Ended", startTime: 1n, endTime: 2n, exit: Exit.die("boom") },
      }),
    ).exit;
    const combined = spanToTraceRecord(
      makeEndedSpan({
        status: {
          _tag: "Ended",
          startTime: 1n,
          endTime: 2n,
          exit: Exit.failCause(
            Cause.fromReasons([Cause.makeFailReason("typed"), Cause.makeDieReason("defect")]),
          ),
        },
      }),
    ).exit;
    const empty = spanToTraceRecord(
      makeEndedSpan({
        status: { _tag: "Ended", startTime: 1n, endTime: 2n, exit: Exit.failCause(Cause.empty) },
      }),
    ).exit;

    assert.equal(defect._tag, "Failure");
    assert.equal(defect._tag === "Failure" && defect.classification, "Defect");
    assert.equal(combined._tag, "Failure");
    assert.equal(combined._tag === "Failure" && combined.classification, "Combined");
    assert.deepStrictEqual(combined._tag === "Failure" && combined.reasons, ["Failure", "Defect"]);
    assert.equal(empty._tag, "Failure");
    assert.equal(empty._tag === "Failure" && empty.classification, "Unknown");
    assert.equal(typeof consumeLegacyExit(defect), "string");
  });

  it.effect("captures sampled span behavior through an injected public trace sink", () =>
    Effect.gen(function* () {
      const { records, sink } = makeMemoryTraceSink();
      const context = (() => undefined as never) satisfies NonNullable<Tracer.Tracer["context"]>;
      const delegate = Tracer.make({
        span: (options) => new Tracer.NativeSpan(options),
        context,
      });
      const contextTracer = yield* makeLocalFileTracer({
        filePath: sink.filePath,
        maxBytes: 1,
        maxFiles: 1,
        batchWindowMs: 1,
        sink,
        delegate,
      });
      assert.equal(contextTracer.context, context);
      const tracer = yield* makeLocalFileTracer({
        filePath: sink.filePath,
        maxBytes: 1,
        maxFiles: 1,
        batchWindowMs: 1,
        sink,
      });

      const parent = Tracer.externalSpan({
        traceId: "0123456789abcdef0123456789abcdef",
        spanId: "0123456789abcdef",
        sampled: true,
      });
      const linked = Tracer.externalSpan({
        traceId: "fedcba9876543210fedcba9876543210",
        spanId: "fedcba9876543210",
        sampled: false,
      });

      yield* Effect.gen(function* () {
        const span = yield* Effect.currentSpan;
        span.attribute("visible", true);
        span.event("without-attributes", 10n);
        span.addLinks([{ span: linked, attributes: { token: "private", safe: true } }]);
      }).pipe(
        Effect.withSpan("sampled-span", { parent, kind: "client" }),
        Effect.withTracer(tracer),
      );
      yield* Effect.fail({ _tag: "ExpectedFailure" }).pipe(
        Effect.withSpan("failed-span"),
        Effect.withTracer(tracer),
        Effect.exit,
      );
      yield* Effect.die(new Error("expected defect")).pipe(
        Effect.withSpan("defect-span"),
        Effect.withTracer(tracer),
        Effect.exit,
      );
      yield* Effect.failCause(
        Cause.fromReasons([
          Cause.makeFailReason({ _tag: "CombinedFailure" }),
          Cause.makeFailReason({ _tag: "SecondCombinedFailure" }),
          Cause.makeDieReason(new Error("combined defect")),
          Cause.makeInterruptReason(),
        ]),
      ).pipe(Effect.withSpan("combined-span"), Effect.withTracer(tracer), Effect.exit);
      yield* Effect.void.pipe(
        Effect.withSpan("unsampled-span", { sampled: false }),
        Effect.withTracer(tracer),
      );
      yield* Effect.void.pipe(
        Effect.withSpan("tracing-disabled"),
        Effect.withTracerEnabled(false),
        Effect.withTracer(tracer),
      );

      assert.deepStrictEqual(
        records.map((record) => record.name),
        ["sampled-span", "failed-span", "defect-span", "combined-span"],
      );
      const sampled = records[0];
      assert.equal(sampled?.type, "effect-span");
      if (sampled?.type !== "effect-span") return;
      assert.equal(sampled.traceId, parent.traceId);
      assert.equal(sampled.parentSpanId, parent.spanId);
      assert.equal(sampled.kind, "client");
      assert.equal(sampled.attributes["visible"], true);
      assert.deepStrictEqual(sampled.events[0]?.attributes, {});
      assert.deepStrictEqual(sampled.links, [
        {
          traceId: linked.traceId,
          spanId: linked.spanId,
          attributes: { token: "[REDACTED]", safe: true },
        },
      ]);
      assert.equal(records[1]?.type === "effect-span" && records[1].exit._tag, "Failure");
      assert.equal(records[2]?.type === "effect-span" && records[2].exit._tag, "Failure");
      assert.equal(
        records[2]?.type === "effect-span" &&
          records[2].exit._tag === "Failure" &&
          records[2].exit.classification,
        "Defect",
      );
      const combined = records[3];
      assert.equal(combined?.type, "effect-span");
      if (combined?.type !== "effect-span") return;
      assert.deepStrictEqual(combined.exit, {
        _tag: "Failure",
        classification: "Combined",
        reasons: ["Failure", "Defect", "Interrupted"],
        cause: combined.exit._tag === "Failure" ? combined.exit.cause : "",
      });
    }),
  );

  it("decodes complete and partial OTLP trace payloads", () => {
    const makeSpan = (kind: number, suffix: string) => ({
      traceId: `trace-${suffix}`,
      spanId: `span-${suffix}`,
      parentSpanId: suffix === "server" ? "parent-server" : undefined,
      name: `span-${suffix}`,
      kind,
      startTimeUnixNano: "1000000",
      endTimeUnixNano: "3000000",
      attributes: [],
      events: [],
      links: [],
      status: suffix === "server" ? { code: 1, message: "complete" } : { code: 0 },
    });
    const payload = {
      resourceSpans: [
        {
          scopeSpans: [
            {
              scope: { name: "" },
              spans: [
                {
                  ...makeSpan(99, "fallback"),
                  startTimeUnixNano: "invalid-start",
                  endTimeUnixNano: "invalid-end",
                  attributes: [
                    { key: "nothing", value: null },
                    { key: "text", value: { stringValue: "visible" } },
                    { key: "enabled", value: { boolValue: false } },
                    { key: "integer", value: { intValue: "2" } },
                    { key: "double", value: { doubleValue: 2.5 } },
                    { key: "bytes", value: { bytesValue: "AQI=" } },
                    {
                      key: "array",
                      value: {
                        arrayValue: {
                          values: [{ stringValue: "first" }, { intValue: "3" }],
                        },
                      },
                    },
                    {
                      key: "object",
                      value: {
                        kvlistValue: {
                          values: [{ key: "safe", value: { stringValue: "nested" } }],
                        },
                      },
                    },
                    { key: "unknown", value: {} },
                    { key: "token", value: { stringValue: "private-token" } },
                  ],
                  events: [
                    {
                      name: "event",
                      timeUnixNano: "2000000",
                      attributes: [{ key: "prompt", value: { stringValue: "private prompt" } }],
                    },
                  ],
                  links: [
                    {
                      traceId: "linked-trace",
                      spanId: "linked-span",
                      attributes: [{ key: "safe", value: { boolValue: true } }],
                    },
                  ],
                },
              ],
            },
          ],
        },
        {
          resource: {
            attributes: [{ key: "service.name", value: { stringValue: "relay-client" } }],
          },
          scopeSpans: [
            {
              scope: {
                name: "relay-scope",
                version: "1.2.3",
                attributes: [{ key: "scope.safe", value: { boolValue: true } }],
              },
              spans: [
                makeSpan(1, "internal"),
                makeSpan(2, "server"),
                makeSpan(3, "client"),
                makeSpan(4, "producer"),
                makeSpan(5, "consumer"),
              ],
            },
            {
              scope: {
                name: undefined,
                version: 123,
                attributes: "invalid",
              },
              spans: [makeSpan(0, "invalid-scope")],
            },
          ],
        },
      ],
    } as unknown as Parameters<typeof decodeOtlpTraceRecords>[0];

    const records = decodeOtlpTraceRecords(payload);

    assert.equal(records.length, 7);
    assert.equal(records[0]?.kind, "internal");
    assert.equal(records[0]?.durationMs, 0);
    assert.deepStrictEqual(records[0]?.resourceAttributes, {});
    assert.deepStrictEqual(records[0]?.scope, { attributes: {} });
    assert.deepStrictEqual(records[0]?.status, { code: "0" });
    assert.deepStrictEqual(records[0]?.attributes, {
      nothing: null,
      text: "visible",
      enabled: false,
      integer: "2",
      double: 2.5,
      bytes: "AQI=",
      array: ["first", "3"],
      object: { safe: "nested" },
      unknown: null,
      token: "[REDACTED]",
    });
    assert.deepStrictEqual(records[0]?.events, [
      {
        name: "event",
        timeUnixNano: "2000000",
        attributes: { prompt: "[REDACTED]" },
      },
    ]);
    assert.deepStrictEqual(records[0]?.links, [
      {
        traceId: "linked-trace",
        spanId: "linked-span",
        attributes: { safe: true },
      },
    ]);
    assert.deepStrictEqual(
      records.slice(1, 6).map((record) => record.kind),
      ["internal", "server", "client", "producer", "consumer"],
    );
    assert.equal(records[2]?.parentSpanId, "parent-server");
    assert.equal(records[2]?.durationMs, 2);
    assert.deepStrictEqual(records[2]?.resourceAttributes, { "service.name": "relay-client" });
    assert.deepStrictEqual(records[2]?.scope, {
      name: "relay-scope",
      version: "1.2.3",
      attributes: { "scope.safe": true },
    });
    assert.deepStrictEqual(records[2]?.status, { code: "1", message: "complete" });
    assert.deepStrictEqual(records[6]?.scope, { attributes: {} });
  });

  it.effect("rejects non-positive and non-finite batch windows before starting a retry fiber", () =>
    Effect.gen(function* () {
      for (const batchWindowMs of [0, -1, Number.NaN]) {
        const exit = yield* Effect.scoped(
          makeTraceSink({
            filePath: "unused-invalid-trace.ndjson",
            maxBytes: 1_024,
            maxFiles: 1,
            batchWindowMs,
          }),
        ).pipe(Effect.exit);
        assert.equal(Exit.isFailure(exit), true);
        if (Exit.isSuccess(exit)) continue;
        const defect = Cause.findDefect(exit.cause);
        assert.equal(Result.isSuccess(defect), true);
        if (Result.isSuccess(defect)) {
          assert.equal(defect.success instanceof TraceSinkConfigurationError, true);
        }
      }
    }),
  );

  it.effect("accepts a tiny positive numeric batch window and closes deterministically", () =>
    Effect.scoped(
      Effect.gen(function* () {
        const sink = yield* makeTraceSink({
          filePath: "unused-tiny-window-trace.ndjson",
          maxBytes: 1_024,
          maxFiles: 1,
          batchWindowMs: 1e-7,
        });
        assert.equal(sink.filePath, "unused-tiny-window-trace.ndjson");
        yield* sink.close();
      }),
    ),
  );

  nodeServicesIt("node services", (it) => {
    it.effect("flushes buffered trace records on close", () =>
      Effect.scoped(
        Effect.gen(function* () {
          const fileSystem = yield* FileSystem.FileSystem;
          const path = yield* Path.Path;
          const tempDir = yield* fileSystem.makeTempDirectoryScoped({
            prefix: "coddy-studio-trace-sink-",
          });
          const tracePath = path.join(tempDir, "shared.trace.ndjson");

          const sink = yield* makeTraceSink({
            filePath: tracePath,
            maxBytes: 1024,
            maxFiles: 2,
            batchWindowMs: 10_000,
          });

          sink.push(makeRecord("alpha"));
          sink.push(makeRecord("beta"));
          yield* sink.close();

          const lines = yield* readTraceRecords(tracePath);

          assert.equal(lines.length, 2);
          assert.equal(lines[0]?.name, "alpha");
          assert.equal(lines[1]?.name, "beta");
        }),
      ),
    );

    it.effect("writes locally prepared span records without a second normalization traversal", () =>
      Effect.scoped(
        Effect.gen(function* () {
          const fileSystem = yield* FileSystem.FileSystem;
          const path = yield* Path.Path;
          const tempDir = yield* fileSystem.makeTempDirectoryScoped({
            prefix: "coddy-studio-trace-prepared-record-",
          });
          const tracePath = path.join(tempDir, "shared.trace.ndjson");
          let toStringCalls = 0;
          class PreparedValue {
            toString() {
              toStringCalls += 1;
              return "prepared-visible-value";
            }
          }
          const preparedValue = new PreparedValue();
          const sink = yield* makeTraceSink({
            filePath: tracePath,
            maxBytes: 1_024,
            maxFiles: 1,
            batchWindowMs: 10_000,
          });
          const tracer = yield* makeLocalFileTracer({
            filePath: tracePath,
            maxBytes: 1_024,
            maxFiles: 1,
            batchWindowMs: 10_000,
            sink,
          });

          yield* Effect.gen(function* () {
            const span = yield* Effect.currentSpan;
            span.attribute("prepared", preparedValue);
          }).pipe(Effect.withSpan("prepared-span"), Effect.withTracer(tracer));
          yield* sink.close();

          const records = yield* readTraceRecords(tracePath);
          assert.equal(toStringCalls, 1);
          assert.equal(records.length, 1);
          assert.equal(records[0]?.name, "prepared-span");
          assert.deepStrictEqual(records[0]?.attributes, {
            prepared: "prepared-visible-value",
          });
        }),
      ),
    );

    it.effect("shares concurrent and repeated close completion", () =>
      Effect.scoped(
        Effect.gen(function* () {
          const fileSystem = yield* FileSystem.FileSystem;
          const path = yield* Path.Path;
          const tempDir = yield* fileSystem.makeTempDirectoryScoped({
            prefix: "coddy-studio-trace-concurrent-close-",
          });
          const tracePath = path.join(tempDir, "shared.trace.ndjson");
          const sink = yield* makeTraceSink({
            filePath: tracePath,
            maxBytes: 1_024,
            maxFiles: 1,
            batchWindowMs: 10_000,
          });
          sink.push(makeRecord("concurrent-close"));

          yield* Effect.all([sink.close(), sink.close()], { concurrency: "unbounded" });
          yield* sink.close();

          const records = yield* readTraceRecords(tracePath);
          assert.deepStrictEqual(
            records.map((record) => record.name),
            ["concurrent-close"],
          );
        }),
      ),
    );

    it.effect("does not lose finalization when the first close caller is interrupted", () =>
      Effect.scoped(
        Effect.gen(function* () {
          const fileSystem = yield* FileSystem.FileSystem;
          const path = yield* Path.Path;
          const tempDir = yield* fileSystem.makeTempDirectoryScoped({
            prefix: "coddy-studio-trace-interrupted-close-",
          });
          const tracePath = path.join(tempDir, "shared.trace.ndjson");

          yield* Effect.scoped(
            Effect.gen(function* () {
              const sink = yield* makeTraceSink({
                filePath: tracePath,
                maxBytes: 1_024,
                maxFiles: 1,
                batchWindowMs: 10_000,
              });
              sink.push(makeRecord("survives-interrupted-close"));
              const firstClose = yield* Effect.forkChild(sink.close());
              yield* Effect.yieldNow;
              yield* Fiber.interrupt(firstClose);
              yield* sink.close();
            }),
          );

          const records = yield* readTraceRecords(tracePath);
          assert.deepStrictEqual(
            records.map((record) => record.name),
            ["survives-interrupted-close"],
          );
        }),
      ),
    );

    it.effect("makes rollback failure terminal and never replays the uncertain batch", () =>
      Effect.scoped(
        Effect.gen(function* () {
          const fileSystem = yield* FileSystem.FileSystem;
          const path = yield* Path.Path;
          const tempDir = yield* fileSystem.makeTempDirectoryScoped({
            prefix: "coddy-studio-trace-terminal-rollback-",
          });
          const tracePath = path.join(tempDir, "shared.trace.ndjson");
          const diagnostics: Array<TraceSinkDiagnostic> = [];
          let writes = 0;
          const injected = {
            mkdirSync: NodeFS.mkdirSync,
            statSync: NodeFS.statSync,
            openSync: NodeFS.openSync,
            fstatSync: NodeFS.fstatSync,
            writeSync: ((
              fd: number,
              buffer: Uint8Array,
              offset: number,
              length: number,
              position: number | null,
            ) => {
              writes += 1;
              if (writes === 1) {
                return NodeFS.writeSync(fd, buffer, offset, Math.min(8, length), position ?? null);
              }
              throw new Error("injected terminal write failure");
            }) as unknown as typeof NodeFS.writeSync,
            ftruncateSync: (() => {
              throw new Error("injected terminal rollback failure");
            }) as typeof NodeFS.ftruncateSync,
            closeSync: NodeFS.closeSync,
            existsSync: NodeFS.existsSync,
            rmSync: NodeFS.rmSync,
            renameSync: NodeFS.renameSync,
            readdirSync: NodeFS.readdirSync,
          } satisfies RotatingFileSinkFileSystem;
          const sink = yield* makeTraceSink({
            filePath: tracePath,
            maxBytes: 1_024,
            maxFiles: 1,
            batchWindowMs: 10_000,
            fileSystem: injected,
            onDiagnostic: (diagnostic) => diagnostics.push(diagnostic),
          });

          sink.push(makeRecord("terminal", "private-token-value"));
          yield* sink.flush;
          sink.push(makeRecord("after-terminal"));
          yield* sink.flush;
          yield* sink.close();
          yield* sink.close();

          assert.equal(writes, 2);
          assert.deepStrictEqual(sink.stats(), {
            pendingRecords: 0,
            pendingBytes: 0,
            droppedRecords: 2,
            consecutiveFailures: 1,
            maxPendingRecords: DEFAULT_MAX_PENDING_TRACE_RECORDS,
            maxPendingBytes: DEFAULT_MAX_PENDING_TRACE_BYTES,
          });
          assert.equal(diagnostics.length, 1);
          assert.equal(diagnostics[0]?.event, "sink-terminal-failure");
          assert.equal(
            diagnostics.some((diagnostic) =>
              Object.values(diagnostic).some(
                (value) => typeof value === "string" && value.includes("private-token-value"),
              ),
            ),
            false,
          );
          assert.equal((yield* fileSystem.readFileString(tracePath)).length, 8);
        }),
      ),
    );

    it.effect(
      "makes descriptor close failure terminal and never retries the uncertain append",
      () =>
        Effect.scoped(
          Effect.gen(function* () {
            const fileSystem = yield* FileSystem.FileSystem;
            const path = yield* Path.Path;
            const tempDir = yield* fileSystem.makeTempDirectoryScoped({
              prefix: "coddy-studio-trace-terminal-close-",
            });
            const tracePath = path.join(tempDir, "shared.trace.ndjson");
            const diagnostics: Array<TraceSinkDiagnostic> = [];
            let writes = 0;
            let closes = 0;
            const injected = {
              mkdirSync: NodeFS.mkdirSync,
              statSync: NodeFS.statSync,
              openSync: NodeFS.openSync,
              fstatSync: NodeFS.fstatSync,
              writeSync: ((...args: Parameters<typeof NodeFS.writeSync>) => {
                writes += 1;
                return (NodeFS.writeSync as (...values: typeof args) => number)(...args);
              }) as typeof NodeFS.writeSync,
              ftruncateSync: NodeFS.ftruncateSync,
              closeSync: ((fd) => {
                closes += 1;
                NodeFS.closeSync(fd);
                throw new Error("injected close failure after release");
              }) as typeof NodeFS.closeSync,
              existsSync: NodeFS.existsSync,
              rmSync: NodeFS.rmSync,
              renameSync: NodeFS.renameSync,
              readdirSync: NodeFS.readdirSync,
            } satisfies RotatingFileSinkFileSystem;
            const sink = yield* makeTraceSink({
              filePath: tracePath,
              maxBytes: 1_024,
              maxFiles: 1,
              batchWindowMs: 10_000,
              fileSystem: injected,
              onDiagnostic: (diagnostic) => diagnostics.push(diagnostic),
            });

            sink.push(makeRecord("terminal-close"));
            yield* sink.flush;
            yield* sink.flush;
            yield* Effect.all([sink.close(), sink.close()], { concurrency: "unbounded" });

            assert.equal(writes, 1);
            assert.equal(closes, 1);
            assert.equal(
              diagnostics.filter((diagnostic) => diagnostic.event === "sink-terminal-failure")
                .length,
              1,
            );
            assert.deepStrictEqual(sink.stats(), {
              pendingRecords: 0,
              pendingBytes: 0,
              droppedRecords: 1,
              consecutiveFailures: 1,
              maxPendingRecords: DEFAULT_MAX_PENDING_TRACE_RECORDS,
              maxPendingBytes: DEFAULT_MAX_PENDING_TRACE_BYTES,
            });
            const records = yield* readTraceRecords(tracePath);
            assert.deepStrictEqual(
              records.map((record) => record.name),
              ["terminal-close"],
            );
          }),
        ),
    );

    it.effect("flushes a full batch immediately", () =>
      Effect.scoped(
        Effect.gen(function* () {
          const fileSystem = yield* FileSystem.FileSystem;
          const path = yield* Path.Path;
          const tempDir = yield* fileSystem.makeTempDirectoryScoped({
            prefix: "coddy-studio-trace-batch-",
          });
          const tracePath = path.join(tempDir, "shared.trace.ndjson");
          const sink = yield* makeTraceSink({
            filePath: tracePath,
            maxBytes: 1024 * 1024,
            maxFiles: 2,
            batchWindowMs: 10_000,
          });

          for (let index = 0; index < 32; index += 1) {
            sink.push(makeRecord("batch", String(index)));
          }

          assert.equal((yield* readTraceRecords(tracePath)).length, 32);
        }),
      ),
    );

    it.effect("retains buffered records when a write fails and retries them", () =>
      Effect.scoped(
        Effect.gen(function* () {
          const fileSystem = yield* FileSystem.FileSystem;
          const path = yield* Path.Path;
          const tempDir = yield* fileSystem.makeTempDirectoryScoped({
            prefix: "coddy-studio-trace-retry-",
          });
          const parentPath = path.join(tempDir, "trace-parent");
          const tracePath = path.join(parentPath, "shared.trace.ndjson");
          const sink = yield* makeTraceSink({
            filePath: tracePath,
            maxBytes: 1024,
            maxFiles: 2,
            batchWindowMs: 10_000,
          });
          sink.push(makeRecord("retry"));

          yield* fileSystem.remove(parentPath, { recursive: true });
          yield* fileSystem.writeFileString(parentPath, "blocks child path");
          yield* sink.flush;
          yield* fileSystem.remove(parentPath);
          yield* fileSystem.makeDirectory(parentPath);
          yield* sink.flush;

          assert.deepStrictEqual(
            (yield* readTraceRecords(tracePath)).map((record) => record.name),
            ["retry"],
          );
        }),
      ),
    );

    it.effect(
      "bounds failed records, reports failures safely, and recovers without duplicates",
      () =>
        Effect.scoped(
          Effect.gen(function* () {
            const fileSystem = yield* FileSystem.FileSystem;
            const path = yield* Path.Path;
            const tempDir = yield* fileSystem.makeTempDirectoryScoped({
              prefix: "coddy-studio-trace-bounded-retry-",
            });
            const parentPath = path.join(tempDir, "trace-parent");
            const tracePath = path.join(parentPath, "shared.trace.ndjson");
            const diagnostics: Array<TraceSinkDiagnostic> = [];
            const sink = yield* makeTraceSink({
              filePath: tracePath,
              maxBytes: 1024 * 1024,
              maxFiles: 2,
              batchWindowMs: 10_000,
              maxPendingRecords: 3,
              onDiagnostic: (diagnostic) => diagnostics.push(diagnostic),
            });

            yield* fileSystem.remove(parentPath, { recursive: true });
            yield* fileSystem.writeFileString(parentPath, "blocks child path");
            for (let index = 0; index < 5; index += 1) {
              sink.push(makeRecord(`record-${index}`, `private-token-${index}`));
            }

            const bufferedStats = sink.stats();
            assert.equal(bufferedStats.pendingBytes > 0, true);
            assert.deepStrictEqual(bufferedStats, {
              pendingRecords: 3,
              pendingBytes: bufferedStats.pendingBytes,
              droppedRecords: 2,
              consecutiveFailures: 0,
              maxPendingRecords: 3,
              maxPendingBytes: DEFAULT_MAX_PENDING_TRACE_BYTES,
            });

            yield* Effect.all([sink.flush, sink.flush, sink.flush], { concurrency: "unbounded" });

            const failedStats = sink.stats();
            assert.equal(failedStats.pendingRecords, 3);
            assert.equal(failedStats.droppedRecords, 2);
            assert.equal(failedStats.consecutiveFailures >= 2, true);
            assert.equal(
              diagnostics.some((diagnostic) =>
                Object.values(diagnostic).some(
                  (value) => typeof value === "string" && value.includes("private-token"),
                ),
              ),
              false,
            );
            assert.equal(
              diagnostics.some(
                (diagnostic) =>
                  typeof diagnostic === "object" &&
                  diagnostic !== null &&
                  "event" in diagnostic &&
                  diagnostic.event === "sink-failure",
              ),
              true,
            );

            yield* fileSystem.remove(parentPath);
            yield* fileSystem.makeDirectory(parentPath);
            yield* Effect.all([sink.flush, sink.flush, sink.flush], { concurrency: "unbounded" });

            assert.deepStrictEqual(
              (yield* readTraceRecords(tracePath)).map((record) => record.name),
              ["record-2", "record-3", "record-4"],
            );
            assert.deepStrictEqual(sink.stats(), {
              pendingRecords: 0,
              pendingBytes: 0,
              droppedRecords: 2,
              consecutiveFailures: 0,
              maxPendingRecords: 3,
              maxPendingBytes: DEFAULT_MAX_PENDING_TRACE_BYTES,
            });
            yield* sink.close();
            yield* sink.close();
          }),
        ),
    );

    it.effect("enforces record and byte caps across concurrent pushes and ordered recovery", () =>
      Effect.scoped(
        Effect.gen(function* () {
          const fileSystem = yield* FileSystem.FileSystem;
          const path = yield* Path.Path;
          const tempDir = yield* fileSystem.makeTempDirectoryScoped({
            prefix: "coddy-studio-trace-byte-bound-",
          });
          const parentPath = path.join(tempDir, "trace-parent");
          const tracePath = path.join(parentPath, "shared.trace.ndjson");
          const diagnostics: Array<TraceSinkDiagnostic> = [];
          const sink = yield* makeTraceSink({
            filePath: tracePath,
            maxBytes: 1024 * 1024,
            maxFiles: 2,
            batchWindowMs: 10_000,
            maxPendingRecords: 4,
            maxPendingBytes: 900,
            onDiagnostic: (diagnostic) => diagnostics.push(diagnostic),
          });

          yield* fileSystem.remove(parentPath, { recursive: true });
          yield* fileSystem.writeFileString(parentPath, "blocks child path");
          yield* Effect.all(
            Array.from({ length: 10 }, (_, index) =>
              Effect.sync(() => sink.push(makeRecord(`byte-${index}`, `payload-${index}`))),
            ),
            { concurrency: "unbounded" },
          );

          const failedStats = sink.stats();
          assert.equal(failedStats.pendingRecords > 0, true);
          assert.equal(failedStats.pendingRecords <= 4, true);
          assert.equal(failedStats.pendingBytes > 0, true);
          assert.equal(failedStats.pendingBytes <= 900, true);
          assert.equal(failedStats.maxPendingRecords, 4);
          assert.equal(failedStats.maxPendingBytes, 900);
          assert.equal(failedStats.droppedRecords, 10 - failedStats.pendingRecords);
          const overflow = diagnostics.find((diagnostic) => diagnostic.event === "buffer-overflow");
          assert.equal(overflow?.maxPendingRecords, 4);
          assert.equal(overflow?.maxPendingBytes, 900);
          assert.equal((overflow?.pendingBytes ?? 901) <= 900, true);

          yield* Effect.all([sink.flush, sink.flush], { concurrency: "unbounded" });
          const retainedCount = sink.stats().pendingRecords;
          assert.equal(retainedCount, failedStats.pendingRecords);
          yield* fileSystem.remove(parentPath);
          yield* fileSystem.makeDirectory(parentPath);
          yield* Effect.all([sink.flush, sink.flush], { concurrency: "unbounded" });

          const names = (yield* readTraceRecords(tracePath)).map((record) => record.name);
          assert.deepStrictEqual(
            names,
            Array.from(
              { length: retainedCount },
              (_, index) => `byte-${10 - retainedCount + index}`,
            ),
          );
          assert.equal(new Set(names).size, names.length);
          assert.equal(sink.stats().pendingBytes, 0);
        }),
      ),
    );

    it.effect("rejects oversized records without recursive diagnostics", () =>
      Effect.scoped(
        Effect.gen(function* () {
          const fileSystem = yield* FileSystem.FileSystem;
          const path = yield* Path.Path;
          const tempDir = yield* fileSystem.makeTempDirectoryScoped({
            prefix: "coddy-studio-trace-oversized-record-",
          });
          const tracePath = path.join(tempDir, "shared.trace.ndjson");
          const diagnostics: Array<TraceSinkDiagnostic> = [];
          let sinkReference: TraceSink | undefined;
          let lateGetterReads = 0;
          const hugeArray: Array<unknown> = Array.from({ length: 2_000 }, (_, index) => index);
          Object.defineProperty(hugeArray, 1_500, {
            get() {
              lateGetterReads += 1;
              return "must-not-be-read";
            },
          });
          const oversizedRecord: TraceRecord = {
            ...makeRecord("oversized-record"),
            attributes: { hugeArray },
          };
          const sink = yield* makeTraceSink({
            filePath: tracePath,
            maxBytes: 1024,
            maxFiles: 2,
            batchWindowMs: 10_000,
            maxPendingRecords: 8,
            maxPendingBytes: 64,
            onDiagnostic(diagnostic) {
              diagnostics.push(diagnostic);
              sinkReference?.push(oversizedRecord);
              throw new Error("diagnostic callback failure");
            },
          });
          sinkReference = sink;

          sink.push(oversizedRecord);

          assert.equal(lateGetterReads, 0);
          assert.deepStrictEqual(sink.stats(), {
            pendingRecords: 0,
            pendingBytes: 0,
            droppedRecords: 2,
            consecutiveFailures: 0,
            maxPendingRecords: 8,
            maxPendingBytes: 64,
          });
          assert.equal(diagnostics.length, 1);
          assert.equal(diagnostics[0]?.event, "record-too-large");
          assert.equal(diagnostics[0]?.maxPendingRecords, 8);
          assert.equal(diagnostics[0]?.maxPendingBytes, 64);
        }),
      ),
    );

    it.effect("does not tight-loop automatic retries after a persistent sink failure", () =>
      Effect.scoped(
        Effect.gen(function* () {
          const fileSystem = yield* FileSystem.FileSystem;
          const path = yield* Path.Path;
          const tempDir = yield* fileSystem.makeTempDirectoryScoped({
            prefix: "coddy-studio-trace-persistent-failure-",
          });
          const parentPath = path.join(tempDir, "trace-parent");
          const tracePath = path.join(parentPath, "shared.trace.ndjson");
          const diagnostics: Array<TraceSinkDiagnostic> = [];
          const sink = yield* makeTraceSink({
            filePath: tracePath,
            maxBytes: 1024 * 1024,
            maxFiles: 2,
            batchWindowMs: 10_000,
            maxPendingRecords: 40,
            onDiagnostic: (diagnostic) => diagnostics.push(diagnostic),
          });

          yield* fileSystem.remove(parentPath, { recursive: true });
          yield* fileSystem.writeFileString(parentPath, "blocks child path");
          for (let index = 0; index < 41; index += 1) {
            sink.push(makeRecord(`persistent-${index}`));
          }

          assert.equal(
            diagnostics.filter((diagnostic) => diagnostic.event === "sink-failure").length,
            1,
          );
          const persistentStats = sink.stats();
          assert.equal(persistentStats.pendingBytes > 0, true);
          assert.deepStrictEqual(persistentStats, {
            pendingRecords: 40,
            pendingBytes: persistentStats.pendingBytes,
            droppedRecords: 1,
            consecutiveFailures: 1,
            maxPendingRecords: 40,
            maxPendingBytes: DEFAULT_MAX_PENDING_TRACE_BYTES,
          });

          yield* sink.close();
          yield* sink.close();
          sink.push(makeRecord("after-close"));
          yield* sink.flush;
          assert.equal(
            diagnostics.filter((diagnostic) => diagnostic.event === "sink-failure").length,
            2,
          );
          assert.equal(sink.stats().pendingRecords, 40);
          assert.equal(sink.stats().droppedRecords, 2);
        }),
      ),
    );

    it.effect("rotates the trace file when the configured max size is exceeded", () =>
      Effect.scoped(
        Effect.gen(function* () {
          const fileSystem = yield* FileSystem.FileSystem;
          const path = yield* Path.Path;
          const tempDir = yield* fileSystem.makeTempDirectoryScoped({
            prefix: "coddy-studio-trace-sink-",
          });
          const tracePath = path.join(tempDir, "shared.trace.ndjson");

          const sink = yield* makeTraceSink({
            filePath: tracePath,
            maxBytes: 180,
            maxFiles: 2,
            batchWindowMs: 10_000,
          });

          for (let index = 0; index < 8; index += 1) {
            sink.push(makeRecord("rotate", `${index}-${"x".repeat(48)}`));
            yield* sink.flush;
          }
          yield* sink.close();

          const matchingFiles = Arr.sort(
            (yield* fileSystem.readDirectory(tempDir)).filter(
              (entry) =>
                entry === "shared.trace.ndjson" || entry.startsWith("shared.trace.ndjson."),
            ),
            Order.String,
          );

          assert.equal(
            matchingFiles.some((entry) => entry === "shared.trace.ndjson.1"),
            true,
          );
          assert.equal(
            matchingFiles.some((entry) => entry === "shared.trace.ndjson.3"),
            false,
          );
        }),
      ),
    );

    it.effect("normalizes circular trace records before serialization", () =>
      Effect.scoped(
        Effect.gen(function* () {
          const fileSystem = yield* FileSystem.FileSystem;
          const path = yield* Path.Path;
          const tempDir = yield* fileSystem.makeTempDirectoryScoped({
            prefix: "coddy-studio-trace-sink-",
          });
          const tracePath = path.join(tempDir, "shared.trace.ndjson");

          const circular: Array<unknown> = [];
          circular.push(circular);
          const invalidRecord = {
            ...makeRecord("invalid"),
            attributes: { circular },
          } as TraceRecord;
          const sink = yield* makeTraceSink({
            filePath: tracePath,
            maxBytes: 1024,
            maxFiles: 2,
            batchWindowMs: 10_000,
            maxPendingRecords: 0,
            maxPendingBytes: 0,
          });

          sink.push(makeRecord("alpha"));
          sink.push(invalidRecord);
          sink.push(makeRecord("beta"));
          yield* sink.close();

          const lines = yield* readTraceRecords(tracePath);

          assert.deepStrictEqual(
            lines.map((line) => line.name),
            ["alpha", "invalid", "beta"],
          );
          assert.deepStrictEqual(lines[1]?.attributes["circular"], ["[Circular]"]);
          assert.equal(sink.stats().maxPendingRecords, DEFAULT_MAX_PENDING_TRACE_RECORDS);
          assert.equal(sink.stats().maxPendingBytes, DEFAULT_MAX_PENDING_TRACE_BYTES);
        }),
      ),
    );

    it.effect("writes nested spans to disk and captures log messages as span events", () =>
      Effect.scoped(
        Effect.gen(function* () {
          const fileSystem = yield* FileSystem.FileSystem;
          const path = yield* Path.Path;
          const tempDir = yield* fileSystem.makeTempDirectoryScoped({
            prefix: "coddy-studio-local-tracer-",
          });
          const tracePath = path.join(tempDir, "shared.trace.ndjson");

          yield* Effect.scoped(
            Effect.gen(function* () {
              const program = Effect.gen(function* () {
                yield* Effect.annotateCurrentSpan({
                  "demo.parent": true,
                });
                yield* Effect.logInfo("parent event");
                yield* Effect.gen(function* () {
                  yield* Effect.annotateCurrentSpan({
                    "demo.child": true,
                  });
                  yield* Effect.logInfo("child event");
                }).pipe(Effect.withSpan("child-span"));
              }).pipe(Effect.withSpan("parent-span"));

              yield* program.pipe(Effect.provide(makeTestLayer(tracePath)));
            }),
          );

          const records = yield* readTraceRecords(tracePath);
          assert.equal(records.length, 2);

          const parent = records.find((record) => record.name === "parent-span");
          const child = records.find((record) => record.name === "child-span");

          assert.notEqual(parent, undefined);
          assert.notEqual(child, undefined);
          if (!parent || !child) {
            return;
          }

          assert.equal(child.parentSpanId, parent.spanId);
          assert.equal(parent.attributes["demo.parent"], true);
          assert.equal(child.attributes["demo.child"], true);
          assert.equal(
            parent.events.some((event) => event.name === "parent event"),
            true,
          );
          assert.equal(
            child.events.some((event) => event.name === "child event"),
            true,
          );
          assert.equal(
            child.events.some((event) => event.attributes["effect.logLevel"] === "INFO"),
            true,
          );
        }),
      ),
    );

    it.effect(
      "captures logger severity defaults and overrides without leaking structured values",
      () =>
        Effect.scoped(
          Effect.gen(function* () {
            const fileSystem = yield* FileSystem.FileSystem;
            const path = yield* Path.Path;
            const tempDir = yield* fileSystem.makeTempDirectoryScoped({
              prefix: "coddy-studio-local-logger-",
            });
            const tracePath = path.join(tempDir, "shared.trace.ndjson");
            const secret = "logger-private-value";
            const urlPassword = "logger-url-password";
            const cookieSecret = "logger-cookie-secret";

            yield* Effect.scoped(
              Effect.gen(function* () {
                yield* Effect.logTrace("trace event");
                yield* Effect.logDebug("debug event");
                yield* Effect.logInfo("info event", { safe: true, token: secret });
                yield* Effect.logWarning("warn event");
                yield* Effect.logError(`error event token=${secret}`);
                yield* Effect.logFatal("fatal event");
                yield* Effect.logInfo("url event", {
                  url: new URL(
                    `https://reader:${urlPassword}@logger.test/path?access_token=${secret}&safe=visible`,
                  ),
                  params: new URLSearchParams(`refresh_token=${secret}&ordinary=tokenization`),
                  Cookie: `session=${cookieSecret}`,
                });
              }).pipe(
                Effect.withSpan("all-levels"),
                Effect.provide(makeTestLayer(tracePath, "Trace")),
              ),
            );
            yield* Effect.scoped(
              Effect.gen(function* () {
                yield* Effect.logDebug("filtered debug");
                yield* Effect.logInfo("default info");
              }).pipe(Effect.withSpan("default-level"), Effect.provide(makeTestLayer(tracePath))),
            );

            const serialized = yield* fileSystem.readFileString(tracePath);
            const records = yield* readTraceRecords(tracePath);
            const allLevels = records.find((record) => record.name === "all-levels");
            const defaultLevel = records.find((record) => record.name === "default-level");
            assert.notEqual(allLevels, undefined);
            assert.notEqual(defaultLevel, undefined);
            assert.deepStrictEqual(
              allLevels?.events.map((event) => event.attributes["effect.logLevel"]),
              ["TRACE", "DEBUG", "INFO", "WARN", "ERROR", "FATAL", "INFO"],
            );
            assert.deepStrictEqual(
              defaultLevel?.events.map((event) => event.attributes["effect.logLevel"]),
              ["INFO"],
            );
            assert.equal(serialized.includes(secret), false);
            assert.equal(serialized.includes(urlPassword), false);
            assert.equal(serialized.includes(cookieSecret), false);
            assert.equal(serialized.includes("[REDACTED]"), true);
            assert.equal(serialized.includes("logger.test/path"), true);
            assert.equal(serialized.includes("safe=visible"), true);
          }),
        ),
    );

    it.effect("serializes interrupted spans with an interrupted exit status", () =>
      Effect.scoped(
        Effect.gen(function* () {
          const fileSystem = yield* FileSystem.FileSystem;
          const path = yield* Path.Path;
          const tempDir = yield* fileSystem.makeTempDirectoryScoped({
            prefix: "coddy-studio-local-tracer-",
          });
          const tracePath = path.join(tempDir, "shared.trace.ndjson");

          yield* Effect.scoped(
            Effect.exit(
              Effect.interrupt.pipe(
                Effect.withSpan("interrupt-span"),
                Effect.provide(makeTestLayer(tracePath)),
              ),
            ),
          );

          const records = yield* readTraceRecords(tracePath);
          assert.equal(records.length, 1);
          assert.equal(records[0]?.name, "interrupt-span");
          assert.equal(records[0]?.exit?._tag, "Interrupted");
        }),
      ),
    );
  });
});
