import type { UploadAppendInput } from "@bibcode/contracts";
import * as Effect from "effect/Effect";
import { RpcClientError } from "effect/rpc";
import { vi } from "vite-plus/test";

import type { UploadPort, UploadSession } from "./uploadStager.ts";

export const fileTarget = {
  _tag: "chat-attachment",
  type: "file",
  name: "a.txt",
  mimeType: "text/plain",
} as const;
export const lost = new RpcClientError.RpcClientError({
  reason: new RpcClientError.RpcClientDefect({
    message: "socket closed",
    cause: new Error("closed"),
  }),
});
export const body = (size: number) =>
  new Blob([Uint8Array.from({ length: size }, (_, i) => i % 251)]);

export function uploadHarness(capable = true, sizeBytes = 256 * 1024) {
  let ids = 0;
  const begin = vi.fn<UploadSession["begin"]>(() =>
    Effect.sync(() => ({ uploadId: `u-${++ids}`, exists: false })),
  );
  const append = vi.fn<UploadSession["append"]>((input: UploadAppendInput) =>
    Effect.succeed({
      receivedBytes: input.offset + Buffer.from(input.data, "base64").length,
    }),
  );
  const get = vi.fn<UploadSession["get"]>(() =>
    Effect.succeed({ uploadId: "u-1", sizeBytes, receivedBytes: 0, complete: false }),
  );
  const cancel = vi.fn<UploadSession["cancel"]>(() => Effect.succeed({}));
  const session: UploadSession = { identity: {}, capable, begin, append, get, cancel };
  const next = vi.fn<UploadPort["next"]>(() => Effect.succeed({ ...session, identity: {} }));
  const port: UploadPort = { initial: Effect.succeed(session), next };
  return { session, port, begin, append, get, cancel, next };
}

export const waitForUpload = Effect.fn("TestUploads.waitFor")(function* (predicate: () => boolean) {
  for (let i = 0; i < 2000; i++) {
    if (predicate()) return;
    yield* Effect.yieldNow;
  }
  throw new Error("upload test event did not arrive");
});
