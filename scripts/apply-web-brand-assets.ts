#!/usr/bin/env node

import * as NodeRuntime from "@effect/platform-node/NodeRuntime";
import * as NodeServices from "@effect/platform-node/NodeServices";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Option from "effect/Option";
import * as Path from "effect/Path";
import * as PlatformError from "effect/PlatformError";
import { Argument, Command, Flag } from "effect/cli";
import {
  resolveWebAssetBrandForChannel,
  resolveWebIconOverrides,
  WEB_ASSET_CHANNELS,
  type WebAssetBrand,
} from "./lib/brand-assets.ts";

const WEB_ASSET_BRANDS = [
  "development",
  "nightly",
  "production",
] as const satisfies ReadonlyArray<WebAssetBrand>;

export const applyWebBrandAssets = Effect.fn("applyWebBrandAssets")(function* (
  brand: WebAssetBrand,
  targetDirectory: string,
  options: { readonly rootDir?: string | undefined } = {},
) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const repoRoot = options.rootDir
    ? path.resolve(options.rootDir)
    : yield* path.fromFileUrl(new URL("..", import.meta.url));

  yield* Effect.forEach(
    resolveWebIconOverrides(brand, targetDirectory),
    (override) =>
      fs.copyFile(
        path.join(repoRoot, override.sourceRelativePath),
        path.join(repoRoot, override.targetRelativePath),
      ),
    { concurrency: "unbounded" },
  );
});

type ApplyWebBrandAssets = (
  brand: WebAssetBrand,
  targetDirectory: string,
) => Effect.Effect<
  void,
  PlatformError.PlatformError | PlatformError.BadArgument,
  FileSystem.FileSystem | Path.Path
>;

type MainLauncher = <E, A>(effect: Effect.Effect<A, E, never>) => void;

export const makeApplyWebBrandAssetsCommand = (apply: ApplyWebBrandAssets = applyWebBrandAssets) =>
  Command.make(
    "apply-web-brand-assets",
    {
      brand: Argument.Literals("brand", WEB_ASSET_BRANDS).pipe(
        Argument.withDescription("Asset brand to copy into the hosted web output directory."),
        Argument.optional,
      ),
      channel: Flag.Literals("channel", WEB_ASSET_CHANNELS).pipe(
        Flag.withDescription("Hosted release channel to map to a web asset brand."),
        Flag.optional,
      ),
      targetDirectory: Argument.String("target-directory").pipe(
        Argument.withDescription("Output directory that contains the hosted web build assets."),
        Argument.optional,
      ),
    },
    ({ brand, channel, targetDirectory }) =>
      apply(
        Option.getOrElse(brand, () =>
          Option.match(channel, {
            onNone: () => "production" as const,
            onSome: resolveWebAssetBrandForChannel,
          }),
        ),
        Option.getOrElse(targetDirectory, () => "apps/web/dist"),
      ),
  ).pipe(Command.withDescription("Copy web brand assets into a built hosted web app."));

export const applyWebBrandAssetsCommand = makeApplyWebBrandAssetsCommand();

export const runApplyWebBrandAssetsMain = (
  isMain: boolean,
  launch: MainLauncher = NodeRuntime.runMain,
) => {
  if (!isMain) return false;

  launch(
    Command.run(applyWebBrandAssetsCommand, { version: "0.0.0" }).pipe(
      Effect.provide(NodeServices.layer),
    ),
  );
  return true;
};

runApplyWebBrandAssetsMain(import.meta.main);
