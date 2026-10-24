// @effect-diagnostics nodeBuiltinImport:off
import * as NodeHttpServer from "@effect/platform-node/NodeHttpServer";
import * as NodeServices from "@effect/platform-node/NodeServices";
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";
import { assert, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as ConfigProvider from "effect/ConfigProvider";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Path from "effect/Path";
import { HttpClient, HttpClientRequest, HttpRouter } from "effect/http";

import {
  formatMockUpdateRequestLogEntry,
  makeMockUpdateRouteLayer,
  makeMockUpdateServerLayer,
  makeResolveMockUpdateServerConfig,
  MOCK_UPDATE_LOOPBACK_HOST,
  MOCK_UPDATE_READY_PATH,
  openValidatedUpdateFile,
  resolveByteRange,
  resolveRequestedFilePath,
  resolveMockUpdateServerConfig,
  runMockUpdateServerMain,
} from "./mock-update-server.ts";

it("pins the updater server and probes to the same IPv4 loopback host", () => {
  assert.equal(MOCK_UPDATE_LOOPBACK_HOST, "127.0.0.1");
});

const withMockUpdateServer = <A, E, R>(rootRealPath: string, effect: Effect.Effect<A, E, R>) =>
  effect.pipe(
    Effect.provide(
      HttpRouter.serve(makeMockUpdateRouteLayer(rootRealPath), {
        disableListenLog: true,
        disableLogger: true,
      }).pipe(Layer.provideMerge(NodeHttpServer.layerTest)),
    ),
  );

const createSymlinkOrSkip = Effect.fn("createSymlinkOrSkip")(function* (
  target: string,
  linkPath: string,
) {
  const fileSystem = yield* FileSystem.FileSystem;
  return yield* fileSystem.symlink(target, linkPath).pipe(
    Effect.as(true),
    Effect.orElseSucceed(() => false),
  );
});

it.layer(NodeServices.layer)("mock-update-server", (it) => {
  it.effect("serves files from the configured root", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const root = yield* fileSystem.makeTempDirectoryScoped({
        prefix: "mock-update-server-root-",
      });
      const rootRealPath = yield* fileSystem.realPath(root);
      const filePath = path.join(root, "latest.yml");

      yield* fileSystem.writeFileString(filePath, "version: 0.0.1\n");
      yield* fileSystem.writeFileString(path.join(root, "latest.yaml"), "version: yaml\n");
      yield* fileSystem.writeFileString(path.join(root, "latest.json"), "{}\n");

      yield* withMockUpdateServer(
        rootRealPath,
        Effect.gen(function* () {
          const client = yield* HttpClient.HttpClient;
          const response = yield* client.get("/latest.yml");

          assert.equal(response.status, 200);
          assert.equal(response.headers["content-type"], "text/yaml");
          assert.equal(yield* response.text, "version: 0.0.1\n");

          const yaml = yield* client.get("/latest.yaml");
          assert.equal(yaml.headers["content-type"], "text/yaml");
          const json = yield* client.get("/latest.json");
          assert.equal(json.headers["content-type"], "application/json");
        }),
      );
    }),
  );

  it.effect("serves valid ranges and rejects unsatisfiable ranges", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const root = yield* fileSystem.makeTempDirectoryScoped({ prefix: "mock-update-range-" });
      const rootRealPath = yield* fileSystem.realPath(root);
      yield* fileSystem.writeFileString(path.join(root, "artifact.bin"), "0123456789");

      yield* withMockUpdateServer(
        rootRealPath,
        Effect.gen(function* () {
          const client = yield* HttpClient.HttpClient;
          const partial = yield* client.get("/artifact.bin", {
            headers: { range: "bytes=2-5" },
          });
          assert.equal(partial.status, 206);
          assert.equal(partial.headers["content-range"], "bytes 2-5/10");
          assert.equal(partial.headers["accept-ranges"], "bytes");
          assert.equal(yield* partial.text, "2345");

          const suffix = yield* client.get("/artifact.bin", {
            headers: { range: "bytes=-3" },
          });
          assert.equal(suffix.status, 206);
          assert.equal(yield* suffix.text, "789");

          const invalid = yield* client.get("/artifact.bin", {
            headers: { range: "bytes=20-30" },
          });
          assert.equal(invalid.status, 416);
          assert.equal(invalid.headers["content-range"], "bytes */10");

          const head = yield* HttpClientRequest.head("/artifact.bin").pipe(HttpClient.execute);
          assert.equal(head.status, 200);
          assert.equal(yield* head.text, "");

          const partialHead = yield* HttpClientRequest.head("/artifact.bin").pipe(
            HttpClientRequest.setHeader("range", "bytes=2-5"),
            HttpClient.execute,
          );
          assert.equal(partialHead.status, 206);
          assert.equal(partialHead.headers["content-range"], "bytes 2-5/10");

          const post = yield* HttpClientRequest.post("/artifact.bin").pipe(HttpClient.execute);
          assert.equal(post.status, 405);
          assert.equal(post.headers.allow, "GET, HEAD");
        }),
      );
    }),
  );

  it("parses open, suffix, malformed, and multi-range headers deterministically", () => {
    assert.deepStrictEqual(resolveByteRange(undefined, 10), { _tag: "Full" });
    assert.deepStrictEqual(resolveByteRange("bytes=4-", 10), { _tag: "Partial", start: 4, end: 9 });
    assert.deepStrictEqual(resolveByteRange("bytes=-99", 10), {
      _tag: "Partial",
      start: 0,
      end: 9,
    });
    assert.deepStrictEqual(resolveByteRange("bytes=abc", 10), { _tag: "Full" });
    assert.deepStrictEqual(resolveByteRange("bytes=-0", 10), { _tag: "Full" });
    assert.deepStrictEqual(resolveByteRange("items=1-2", 10), { _tag: "Full" });
    assert.deepStrictEqual(resolveByteRange("bytes=1-2,4-5", 10), { _tag: "Full" });
    assert.deepStrictEqual(resolveByteRange("bytes=8-4", 10), { _tag: "Unsatisfiable" });
    assert.deepStrictEqual(resolveByteRange("bytes=0-0", 0), { _tag: "Unsatisfiable" });
  });

  it.effect("normalizes direct request paths", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const root = yield* fileSystem.makeTempDirectoryScoped({ prefix: "mock-update-paths-" });
      const rootRealPath = yield* fileSystem.realPath(root);

      assert.equal(yield* resolveRequestedFilePath(rootRealPath, undefined), rootRealPath);
      assert.equal(
        yield* resolveRequestedFilePath(rootRealPath, "artifact.bin?download=1"),
        path.join(rootRealPath, "artifact.bin"),
      );
      assert.equal(yield* resolveRequestedFilePath(rootRealPath, "/%E0%A4%A"), undefined);
      assert.equal(yield* resolveRequestedFilePath(rootRealPath, "/../outside"), undefined);
    }),
  );

  it.effect("returns 500 when constructing the file response fails", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const root = yield* fileSystem.makeTempDirectoryScoped({ prefix: "mock-update-failure-" });
      const rootRealPath = yield* fileSystem.realPath(root);
      yield* fileSystem.writeFileString(path.join(root, "artifact.bin"), "payload");

      yield* Effect.gen(function* () {
        const client = yield* HttpClient.HttpClient;
        const response = yield* client.get("/artifact.bin");
        assert.equal(response.status, 500);
        assert.equal(yield* response.text, "Internal Server Error");
      }).pipe(
        Effect.provide(
          HttpRouter.serve(
            makeMockUpdateRouteLayer(rootRealPath, () => Effect.die("file response failed")),
            { disableListenLog: true, disableLogger: true },
          ).pipe(Layer.provideMerge(NodeHttpServer.layerTest)),
        ),
      );
    }),
  );

  it.effect("closes the opened handle when response stream construction fails", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const root = yield* fileSystem.makeTempDirectoryScoped({
        prefix: "mock-update-stream-fail-",
      });
      const rootRealPath = yield* fileSystem.realPath(root);
      let closeCalls = 0;

      const response = yield* Effect.gen(function* () {
        const client = yield* HttpClient.HttpClient;
        return yield* client.get("/artifact.bin");
      }).pipe(
        Effect.provide(
          HttpRouter.serve(
            makeMockUpdateRouteLayer(rootRealPath, () =>
              Effect.succeed({
                size: 7,
                close: async () => {
                  closeCalls += 1;
                },
                stream: () => {
                  throw new Error("stream construction failed");
                },
              }),
            ),
            { disableListenLog: true, disableLogger: true },
          ).pipe(Layer.provideMerge(NodeHttpServer.layerTest)),
        ),
      );

      assert.equal(response.status, 500);
      assert.equal(closeCalls, 1);
    }),
  );

  it.effect("fails closed when the opened handle identity changes after validation", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const root = yield* fileSystem.makeTempDirectoryScoped({ prefix: "mock-update-swap-root-" });
      const outside = yield* fileSystem.makeTempDirectoryScoped({
        prefix: "mock-update-swap-outside-",
      });
      const rootRealPath = yield* fileSystem.realPath(root);
      const requestedPath = path.join(root, "artifact.bin");
      const outsidePath = path.join(outside, "secret.bin");
      yield* fileSystem.writeFileString(requestedPath, "inside");
      yield* fileSystem.writeFileString(outsidePath, "secret");

      let outsideHandleClosed = false;
      const response = yield* Effect.gen(function* () {
        const client = yield* HttpClient.HttpClient;
        return yield* client.get("/artifact.bin");
      }).pipe(
        Effect.provide(
          HttpRouter.serve(
            makeMockUpdateRouteLayer(rootRealPath, (validatedRoot, filePath) =>
              openValidatedUpdateFile(validatedRoot, filePath, {
                open: async () => {
                  const handle = await NodeFS.promises.open(outsidePath, "r");
                  return {
                    stat: (options) => handle.stat(options),
                    createReadStream: (options) => handle.createReadStream(options),
                    close: async () => {
                      outsideHandleClosed = true;
                      await handle.close();
                    },
                  };
                },
              }),
            ),
            { disableListenLog: true, disableLogger: true },
          ).pipe(Layer.provideMerge(NodeHttpServer.layerTest)),
        ),
      );

      assert.equal(response.status, 404);
      assert.equal(yield* response.text, "Not Found");
      assert.isTrue(outsideHandleClosed);
    }),
  );

  it.effect("fails closed for unverifiable paths, handles, and file sizes", () =>
    Effect.gen(function* () {
      const root = NodePath.resolve("X:/updates");
      const filePath = NodePath.join(root, "artifact.bin");
      const safeStat = {
        dev: 1n,
        ino: 2n,
        size: 6n,
        isFile: () => true,
      };
      let closeCalls = 0;
      const handle = {
        stat: async () => safeStat,
        createReadStream: () => {
          throw new Error("unreachable stream");
        },
        close: async () => {
          closeCalls += 1;
        },
      };

      assert.equal(
        yield* openValidatedUpdateFile(root, filePath, {
          realPath: async () => NodePath.resolve("X:/outside/artifact.bin"),
          stat: async () => safeStat,
          open: async () => handle,
        }),
        undefined,
      );
      assert.equal(
        yield* openValidatedUpdateFile(root, filePath, {
          realPath: async () => filePath,
          stat: async () => ({ ...safeStat, isFile: () => false }),
          open: async () => handle,
        }),
        undefined,
      );
      assert.equal(
        yield* openValidatedUpdateFile(root, filePath, {
          realPath: async () => filePath,
          stat: async () => safeStat,
          open: async () => ({ ...handle, stat: async () => ({ ...safeStat, dev: 0n }) }),
        }),
        undefined,
      );
      assert.equal(
        yield* openValidatedUpdateFile(root, filePath, {
          realPath: async () => filePath,
          stat: async () => ({ ...safeStat, size: BigInt(Number.MAX_SAFE_INTEGER) + 1n }),
          open: async () => ({
            ...handle,
            stat: async () => ({ ...safeStat, size: BigInt(Number.MAX_SAFE_INTEGER) + 1n }),
          }),
        }),
        undefined,
      );
      assert.equal(
        yield* openValidatedUpdateFile(root, filePath, {
          realPath: async () => filePath,
          stat: async () => safeStat,
          open: async () => Promise.reject(new Error("open failed")),
        }),
        undefined,
      );
      assert.equal(
        yield* openValidatedUpdateFile(root, filePath, {
          realPath: async () => filePath,
          stat: async () => safeStat,
          open: async () => ({
            ...handle,
            stat: async () => ({ ...safeStat, ino: 3n }),
            close: async () => Promise.reject(new Error("close failed")),
          }),
        }),
        undefined,
      );
      assert.equal(closeCalls, 2);
    }),
  );

  it.effect("returns 404 for malformed, missing, directory, and NUL paths", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const root = yield* fileSystem.makeTempDirectoryScoped({ prefix: "mock-update-invalid-" });
      const rootRealPath = yield* fileSystem.realPath(root);
      yield* fileSystem.makeDirectory(path.join(root, "directory"));

      yield* withMockUpdateServer(
        rootRealPath,
        Effect.gen(function* () {
          const client = yield* HttpClient.HttpClient;
          for (const requestPath of ["/%E0%A4%A", "/missing", "/directory", "/%00bad"]) {
            const response = yield* client.get(requestPath);
            assert.equal(response.status, 404);
          }
        }),
      );
    }),
  );

  it.effect("resolves server defaults and configured root paths", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const root = yield* fileSystem.makeTempDirectoryScoped({ prefix: "mock-update-config-" });
      const config = yield* resolveMockUpdateServerConfig.pipe(
        Effect.provide(
          ConfigProvider.layer(
            ConfigProvider.fromEnv({
              env: {
                BIBCODE_DESKTOP_MOCK_UPDATE_SERVER_PORT: "4312",
                BIBCODE_DESKTOP_MOCK_UPDATE_SERVER_REQUEST_LOG: path.join(root, "requests.jsonl"),
                BIBCODE_DESKTOP_MOCK_UPDATE_SERVER_ROOT: root,
              },
            }),
          ),
        ),
      );
      assert.equal(config.port, 4312);
      assert.equal(config.requestLogPath, path.join(root, "requests.jsonl"));
      assert.equal(config.rootRealPath, yield* fileSystem.realPath(path.resolve(root)));

      const scriptDirectory = path.join(root, "scripts");
      const defaultRoot = path.join(root, "release-mock");
      yield* fileSystem.makeDirectory(scriptDirectory);
      yield* fileSystem.makeDirectory(defaultRoot);
      const defaults = yield* makeResolveMockUpdateServerConfig(scriptDirectory).pipe(
        Effect.provide(ConfigProvider.layer(ConfigProvider.fromEnv({ env: {} }))),
      );
      assert.equal(defaults.port, 3000);
      assert.isUndefined(defaults.requestLogPath);
      assert.equal(defaults.rootRealPath, yield* fileSystem.realPath(defaultRoot));

      assert.ok(makeMockUpdateServerLayer(config));
    }),
  );

  it.effect("publishes deterministic readiness and range-request logs without query secrets", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const root = yield* fileSystem.makeTempDirectoryScoped({ prefix: "mock-update-ready-" });
      const rootRealPath = yield* fileSystem.realPath(root);
      yield* fileSystem.writeFileString(NodePath.join(root, "candidate.AppImage"), "candidate");

      yield* withMockUpdateServer(
        rootRealPath,
        Effect.gen(function* () {
          const client = yield* HttpClient.HttpClient;
          const ready = yield* client.get(MOCK_UPDATE_READY_PATH);
          assert.equal(ready.status, 200);
          assert.deepEqual(yield* ready.json, { ready: true });

          const entry = formatMockUpdateRequestLogEntry({
            method: "GET",
            range: "bytes=0-3",
            requestUrl: "/candidate.AppImage?token=updater-secret",
          });
          assert.equal(entry, '{"method":"GET","path":"/candidate.AppImage","range":"bytes=0-3"}');
          assert.notInclude(entry, "updater-secret");
        }),
      );
    }),
  );

  it("launches only as the direct CLI entrypoint", () => {
    const launched: unknown[] = [];
    assert.equal(
      runMockUpdateServerMain(false, (effect) => launched.push(effect)),
      false,
    );
    assert.equal(
      runMockUpdateServerMain(true, (effect) => launched.push(effect)),
      true,
    );
    assert.equal(launched.length, 1);
  });

  it.effect("rejects encoded path traversal outside the configured root", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const root = yield* fileSystem.makeTempDirectoryScoped({
        prefix: "mock-update-server-root-",
      });
      const outside = yield* fileSystem.makeTempDirectoryScoped({
        prefix: "mock-update-server-outside-",
      });
      const rootRealPath = yield* fileSystem.realPath(root);

      yield* fileSystem.writeFileString(path.join(outside, "secret.txt"), "nope\n");

      yield* withMockUpdateServer(
        rootRealPath,
        Effect.gen(function* () {
          const client = yield* HttpClient.HttpClient;
          const response = yield* client.get("/%2e%2e/secret.txt");

          assert.equal(response.status, 404);
          assert.equal(yield* response.text, "Not Found");
        }),
      );
    }),
  );

  it.effect("rejects symlinked files that escape the configured root", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const root = yield* fileSystem.makeTempDirectoryScoped({
        prefix: "mock-update-server-root-",
      });
      const outside = yield* fileSystem.makeTempDirectoryScoped({
        prefix: "mock-update-server-outside-",
      });
      const rootRealPath = yield* fileSystem.realPath(root);
      const outsideFile = path.join(outside, "outside.yml");
      const linksDir = path.join(root, "links");
      const symlinkPath = path.join(linksDir, "outside.yml");

      yield* fileSystem.writeFileString(outsideFile, "version: outside\n");
      yield* fileSystem.makeDirectory(linksDir, { recursive: true });
      const created = yield* createSymlinkOrSkip(outsideFile, symlinkPath);
      if (!created) return;

      yield* withMockUpdateServer(
        rootRealPath,
        Effect.gen(function* () {
          const client = yield* HttpClient.HttpClient;
          const response = yield* client.get("/links/outside.yml");

          assert.equal(response.status, 404);
          assert.equal(yield* response.text, "Not Found");
        }),
      );
    }),
  );
});
