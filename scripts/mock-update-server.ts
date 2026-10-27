// @effect-diagnostics nodeBuiltinImport:off
import * as NodeFS from "node:fs";
import * as NodeHttp from "node:http";
import * as NodePath from "node:path";

import * as NodeHttpServer from "@effect/platform-node/NodeHttpServer";
import * as NodeRuntime from "@effect/platform-node/NodeRuntime";
import * as NodeServices from "@effect/platform-node/NodeServices";
import * as NodeStream from "@effect/platform-node/NodeStream";
import * as Config from "effect/Config";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Path from "effect/Path";
import { HttpRouter, HttpServerRequest, HttpServerResponse } from "effect/http";

export interface MockUpdateServerConfig {
  readonly port: number;
  readonly requestLogPath?: string;
  readonly rootRealPath: string;
}

export const MOCK_UPDATE_READY_PATH = "/.well-known/coddy-studio-updater-ready";
export const MOCK_UPDATE_LOOPBACK_HOST = "127.0.0.1";

export const makeResolveMockUpdateServerConfig = (scriptDirectory: string) =>
  Effect.gen(function* () {
    const fileSystem = yield* FileSystem.FileSystem;
    const path = yield* Path.Path;
    const config = yield* Config.all({
      port: Config.Port("CODDY_STUDIO_DESKTOP_MOCK_UPDATE_SERVER_PORT").pipe(Config.withDefault(3000)),
      requestLogPath: Config.String("CODDY_STUDIO_DESKTOP_MOCK_UPDATE_SERVER_REQUEST_LOG").pipe(
        Config.option,
      ),
      root: Config.String("CODDY_STUDIO_DESKTOP_MOCK_UPDATE_SERVER_ROOT").pipe(
        Config.withDefault("../release-mock"),
      ),
    });

    const resolvedRoot = path.resolve(scriptDirectory, config.root);

    return {
      port: config.port,
      ...(config.requestLogPath._tag === "Some"
        ? { requestLogPath: path.resolve(config.requestLogPath.value) }
        : {}),
      rootRealPath: yield* fileSystem.realPath(resolvedRoot),
    } satisfies MockUpdateServerConfig;
  });

export const resolveMockUpdateServerConfig = makeResolveMockUpdateServerConfig(import.meta.dirname);

const isOutsideRoot = (rootRealPath: string, filePath: string) =>
  Effect.gen(function* () {
    const path = yield* Path.Path;
    const relativePath = path.relative(rootRealPath, filePath);
    return (
      relativePath === ".." || relativePath.startsWith("../") || relativePath.startsWith("..\\")
    );
  });

export const resolveRequestedFilePath = (rootRealPath: string, requestUrl: string | undefined) =>
  Effect.gen(function* () {
    const path = yield* Path.Path;
    const rawPath = (requestUrl ?? "/").split("?", 1)[0]!;
    const decodedPath = yield* Effect.try({
      try: () => decodeURIComponent(rawPath),
      catch: () => null,
    }).pipe(
      Effect.match({
        onFailure: () => undefined,
        onSuccess: (value) => value,
      }),
    );

    if (!decodedPath) {
      return undefined;
    }

    if (decodedPath.includes("\0")) {
      return undefined;
    }

    const filePath = path.resolve(
      rootRealPath,
      `.${decodedPath.startsWith("/") ? decodedPath : `/${decodedPath}`}`,
    );

    return (yield* isOutsideRoot(rootRealPath, filePath)) ? undefined : filePath;
  });

export type ByteRange =
  | { readonly _tag: "Full" }
  | { readonly _tag: "Partial"; readonly start: number; readonly end: number }
  | { readonly _tag: "Unsatisfiable" };

export function formatMockUpdateRequestLogEntry(input: {
  readonly method: string;
  readonly range?: string | undefined;
  readonly requestUrl: string | undefined;
}): string {
  const path = (input.requestUrl ?? "/").split("?", 1)[0]!;
  return JSON.stringify({
    method: input.method,
    path,
    ...(input.range === undefined ? {} : { range: input.range }),
  });
}

export interface UpdateFileStat {
  readonly dev: bigint;
  readonly ino: bigint;
  readonly size: bigint;
  isFile(): boolean;
}

export interface UpdateFileHandle {
  stat(options: { readonly bigint: true }): Promise<UpdateFileStat>;
  createReadStream(options: {
    readonly autoClose: true;
    readonly start?: number;
    readonly end?: number;
  }): NodeJS.ReadableStream;
  close(): Promise<void>;
}

export interface UpdateFileOpenDependencies {
  readonly realPath?: ((filePath: string) => Promise<string>) | undefined;
  readonly stat?: ((filePath: string) => Promise<UpdateFileStat>) | undefined;
  readonly open?: ((filePath: string, flags: number) => Promise<UpdateFileHandle>) | undefined;
}

export interface OpenedUpdateFile {
  readonly size: number;
  close(): Promise<void>;
  stream(
    offset: number,
    bytesToRead: number,
  ): ReturnType<typeof NodeStream.fromReadable<Uint8Array>>;
}

const nodePathIsWithin = (root: string, candidate: string): boolean => {
  const relative = NodePath.relative(root, candidate);
  return (
    relative === "" ||
    (relative !== ".." &&
      !relative.startsWith("../") &&
      !relative.startsWith("..\\") &&
      !NodePath.isAbsolute(relative))
  );
};

const sameFileIdentity = (expected: UpdateFileStat, opened: UpdateFileStat): boolean =>
  expected.isFile() &&
  opened.isFile() &&
  expected.dev !== 0n &&
  expected.ino !== 0n &&
  expected.dev === opened.dev &&
  expected.ino === opened.ino &&
  expected.size === opened.size;

export const openValidatedUpdateFile = (
  rootRealPath: string,
  filePath: string,
  dependencies: UpdateFileOpenDependencies = {},
): Effect.Effect<OpenedUpdateFile | undefined> =>
  Effect.promise(async () => {
    const realPath = dependencies.realPath ?? NodeFS.promises.realpath;
    const stat =
      dependencies.stat ?? ((target: string) => NodeFS.promises.stat(target, { bigint: true }));
    const open =
      dependencies.open ?? ((target: string, flags: number) => NodeFS.promises.open(target, flags));

    try {
      const canonicalBeforeOpen = await realPath(filePath);
      if (!nodePathIsWithin(rootRealPath, canonicalBeforeOpen)) return undefined;
      const expectedStat = await stat(canonicalBeforeOpen);
      if (!expectedStat.isFile()) return undefined;

      const noFollow = NodeFS.constants.O_NOFOLLOW ?? 0;
      const handle = await open(filePath, NodeFS.constants.O_RDONLY | noFollow);
      let accepted = false;
      try {
        const openedStat = await handle.stat({ bigint: true });
        const canonicalAfterOpen = await realPath(filePath);
        if (
          canonicalAfterOpen !== canonicalBeforeOpen ||
          !nodePathIsWithin(rootRealPath, canonicalAfterOpen) ||
          !sameFileIdentity(expectedStat, openedStat)
        ) {
          return undefined;
        }
        const size = Number(openedStat.size);
        if (!Number.isSafeInteger(size) || size < 0) return undefined;
        accepted = true;
        return {
          size,
          close: () => handle.close(),
          stream: (offset, bytesToRead) => {
            const readable = handle.createReadStream({
              autoClose: true,
              ...(offset > 0 ? { start: offset } : {}),
              ...(bytesToRead < size ? { end: offset + bytesToRead - 1 } : {}),
            });
            return NodeStream.fromReadable<Uint8Array>({ evaluate: () => readable });
          },
        };
      } finally {
        if (!accepted) await handle.close().catch(() => undefined);
      }
    } catch {
      return undefined;
    }
  });

const contentTypeForUpdateFile = (filePath: string): string => {
  const extension = NodePath.extname(filePath).toLowerCase();
  if (extension === ".yml" || extension === ".yaml") return "text/yaml";
  if (extension === ".json") return "application/json";
  return "application/octet-stream";
};

const makeOpenedFileStream = (openedFile: OpenedUpdateFile, offset: number, bytesToRead: number) =>
  Effect.sync(() => openedFile.stream(offset, bytesToRead)).pipe(
    Effect.onError(() => Effect.promise(() => openedFile.close()).pipe(Effect.ignore)),
  );

export function resolveByteRange(rangeHeader: string | undefined, size: number): ByteRange {
  if (rangeHeader === undefined) return { _tag: "Full" };
  const match = /^bytes=(\d*)-(\d*)$/.exec(rangeHeader.trim());
  if (!match || (match[1] === "" && match[2] === "")) return { _tag: "Full" };
  if (size <= 0) return { _tag: "Unsatisfiable" };

  if (match[1] === "") {
    const suffixLength = Number(match[2]);
    if (!Number.isSafeInteger(suffixLength) || suffixLength <= 0) return { _tag: "Full" };
    return { _tag: "Partial", start: Math.max(0, size - suffixLength), end: size - 1 };
  }

  const start = Number(match[1]);
  const requestedEnd = match[2] === "" ? size - 1 : Number(match[2]);
  if (
    !Number.isSafeInteger(start) ||
    !Number.isSafeInteger(requestedEnd) ||
    start < 0 ||
    start >= size ||
    requestedEnd < start
  ) {
    return { _tag: "Unsatisfiable" };
  }
  return { _tag: "Partial", start, end: Math.min(requestedEnd, size - 1) };
}

export const makeMockUpdateRouteLayer = (
  rootRealPath: string,
  openFile: typeof openValidatedUpdateFile = openValidatedUpdateFile,
  requestLogPath?: string,
) => {
  return HttpRouter.add(
    "*",
    "*",
    Effect.gen(function* () {
      const request = yield* HttpServerRequest.HttpServerRequest;
      const requestPath = request.url.split("?", 1)[0];
      const requestLogEntry = formatMockUpdateRequestLogEntry({
        method: request.method,
        range: request.headers.range,
        requestUrl: request.url,
      });
      yield* Effect.logInfo(requestLogEntry);
      if (requestLogPath !== undefined) {
        yield* Effect.promise(() =>
          NodeFS.promises.appendFile(requestLogPath, `${requestLogEntry}\n`),
        );
      }

      if (requestPath === MOCK_UPDATE_READY_PATH && request.method === "GET") {
        return yield* HttpServerResponse.json({ ready: true });
      }

      if (request.method !== "GET" && request.method !== "HEAD") {
        return HttpServerResponse.empty({
          status: 405,
          headers: { allow: "GET, HEAD" },
        });
      }

      const filePath = yield* resolveRequestedFilePath(rootRealPath, request.url);
      if (!filePath) {
        yield* Effect.logWarning(`Attempted to access file outside of root: ${request.url}`);
        return HttpServerResponse.text("Not Found", { status: 404 });
      }

      const openedFile = yield* openFile(rootRealPath, filePath);
      if (!openedFile) {
        yield* Effect.logWarning(`Attempted to access invalid file: ${filePath}`);
        return HttpServerResponse.text("Not Found", { status: 404 });
      }

      yield* Effect.logInfo(`Serving file: ${filePath}`);
      const size = openedFile.size;
      const range = resolveByteRange(request.headers.range, size);
      if (range._tag === "Unsatisfiable") {
        yield* Effect.promise(() => openedFile.close());
        return HttpServerResponse.empty({
          status: 416,
          headers: { "accept-ranges": "bytes", "content-range": `bytes */${size}` },
        });
      }
      const responseHeaders = {
        "accept-ranges": "bytes",
        "content-type": contentTypeForUpdateFile(filePath),
      };
      if (range._tag === "Partial") {
        if (request.method === "HEAD") {
          yield* Effect.promise(() => openedFile.close());
          return HttpServerResponse.empty({
            status: 206,
            headers: {
              ...responseHeaders,
              "content-length": String(range.end - range.start + 1),
              "content-range": `bytes ${range.start}-${range.end}/${size}`,
            },
          });
        }
        const body = yield* makeOpenedFileStream(
          openedFile,
          range.start,
          range.end - range.start + 1,
        );
        return HttpServerResponse.stream(body, {
          status: 206,
          contentLength: range.end - range.start + 1,
          headers: {
            ...responseHeaders,
            "content-range": `bytes ${range.start}-${range.end}/${size}`,
          },
        });
      }
      if (request.method === "HEAD") {
        yield* Effect.promise(() => openedFile.close());
        return HttpServerResponse.empty({
          status: 200,
          headers: { ...responseHeaders, "content-length": String(size) },
        });
      }
      const body = yield* makeOpenedFileStream(openedFile, 0, size);
      return HttpServerResponse.stream(body, {
        status: 200,
        contentLength: size,
        headers: responseHeaders,
      });
    }).pipe(
      Effect.catchCause((cause) =>
        Effect.gen(function* () {
          yield* Effect.logError(`Unhandled mock update request failure: ${cause}`);
          return HttpServerResponse.text("Internal Server Error", { status: 500 });
        }),
      ),
    ),
  );
};

export const makeMockUpdateServerLayer = (config: MockUpdateServerConfig) =>
  HttpRouter.serve(
    makeMockUpdateRouteLayer(config.rootRealPath, openValidatedUpdateFile, config.requestLogPath),
  ).pipe(
    Layer.provideMerge(
      NodeHttpServer.layer(NodeHttp.createServer, {
        host: MOCK_UPDATE_LOOPBACK_HOST,
        port: config.port,
      }),
    ),
    Layer.provideMerge(NodeServices.layer),
  );

type MainLauncher = <E, A>(effect: Effect.Effect<A, E, never>) => void;

export function runMockUpdateServerMain(
  isMain: boolean,
  launch: MainLauncher = NodeRuntime.runMain,
): boolean {
  if (!isMain) return false;
  launch(
    resolveMockUpdateServerConfig.pipe(
      Effect.map(makeMockUpdateServerLayer),
      Layer.unwrap,
      Layer.launch,
      Effect.provide(NodeServices.layer),
    ),
  );
  return true;
}

runMockUpdateServerMain(import.meta.main);
