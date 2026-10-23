#!/usr/bin/env node

import * as NodeRuntime from "@effect/platform-node/NodeRuntime";
import * as NodeServices from "@effect/platform-node/NodeServices";
import * as NodeBuffer from "node:buffer";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";
import * as Schema from "effect/Schema";
import { Command, Flag } from "effect/cli";
import { fromJsonStringPretty } from "@bibcode/shared/schemaJson";

import {
  TAURI_UPDATE_TARGETS,
  type TauriUpdaterTarget as TauriUpdateTarget,
} from "./lib/release-targets.ts";

export { TAURI_UPDATE_TARGETS };

export interface BuildTauriUpdateManifestInput {
  readonly assetsDir: string;
  readonly version: string;
  readonly tag: string;
  readonly repository: "mubeda/BibCode";
  readonly pubDate: string;
  readonly notes: string;
}

export interface TauriUpdateManifest {
  readonly version: string;
  readonly notes: string;
  readonly pub_date: string;
  readonly platforms: Readonly<
    Record<TauriUpdateTarget, { readonly signature: string; readonly url: string }>
  >;
}

export class TauriUpdateManifestLayoutError extends Schema.TaggedError<TauriUpdateManifestLayoutError>()(
  "TauriUpdateManifestLayoutError",
  { message: Schema.String },
) {}

const SafeBasename = Schema.String.check(Schema.isPattern(/^[A-Za-z0-9][A-Za-z0-9._()+ -]*$/));
const SignatureBasename = SafeBasename.check(Schema.isPattern(/\.sig$/));
const StableVersion = Schema.String.check(
  Schema.isPattern(/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/),
);
const Rfc3339Date = Schema.String.check(
  Schema.isPattern(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2)$/),
).pipe(Schema.decodeTo(Schema.DateTimeUtcFromString));
const BASE64 =
  "(?:(?:[A-Za-z0-9+/]{4})+(?:(?:[A-Za-z0-9+/]{2}==)|(?:[A-Za-z0-9+/]{3}=))?|(?:[A-Za-z0-9+/]{2}==)|(?:[A-Za-z0-9+/]{3}=))";
const MinisignSignature = Schema.String.check(
  Schema.isPattern(
    new RegExp(
      `^untrusted comment: [^\\r\\n]+\\n${BASE64}\\ntrusted comment: [^\\r\\n]+\\n${BASE64}\\n?$`,
    ),
  ),
).check(
  Schema.makeFilter((signature) => {
    const [, signatureBox, , globalSignature] = signature.split("\n");
    const signatureBytes = NodeBuffer.Buffer.from(signatureBox!, "base64");
    if (
      signatureBytes.length !== 74 ||
      signatureBytes[0] !== 0x45 ||
      (signatureBytes[1] !== 0x64 && signatureBytes[1] !== 0x44)
    ) {
      return "minisign signature box must be 74 bytes beginning with Ed or ED";
    }
    return NodeBuffer.Buffer.from(globalSignature!, "base64").length === 64
      ? undefined
      : "minisign global signature must be 64 bytes";
  }),
);
const EncodedMinisignSignature = Schema.StringFromBase64.pipe(Schema.decodeTo(MinisignSignature));

const UpdaterArtifactDescriptorSchema = Schema.Struct({
  target: Schema.Literals(TAURI_UPDATE_TARGETS),
  artifact: SafeBasename,
  signature: SignatureBasename,
}).check(
  Schema.makeFilter(({ target, artifact, signature }) => {
    if (signature !== `${artifact}.sig`) return "signature must be <artifact>.sig";
    const suffix = target.startsWith("darwin-")
      ? ".app.tar.gz"
      : target.startsWith("linux-")
        ? ".AppImage"
        : ".exe";
    return artifact.endsWith(suffix) ? undefined : `artifact must end with ${suffix}`;
  }),
);
const UpdaterArtifactDescriptorJson = Schema.fromJsonString(UpdaterArtifactDescriptorSchema);
const TauriUpdateManifestSchema = Schema.Struct({
  version: Schema.String,
  notes: Schema.String,
  pub_date: Schema.String,
  platforms: Schema.Struct({
    "darwin-aarch64": Schema.Struct({ signature: Schema.String, url: Schema.String }),
    "darwin-x86_64": Schema.Struct({ signature: Schema.String, url: Schema.String }),
    "linux-aarch64": Schema.Struct({ signature: Schema.String, url: Schema.String }),
    "linux-x86_64": Schema.Struct({ signature: Schema.String, url: Schema.String }),
    "windows-aarch64": Schema.Struct({ signature: Schema.String, url: Schema.String }),
    "windows-x86_64": Schema.Struct({ signature: Schema.String, url: Schema.String }),
  }),
});
const BuildInputSchema = Schema.Struct({
  assetsDir: Schema.NonEmptyString,
  version: StableVersion,
  tag: Schema.NonEmptyString,
  repository: Schema.Literal("mubeda/BibCode"),
  pubDate: Schema.String.check(
    Schema.isPattern(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2)$/),
  ),
  notes: Schema.String,
}).check(
  Schema.makeFilter(({ tag, version }) =>
    tag === `v${version}` ? undefined : "tag must equal v<version>",
  ),
);

const decodeBuildInput = Schema.decodeUnknownEffect(BuildInputSchema);
const decodeDescriptor = Schema.decodeUnknownEffect(UpdaterArtifactDescriptorJson);
const decodeRfc3339Date = Schema.decodeUnknownEffect(Rfc3339Date);
const decodeEncodedMinisignSignature = Schema.decodeUnknownEffect(EncodedMinisignSignature);
const encodeTauriUpdateManifest = Schema.encodeSync(
  fromJsonStringPretty(TauriUpdateManifestSchema),
);

export const serializeTauriUpdateManifest = (manifest: TauriUpdateManifest): string =>
  `${encodeTauriUpdateManifest(manifest)}\n`;

export const buildTauriUpdateManifest = Effect.fn("buildTauriUpdateManifest")(function* (
  input: BuildTauriUpdateManifestInput,
) {
  const validatedInput = yield* decodeBuildInput(input);
  yield* decodeRfc3339Date(validatedInput.pubDate);

  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const entries = (yield* fs.readDirectory(validatedInput.assetsDir)).toSorted();
  const descriptorPaths = entries
    .filter((entry) => entry.startsWith("updater-") && entry.endsWith(".json"))
    .map((entry) => path.join(validatedInput.assetsDir, entry));
  const descriptors = yield* Effect.forEach(descriptorPaths, (descriptorPath) =>
    fs.readFileString(descriptorPath).pipe(Effect.flatMap(decodeDescriptor)),
  );
  const descriptorsByTarget = new Map<TauriUpdateTarget, (typeof descriptors)[number]>();
  const artifacts = new Set<string>();
  const signatures = new Set<string>();
  for (const descriptor of descriptors) {
    if (descriptorsByTarget.has(descriptor.target)) {
      return yield* new TauriUpdateManifestLayoutError({
        message: `Duplicate updater descriptor for ${descriptor.target}.`,
      });
    }
    if (artifacts.has(descriptor.artifact)) {
      return yield* new TauriUpdateManifestLayoutError({
        message: `Duplicate updater artifact ${descriptor.artifact}.`,
      });
    }
    if (signatures.has(descriptor.signature)) {
      return yield* new TauriUpdateManifestLayoutError({
        message: `Duplicate updater signature ${descriptor.signature}.`,
      });
    }
    descriptorsByTarget.set(descriptor.target, descriptor);
    artifacts.add(descriptor.artifact);
    signatures.add(descriptor.signature);
  }
  if (descriptors.length !== TAURI_UPDATE_TARGETS.length) {
    return yield* new TauriUpdateManifestLayoutError({
      message: "Expected exactly one updater descriptor for each supported target.",
    });
  }

  const platforms = {} as Record<
    TauriUpdateTarget,
    { readonly signature: string; readonly url: string }
  >;
  for (const target of TAURI_UPDATE_TARGETS) {
    const descriptor = descriptorsByTarget.get(target);
    if (!descriptor)
      return yield* new TauriUpdateManifestLayoutError({
        message: `Missing updater descriptor for ${target}.`,
      });
    yield* fs.readFileString(path.join(validatedInput.assetsDir, descriptor.artifact));
    const signature = (yield* fs.readFileString(
      path.join(validatedInput.assetsDir, descriptor.signature),
    )).trim();
    yield* decodeEncodedMinisignSignature(signature);
    platforms[target] = {
      signature,
      url: `https://github.com/${validatedInput.repository}/releases/download/${validatedInput.tag}/${encodeURIComponent(descriptor.artifact).replace(/[()]/g, (character) => `%${character.charCodeAt(0).toString(16).toUpperCase()}`)}`,
    };
  }

  return {
    version: validatedInput.version,
    notes: validatedInput.notes,
    pub_date: validatedInput.pubDate,
    platforms,
  };
});

export const buildTauriUpdateManifestCommand = Command.make(
  "build-tauri-update-manifest",
  {
    assetsDir: Flag.String("assets-dir"),
    version: Flag.String("version"),
    tag: Flag.String("tag"),
    repository: Flag.String("repository"),
    pubDate: Flag.String("pub-date"),
    notes: Flag.String("notes"),
    output: Flag.String("output"),
  },
  ({ output, ...input }) =>
    buildTauriUpdateManifest(input as BuildTauriUpdateManifestInput).pipe(
      Effect.flatMap((manifest) =>
        FileSystem.FileSystem.pipe(
          Effect.flatMap((fs) =>
            fs.writeFileString(output, serializeTauriUpdateManifest(manifest)),
          ),
        ),
      ),
    ),
).pipe(Command.withDescription("Build a deterministic static Tauri update manifest."));

type MainLauncher = <E, A>(effect: Effect.Effect<A, E, never>) => void;

export const runBuildTauriUpdateManifestMain = (
  isMain: boolean,
  launch: MainLauncher = NodeRuntime.runMain,
): boolean => {
  if (!isMain) return false;
  launch(
    Command.run(buildTauriUpdateManifestCommand, { version: "0.0.0" }).pipe(
      Effect.provide(NodeServices.layer),
    ),
  );
  return true;
};

runBuildTauriUpdateManifestMain(import.meta.main);
