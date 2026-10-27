import { UploadError, type UploadAppendInput } from "@coddy/contracts";
import { describe, expect, it } from "@effect/vitest";
import { sha256 } from "@noble/hashes/sha2.js";
import * as Cause from "effect/Cause";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Fiber from "effect/Fiber";
import * as TestClock from "effect/testing/TestClock";

import { nextUploadChunkBytes, stageUpload, type UploadProgress } from "./uploadStager.ts";
import {
  body,
  fileTarget,
  lost,
  uploadHarness,
  waitForUpload,
} from "./uploadStager.testSupport.ts";

const digest = (bytes: Uint8Array) => Buffer.from(sha256(bytes)).toString("hex");

describe("upload stager", () => {
  it.effect(
    "follows one replacement session after a lost begin reply, under the file restart budget",
    () =>
      Effect.gen(function* () {
        const h = uploadHarness(true, 3);
        let begins = 0;
        h.begin.mockImplementation(() =>
          ++begins === 1 ? Effect.fail(lost) : Effect.succeed({ uploadId: "u-2", exists: false }),
        );
        const result = yield* stageUpload(
          { target: fileTarget, file: body(3), fileName: "a.txt" },
          h.port,
        );
        expect(result.uploadId).toBe("u-2");
        expect(h.begin).toHaveBeenCalledTimes(2);
        expect(h.next).toHaveBeenCalledOnce();
      }),
  );
  it("bounds measured chunk adaptation in both directions", () => {
    expect(nextUploadChunkBytes(64 * 1024, 4000)).toBe(32 * 1024);
    expect(nextUploadChunkBytes(64 * 1024, 1)).toBe(128 * 1024);
    expect(nextUploadChunkBytes(16 * 1024, 8000)).toBe(16 * 1024);
    expect(nextUploadChunkBytes(1024 * 1024, 1)).toBe(1024 * 1024);
  });

  it.effect(
    "keeps two appends outstanding, reports acknowledged bytes and supplies the full digest",
    () =>
      Effect.gen(function* () {
        const h = uploadHarness();
        const gate = yield* Deferred.make<void>();
        const sent: UploadAppendInput[] = [];
        const progress: UploadProgress[] = [];
        let active = 0;
        let maximum = 0;
        h.append.mockImplementation((input) =>
          Effect.gen(function* () {
            sent.push(input);
            active++;
            maximum = Math.max(maximum, active);
            yield* Deferred.await(gate);
            active--;
            return { receivedBytes: input.offset + Buffer.from(input.data, "base64").length };
          }),
        );
        const file = body(256 * 1024);
        const fiber = yield* stageUpload(
          { target: fileTarget, file, fileName: "a.txt", onProgress: (p) => progress.push(p) },
          h.port,
        ).pipe(Effect.forkChild);
        yield* waitForUpload(() => sent.length === 2);
        expect(maximum).toBe(2);
        expect(progress.every((p) => p.sentBytes === 0)).toBe(true);
        yield* Deferred.succeed(gate, undefined);
        yield* Fiber.join(fiber);
        expect(maximum).toBe(2);
        expect(Buffer.from(sent[0]!.data, "base64").length).toBe(64 * 1024);
        const bytes = new Uint8Array(yield* Effect.promise(() => file.arrayBuffer()));
        expect(sent.at(-1)!.sha256).toBe(digest(bytes));
        expect(progress.at(-1)).toMatchObject({ sentBytes: file.size, totalBytes: file.size });
        expect(h.cancel).not.toHaveBeenCalled();
      }),
  );

  it.effect("resumes a nonzero acknowledged checkpoint with the correct incremental digest", () =>
    Effect.gen(function* () {
      const h = uploadHarness();
      let cut = false;
      const afterCut: UploadAppendInput[] = [];
      h.append.mockImplementation((input) =>
        Effect.suspend(() => {
          if (!cut && input.offset > 0) {
            cut = true;
            return Effect.fail(lost);
          }
          if (cut) afterCut.push(input);
          return Effect.succeed({
            receivedBytes: input.offset + Buffer.from(input.data, "base64").length,
          });
        }),
      );
      h.get.mockReturnValue(
        Effect.succeed({
          uploadId: "u-1",
          sizeBytes: 256 * 1024,
          receivedBytes: 64 * 1024,
          complete: false,
        }),
      );
      const progress: UploadProgress[] = [];
      const file = body(256 * 1024);
      yield* stageUpload(
        { target: fileTarget, file, fileName: "a.txt", onProgress: (p) => progress.push(p) },
        h.port,
      );
      expect(h.begin).toHaveBeenCalledTimes(1);
      expect(h.next).toHaveBeenCalledOnce();
      expect(afterCut[0]!.offset).toBe(64 * 1024);
      expect(progress.some((p) => p.phase === "reconnecting")).toBe(true);
      expect(afterCut.at(-1)!.sha256).toBe(
        digest(new Uint8Array(yield* Effect.promise(() => file.arrayBuffer()))),
      );
    }),
  );

  it.effect("uses get after a lost completion reply instead of uploading the file again", () =>
    Effect.gen(function* () {
      const h = uploadHarness(true, 3);
      h.append.mockReturnValue(Effect.fail(lost));
      h.get.mockReturnValue(
        Effect.succeed({ uploadId: "u-1", sizeBytes: 3, receivedBytes: 3, complete: true }),
      );
      const result = yield* stageUpload(
        { target: fileTarget, file: body(3), fileName: "a.txt" },
        h.port,
      );
      expect(result.uploadId).toBe("u-1");
      expect(h.begin).toHaveBeenCalledOnce();
      expect(h.append).toHaveBeenCalledOnce();
    }),
  );

  it.effect("restarts a disappeared upload once, then fails and cleans it up", () =>
    Effect.gen(function* () {
      const h = uploadHarness(true, 3);
      h.append.mockReturnValue(Effect.fail(lost));
      h.get.mockReturnValue(
        Effect.fail(new UploadError({ reason: "not_found", message: "the upload expired" })),
      );
      const result = yield* Effect.exit(
        stageUpload({ target: fileTarget, file: body(3), fileName: "a.txt" }, h.port),
      );
      expect(Exit.isFailure(result)).toBe(true);
      expect(h.begin).toHaveBeenCalledTimes(2);
      expect(h.get).toHaveBeenCalledTimes(2);
      expect(h.cancel).toHaveBeenCalledWith({ uploadId: "u-2" });
    }),
  );

  it.effect("does not probe when the replacement session loses the capability", () =>
    Effect.gen(function* () {
      const h = uploadHarness(true, 3);
      h.append.mockReturnValue(Effect.fail(lost));
      h.next.mockReturnValue(Effect.succeed({ ...h.session, identity: {}, capable: false }));
      const result = yield* Effect.exit(
        stageUpload({ target: fileTarget, file: body(3), fileName: "a.txt" }, h.port),
      );
      expect(Exit.isFailure(result)).toBe(true);
      expect(h.get).not.toHaveBeenCalled();
      expect(h.begin).toHaveBeenCalledOnce();
    }),
  );

  it.effect("Cancel remains interrupted and releases the stage without waiting for reconnect", () =>
    Effect.gen(function* () {
      const h = uploadHarness(true, 3);
      h.append.mockReturnValue(Effect.never);
      const fiber = yield* stageUpload(
        { target: fileTarget, file: body(3), fileName: "a.txt" },
        h.port,
      ).pipe(Effect.forkChild);
      yield* waitForUpload(() => h.append.mock.calls.length > 0);
      yield* Fiber.interrupt(fiber);
      const result = yield* Fiber.await(fiber);
      expect(Exit.isFailure(result) && Cause.hasInterruptsOnly(result.cause)).toBe(true);
      expect(h.cancel).toHaveBeenCalledExactlyOnceWith({ uploadId: "u-1" });
      expect(h.next).not.toHaveBeenCalled();
    }),
  );

  it.effect("rejects a resume offset outside the retained chunk boundaries", () =>
    Effect.gen(function* () {
      const h = uploadHarness();
      h.append.mockReturnValue(Effect.fail(lost));
      h.get.mockReturnValue(
        Effect.succeed({
          uploadId: "u-1",
          sizeBytes: 256 * 1024,
          receivedBytes: 17,
          complete: false,
        }),
      );
      const result = yield* Effect.exit(
        stageUpload({ target: fileTarget, file: body(256 * 1024), fileName: "a.txt" }, h.port),
      );
      expect(Exit.isFailure(result)).toBe(true);
      expect(h.begin).toHaveBeenCalledOnce();
      expect(h.cancel).toHaveBeenCalledOnce();
    }),
  );

  it.effect("adapts the scheduler from two actual slow acknowledgements", () =>
    Effect.gen(function* () {
      const h = uploadHarness();
      const sizes: number[] = [];
      h.append.mockImplementation((input) =>
        Effect.gen(function* () {
          const size = Buffer.from(input.data, "base64").length;
          sizes.push(size);
          yield* Effect.sleep("4 seconds");
          return { receivedBytes: input.offset + size };
        }),
      );
      const fiber = yield* stageUpload(
        { target: fileTarget, file: body(256 * 1024), fileName: "a.txt" },
        h.port,
      ).pipe(Effect.forkChild);
      yield* waitForUpload(() => sizes.length === 2);
      yield* TestClock.adjust("4 seconds");
      yield* waitForUpload(() => sizes.length >= 3);
      expect(sizes.slice(0, 2)).toEqual([64 * 1024, 64 * 1024]);
      expect(sizes[2]).toBeLessThanOrEqual(32 * 1024);
      expect(sizes[2]).toBeGreaterThanOrEqual(16 * 1024);
      yield* Fiber.interrupt(fiber);
    }),
  );
});
