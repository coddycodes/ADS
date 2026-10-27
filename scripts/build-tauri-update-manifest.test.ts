import * as NodeServices from "@effect/platform-node/NodeServices";
import { assert, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";
import * as Schema from "effect/Schema";

import {
  buildTauriUpdateManifest,
  serializeTauriUpdateManifest,
  TAURI_UPDATE_TARGETS,
} from "./build-tauri-update-manifest.ts";

const TEST_MINISIGN_SIGNATURE =
  "untrusted comment: signature from minisign secret key\n" +
  "RUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=\n" +
  "trusted comment: timestamp:1556193335\tfile:test\n" +
  "y/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==\n";
const TEST_SIGNATURE = Buffer.from(TEST_MINISIGN_SIGNATURE).toString("base64");
const TEST_LEGACY_MINISIGN_SIGNATURE =
  "untrusted comment: signature from minisign secret key\n" +
  "RWQf6LRCGA9i59SLOFxz6NxvASXDJeRtuZykwQepbDEGt87ig1BNpWaVWuNrm73YiIiJbq71Wi+dP9eKL8OC351vwIasSSbXxwA=\n" +
  "trusted comment: timestamp:1555779966\tfile:test\n" +
  "QtKMXWyYcwdpZAlPF7tE2ENJkRd1ujvKjlj1m9RtHTBnZPa5WKU5uWRs5GoP5M/VqE81QFuMKI5k/SfNQUaOAA==\n";
const TEST_LEGACY_SIGNATURE = Buffer.from(TEST_LEGACY_MINISIGN_SIGNATURE).toString("base64");
const encodeJson = Schema.encodeSync(Schema.fromJsonString(Schema.Unknown));

const artifacts = {
  "darwin-aarch64": "coddy-studio-update-darwin-aarch64.app.tar.gz",
  "darwin-x86_64": "coddy-studio-update-darwin-x86_64.app.tar.gz",
  "linux-aarch64": "Coddy Studio_0.2.12_arm64.AppImage",
  "linux-x86_64": "Coddy Studio_0.2.12_amd64.AppImage",
  "windows-aarch64": "Coddy Studio_0.2.12_arm64-setup.exe",
  "windows-x86_64": "Coddy Studio_0.2.12_x64-setup.exe",
} as const;

const inputFor = (assetsDir: string) => ({
  assetsDir,
  version: "0.2.12",
  tag: "v0.2.12",
  repository: "mubeda/CoddyStudio" as const,
  pubDate: "2026-07-24T12:00:00Z",
  notes: "Coddy Studio v0.2.12",
});

const writeFixture = Effect.fn("writeTauriUpdateFixture")(function* (assetsDir: string) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  for (const target of TAURI_UPDATE_TARGETS) {
    const artifact = artifacts[target];
    yield* fs.writeFileString(
      path.join(assetsDir, `updater-${target}.json`),
      encodeJson({ target, artifact, signature: `${artifact}.sig` }),
    );
    yield* fs.writeFileString(path.join(assetsDir, artifact), target);
    yield* fs.writeFileString(path.join(assetsDir, `${artifact}.sig`), `${TEST_SIGNATURE}\n`);
  }
});

const assertFailure = <A, E, R>(effect: Effect.Effect<A, E, R>) =>
  effect.pipe(Effect.flip, Effect.asVoid);

it.layer(NodeServices.layer)("build-tauri-update-manifest", (it) => {
  it.effect("builds a deterministic static manifest from the six updater descriptors", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const assetsDir = yield* fs.makeTempDirectoryScoped({ prefix: "tauri-update-manifest-" });
      yield* writeFixture(assetsDir);

      const expected = {
        version: "0.2.12",
        notes: "Coddy Studio v0.2.12",
        pub_date: "2026-07-24T12:00:00Z",
        platforms: {
          "darwin-aarch64": {
            signature: TEST_SIGNATURE,
            url: "https://github.com/mubeda/CoddyStudio/releases/download/v0.2.12/coddy-studio-update-darwin-aarch64.app.tar.gz",
          },
          "darwin-x86_64": {
            signature: TEST_SIGNATURE,
            url: "https://github.com/mubeda/CoddyStudio/releases/download/v0.2.12/coddy-studio-update-darwin-x86_64.app.tar.gz",
          },
          "linux-aarch64": {
            signature: TEST_SIGNATURE,
            url: "https://github.com/mubeda/CoddyStudio/releases/download/v0.2.12/Coddy Studio_0.2.12_arm64.AppImage",
          },
          "linux-x86_64": {
            signature: TEST_SIGNATURE,
            url: "https://github.com/mubeda/CoddyStudio/releases/download/v0.2.12/Coddy Studio_0.2.12_amd64.AppImage",
          },
          "windows-aarch64": {
            signature: TEST_SIGNATURE,
            url: "https://github.com/mubeda/CoddyStudio/releases/download/v0.2.12/Coddy Studio_0.2.12_arm64-setup.exe",
          },
          "windows-x86_64": {
            signature: TEST_SIGNATURE,
            url: "https://github.com/mubeda/CoddyStudio/releases/download/v0.2.12/Coddy Studio_0.2.12_x64-setup.exe",
          },
        },
      };
      const manifest = yield* buildTauriUpdateManifest(inputFor(assetsDir));
      assert.deepStrictEqual(manifest, expected);
      assert.equal(
        serializeTauriUpdateManifest(manifest),
        [
          "{",
          '  "version": "0.2.12",',
          '  "notes": "Coddy Studio v0.2.12",',
          '  "pub_date": "2026-07-24T12:00:00Z",',
          '  "platforms": {',
          '    "darwin-aarch64": {',
          `      "signature": "${TEST_SIGNATURE}",`,
          '      "url": "https://github.com/mubeda/CoddyStudio/releases/download/v0.2.12/coddy-studio-update-darwin-aarch64.app.tar.gz"',
          "    },",
          '    "darwin-x86_64": {',
          `      "signature": "${TEST_SIGNATURE}",`,
          '      "url": "https://github.com/mubeda/CoddyStudio/releases/download/v0.2.12/coddy-studio-update-darwin-x86_64.app.tar.gz"',
          "    },",
          '    "linux-aarch64": {',
          `      "signature": "${TEST_SIGNATURE}",`,
          '      "url": "https://github.com/mubeda/CoddyStudio/releases/download/v0.2.12/Coddy Studio_0.2.12_arm64.AppImage"',
          "    },",
          '    "linux-x86_64": {',
          `      "signature": "${TEST_SIGNATURE}",`,
          '      "url": "https://github.com/mubeda/CoddyStudio/releases/download/v0.2.12/Coddy Studio_0.2.12_amd64.AppImage"',
          "    },",
          '    "windows-aarch64": {',
          `      "signature": "${TEST_SIGNATURE}",`,
          '      "url": "https://github.com/mubeda/CoddyStudio/releases/download/v0.2.12/Coddy Studio_0.2.12_arm64-setup.exe"',
          "    },",
          '    "windows-x86_64": {',
          `      "signature": "${TEST_SIGNATURE}",`,
          '      "url": "https://github.com/mubeda/CoddyStudio/releases/download/v0.2.12/Coddy Studio_0.2.12_x64-setup.exe"',
          "    }",
          "  }",
          "}",
          "",
        ].join("\n"),
      );
    }),
  );

  it.effect("rejects missing, duplicate, and unknown updater targets", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      for (const change of ["missing", "duplicate", "unknown"] as const) {
        const assetsDir = yield* fs.makeTempDirectoryScoped({ prefix: `tauri-update-${change}-` });
        yield* writeFixture(assetsDir);
        if (change === "missing") {
          yield* fs.remove(path.join(assetsDir, "updater-linux-x86_64.json"));
        } else {
          yield* fs.writeFileString(
            path.join(assetsDir, `updater-${change}.json`),
            encodeJson({
              target: change === "duplicate" ? "darwin-aarch64" : "freebsd-x86_64",
              artifact: artifacts["darwin-aarch64"],
              signature: `${artifacts["darwin-aarch64"]}.sig`,
            }),
          );
        }
        yield* assertFailure(buildTauriUpdateManifest(inputFor(assetsDir)));
      }
    }),
  );

  it.effect("rejects two targets that reference the same updater payload and signature", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const assetsDir = yield* fs.makeTempDirectoryScoped({
        prefix: "tauri-update-duplicate-artifact-",
      });
      const artifact = artifacts["darwin-aarch64"];
      yield* writeFixture(assetsDir);
      yield* fs.writeFileString(
        path.join(assetsDir, "updater-darwin-x86_64.json"),
        encodeJson({ target: "darwin-x86_64", artifact, signature: `${artifact}.sig` }),
      );

      yield* assertFailure(buildTauriUpdateManifest(inputFor(assetsDir)));
    }),
  );

  it.effect("rejects unsafe, missing, and mismatched artifact files", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const cases = ["unsafe", "payload", "signature", "mismatch"] as const;
      for (const change of cases) {
        const assetsDir = yield* fs.makeTempDirectoryScoped({ prefix: `tauri-update-${change}-` });
        yield* writeFixture(assetsDir);
        const descriptorPath = path.join(assetsDir, "updater-darwin-aarch64.json");
        const artifact = artifacts["darwin-aarch64"];
        if (change === "unsafe") {
          yield* fs.writeFileString(
            descriptorPath,
            encodeJson({
              target: "darwin-aarch64",
              artifact: "../unsafe.app.tar.gz",
              signature: "unsafe.sig",
            }),
          );
        } else if (change === "payload") {
          yield* fs.remove(path.join(assetsDir, artifact));
        } else if (change === "signature") {
          yield* fs.remove(path.join(assetsDir, `${artifact}.sig`));
        } else {
          yield* fs.writeFileString(
            descriptorPath,
            encodeJson({ target: "darwin-aarch64", artifact, signature: "wrong.sig" }),
          );
        }
        yield* assertFailure(buildTauriUpdateManifest(inputFor(assetsDir)));
      }
    }),
  );

  it.effect("rejects empty, malformed base64, and malformed minisign signatures", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      for (const signature of [
        "",
        "not base64!",
        Buffer.from("not a minisign box").toString("base64"),
        Buffer.from(TEST_MINISIGN_SIGNATURE + "\n").toString("base64"),
        Buffer.from(TEST_MINISIGN_SIGNATURE.replace("trusted comment:", "trusted:")).toString(
          "base64",
        ),
        Buffer.from(TEST_MINISIGN_SIGNATURE.split("\n").slice(0, 3).join("\n")).toString("base64"),
        Buffer.from("untrusted comment: test\na\ntrusted comment: test\na").toString("base64"),
        Buffer.from("untrusted comment: test\n\ntrusted comment: test\n").toString("base64"),
        Buffer.from("untrusted comment: test\nYQ==\ntrusted comment: test\nYQ==").toString(
          "base64",
        ),
        Buffer.from(
          [
            "untrusted comment: test",
            Buffer.concat([Buffer.from("EE"), Buffer.alloc(72)]).toString("base64"),
            "trusted comment: test",
            Buffer.alloc(64).toString("base64"),
          ].join("\n"),
        ).toString("base64"),
        Buffer.from(
          [
            "untrusted comment: test",
            Buffer.concat([Buffer.from("eD"), Buffer.alloc(72)]).toString("base64"),
            "trusted comment: test",
            Buffer.alloc(64).toString("base64"),
          ].join("\n"),
        ).toString("base64"),
      ] as const) {
        const assetsDir = yield* fs.makeTempDirectoryScoped({ prefix: "tauri-update-signature-" });
        yield* writeFixture(assetsDir);
        const artifact = artifacts["darwin-aarch64"];
        yield* fs.writeFileString(path.join(assetsDir, `${artifact}.sig`), signature);
        yield* assertFailure(buildTauriUpdateManifest(inputFor(assetsDir)));
      }
    }),
  );

  it.effect("accepts the updater-supported legacy Ed signature algorithm", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const assetsDir = yield* fs.makeTempDirectoryScoped({ prefix: "tauri-update-legacy-" });
      yield* writeFixture(assetsDir);
      const artifact = artifacts["darwin-aarch64"];
      yield* fs.writeFileString(
        path.join(assetsDir, `${artifact}.sig`),
        `${TEST_LEGACY_SIGNATURE}\n`,
      );

      const manifest = yield* buildTauriUpdateManifest(inputFor(assetsDir));
      assert.equal(manifest.platforms["darwin-aarch64"].signature, TEST_LEGACY_SIGNATURE);
    }),
  );

  it.effect("encodes an artifact basename as one URL path segment", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const assetsDir = yield* fs.makeTempDirectoryScoped({ prefix: "tauri-update-url-" });
      const originalArtifact = artifacts["darwin-x86_64"];
      const artifact = "coddy-studio update+(x86).app.tar.gz";
      yield* writeFixture(assetsDir);
      yield* fs.remove(path.join(assetsDir, originalArtifact));
      yield* fs.remove(path.join(assetsDir, `${originalArtifact}.sig`));
      yield* fs.writeFileString(
        path.join(assetsDir, "updater-darwin-x86_64.json"),
        encodeJson({ target: "darwin-x86_64", artifact, signature: `${artifact}.sig` }),
      );
      yield* fs.writeFileString(path.join(assetsDir, artifact), "payload");
      yield* fs.writeFileString(path.join(assetsDir, `${artifact}.sig`), TEST_SIGNATURE);

      const manifest = yield* buildTauriUpdateManifest(inputFor(assetsDir));
      assert.equal(
        manifest.platforms["darwin-x86_64"].url,
        "https://github.com/mubeda/CoddyStudio/releases/download/v0.2.12/coddy-studio%20update%2B%28x86%29.app.tar.gz",
      );
    }),
  );

  it.effect("rejects invalid release metadata", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      for (const change of ["tag", "version", "date", "repository"] as const) {
        const assetsDir = yield* fs.makeTempDirectoryScoped({ prefix: `tauri-update-${change}-` });
        yield* writeFixture(assetsDir);
        const input = inputFor(assetsDir);
        yield* assertFailure(
          buildTauriUpdateManifest({
            ...input,
            tag: change === "tag" ? "v9.9.9" : input.tag,
            version: change === "version" ? "0.2" : input.version,
            pubDate: change === "date" ? "tomorrow" : input.pubDate,
            repository: change === "repository" ? ("other/coddy-studio" as never) : input.repository,
          }),
        );
      }
    }),
  );
});
