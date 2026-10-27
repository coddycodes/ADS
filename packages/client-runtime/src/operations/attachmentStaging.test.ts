import { UploadError } from "@coddy/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Fiber from "effect/Fiber";
import * as Clock from "effect/Clock";
import * as TestClock from "effect/testing/TestClock";

import {
  encodedAttachmentCharacters,
  keepStagedAttachmentsAliveWithPort,
  shouldStageAttachments,
  stageAttachmentsWithPort,
  type AttachmentSource,
} from "./attachmentStaging.ts";
import { lost, uploadHarness, waitForUpload } from "./uploadStager.testSupport.ts";
import type { UploadProgress } from "./uploadStager.ts";

function source(size: number, id: string, fileType = "text/plain"): AttachmentSource {
  const file = new Blob([new Uint8Array(size)], { type: fileType });
  return { type: "file", id, name: `${id}.txt`, mimeType: "text/plain", sizeBytes: size, file };
}

describe("attachment staging", () => {
  it.effect("keeps admission-owned stages alive until its owner interrupts and joins it", () =>
    Effect.gen(function* () {
      const h = uploadHarness();
      const fiber = yield* keepStagedAttachmentsAliveWithPort(
        () => [{ uploadId: "admitting" }],
        h.port,
      ).pipe(Effect.forkChild);
      yield* TestClock.adjust("11 minutes");
      expect(
        h.get.mock.calls.filter(([input]) => input.uploadId === "admitting").length,
      ).toBeGreaterThanOrEqual(10);
      expect(h.cancel).not.toHaveBeenCalled();
      yield* Fiber.interrupt(fiber);
      const calls = h.get.mock.calls.length;
      yield* TestClock.adjust("2 minutes");
      expect(h.get.mock.calls.length).toBe(calls);
      expect(h.cancel).not.toHaveBeenCalled();
    }),
  );
  it.effect(
    "keeps later completed files alive while an earlier missing stage is being restaged",
    () =>
      Effect.gen(function* () {
        const h = uploadHarness();
        let lastTouch = 0;
        h.append.mockImplementation((input) =>
          Effect.gen(function* () {
            yield* Effect.sleep("11 minutes");
            return { receivedBytes: input.offset + Buffer.from(input.data, "base64").length };
          }),
        );
        h.get.mockImplementation((input) =>
          Effect.gen(function* () {
            if (input.uploadId === "old-a")
              return yield* new UploadError({ reason: "not_found", message: "the upload expired" });
            if (input.uploadId === "old-b") {
              const now = yield* Clock.currentTimeMillis;
              if (now - lastTouch >= 10 * 60_000)
                throw new Error("the other completed file expired during restaging");
              lastTouch = now;
            }
            return {
              uploadId: input.uploadId,
              sizeBytes: 120_000,
              receivedBytes: 120_000,
              complete: true,
            };
          }),
        );
        const reusable = ["a", "b"].map((id) => ({
          type: "file" as const,
          id,
          name: `${id}.txt`,
          mimeType: "text/plain",
          sizeBytes: 120_000,
          uploadId: `old-${id}`,
        }));
        const fiber = yield* stageAttachmentsWithPort(
          { attachments: [source(120_000, "a"), source(120_000, "b")], reusable },
          h.port,
        ).pipe(Effect.forkChild);
        yield* waitForUpload(() => h.append.mock.calls.length === 2);
        yield* TestClock.adjust("11 minutes");
        const result = yield* Fiber.join(fiber);
        expect(result).toMatchObject({
          _tag: "staged",
          attachments: [{ uploadId: "u-1" }, { uploadId: "old-b" }],
        });
        expect(
          h.get.mock.calls.filter(([input]) => input.uploadId === "old-b").length,
        ).toBeGreaterThanOrEqual(10);
      }),
  );
  it.effect("bounds keepalive sweeps and joins the keeper when an upload is cancelled", () =>
    Effect.gen(function* () {
      const h = uploadHarness();
      let activeGets = 0;
      let maximumGets = 0;
      h.append.mockImplementation((input) =>
        input.uploadId === "u-2"
          ? Effect.never
          : Effect.succeed({
              receivedBytes: input.offset + Buffer.from(input.data, "base64").length,
            }),
      );
      h.get.mockImplementation((input) =>
        Effect.gen(function* () {
          activeGets++;
          maximumGets = Math.max(maximumGets, activeGets);
          yield* Effect.sleep("90 seconds");
          return {
            uploadId: input.uploadId,
            sizeBytes: 120_000,
            receivedBytes: 120_000,
            complete: true,
          };
        }).pipe(
          Effect.ensuring(
            Effect.sync(() => {
              activeGets--;
            }),
          ),
        ),
      );
      const fiber = yield* stageAttachmentsWithPort(
        { attachments: [source(120_000, "a"), source(120_000, "b")] },
        h.port,
      ).pipe(Effect.forkChild);
      yield* waitForUpload(() => h.append.mock.calls.some(([input]) => input.uploadId === "u-2"));
      yield* TestClock.adjust("61 seconds");
      expect(activeGets).toBe(1);
      yield* TestClock.adjust("5 seconds");
      expect(activeGets).toBe(0);
      yield* TestClock.adjust("60 seconds");
      expect(activeGets).toBe(1);
      expect(maximumGets).toBe(1);
      yield* Fiber.interrupt(fiber);
      expect(activeGets).toBe(0);
      const callsAfterCancel = h.get.mock.calls.length;
      yield* TestClock.adjust("2 minutes");
      expect(h.get.mock.calls.length).toBe(callsAfterCancel);
      expect(h.cancel).toHaveBeenCalledWith({ uploadId: "u-1" });
      expect(h.cancel).toHaveBeenCalledWith({ uploadId: "u-2" });
    }),
  );

  it.effect(
    "shares a file's single restart budget between active resume and completed-stage validation",
    () =>
      Effect.gen(function* () {
        const h = uploadHarness();
        h.append.mockImplementation((input) =>
          input.uploadId === "u-1"
            ? Effect.fail(lost)
            : Effect.succeed({
                receivedBytes: input.offset + Buffer.from(input.data, "base64").length,
              }),
        );
        h.get.mockImplementation((input) =>
          input.uploadId === "u-1" || input.uploadId === "u-2"
            ? Effect.fail(new UploadError({ reason: "not_found", message: "the upload expired" }))
            : Effect.succeed({
                uploadId: input.uploadId,
                sizeBytes: 120_000,
                receivedBytes: 120_000,
                complete: true,
              }),
        );
        const result = yield* Effect.exit(
          stageAttachmentsWithPort(
            { attachments: [source(120_000, "a"), source(120_000, "b")] },
            h.port,
          ),
        );
        expect(Exit.isFailure(result)).toBe(true);
        expect(h.begin).toHaveBeenCalledTimes(3);
        expect(h.cancel).toHaveBeenCalledWith({ uploadId: "u-2" });
        expect(h.cancel).toHaveBeenCalledWith({ uploadId: "u-3" });
      }),
  );
  it.effect("validates reused complete stages without reading or uploading bytes again", () =>
    Effect.gen(function* () {
      const h = uploadHarness();
      h.get.mockImplementation((input) =>
        Effect.succeed({
          uploadId: input.uploadId,
          sizeBytes: 120_000,
          receivedBytes: 120_000,
          complete: true,
        }),
      );
      const reusable = ["a", "b"].map((id) => ({
        type: "file" as const,
        id,
        name: `${id}.txt`,
        mimeType: "text/plain",
        sizeBytes: 120_000,
        uploadId: `old-${id}`,
      }));
      const result = yield* stageAttachmentsWithPort(
        { attachments: [source(120_000, "a"), source(120_000, "b")], reusable },
        h.port,
      );
      expect(result).toEqual({ _tag: "staged", attachments: reusable });
      expect(h.begin).not.toHaveBeenCalled();
      expect(h.append).not.toHaveBeenCalled();
      expect(h.get).toHaveBeenCalledTimes(2);
    }),
  );
  it.effect("touches completed stages while a later file takes more than ten minutes", () =>
    Effect.gen(function* () {
      const h = uploadHarness();
      const received = new Map<string, number>();
      const lastTouch = new Map<string, number>();
      h.append.mockImplementation((input) =>
        Effect.gen(function* () {
          if (input.uploadId === "u-2") yield* Effect.sleep("11 minutes");
          const bytes = input.offset + Buffer.from(input.data, "base64").length;
          received.set(input.uploadId, bytes);
          lastTouch.set(input.uploadId, yield* Clock.currentTimeMillis);
          return { receivedBytes: bytes };
        }),
      );
      h.get.mockImplementation((input) =>
        Effect.gen(function* () {
          const now = yield* Clock.currentTimeMillis;
          if (now - (lastTouch.get(input.uploadId) ?? 0) >= 10 * 60_000)
            throw new Error("a completed stage expired before binding");
          lastTouch.set(input.uploadId, now);
          const count = received.get(input.uploadId) ?? 0;
          return {
            uploadId: input.uploadId,
            sizeBytes: 120_000,
            receivedBytes: count,
            complete: count === 120_000,
          };
        }),
      );
      const fiber = yield* stageAttachmentsWithPort(
        { attachments: [source(120_000, "a"), source(120_000, "b")] },
        h.port,
      ).pipe(Effect.forkChild);
      yield* waitForUpload(() => h.append.mock.calls.some(([input]) => input.uploadId === "u-2"));
      yield* TestClock.adjust("11 minutes");
      const result = yield* Fiber.join(fiber);
      expect(result._tag).toBe("staged");
      expect(
        h.get.mock.calls.filter(([input]) => input.uploadId === "u-1").length,
      ).toBeGreaterThanOrEqual(10);
      expect(h.begin).toHaveBeenCalledTimes(2);
      const callsAfterSuccess = h.get.mock.calls.length;
      yield* TestClock.adjust("2 minutes");
      expect(h.get.mock.calls.length).toBe(callsAfterSuccess);
    }),
  );

  it.effect(
    "restages an earlier completed file once when the server restarts during a later file",
    () =>
      Effect.gen(function* () {
        const h = uploadHarness();
        let restarted = false;
        h.append.mockImplementation((input) =>
          Effect.sync(() => {
            if (input.uploadId === "u-2") restarted = true;
            return { receivedBytes: input.offset + Buffer.from(input.data, "base64").length };
          }),
        );
        h.get.mockImplementation((input) =>
          restarted && input.uploadId === "u-1"
            ? Effect.fail(new UploadError({ reason: "not_found", message: "the upload expired" }))
            : Effect.succeed({
                uploadId: input.uploadId,
                sizeBytes: 120_000,
                receivedBytes: 120_000,
                complete: true,
              }),
        );
        const result = yield* stageAttachmentsWithPort(
          { attachments: [source(120_000, "a"), source(120_000, "b")] },
          h.port,
        );
        expect(h.begin).toHaveBeenCalledTimes(3);
        expect(result).toMatchObject({
          _tag: "staged",
          attachments: [{ uploadId: "u-3" }, { uploadId: "u-2" }],
        });
      }),
  );
  it("uses the complete turn's encoded size and the carrying session capability", () => {
    expect(shouldStageAttachments(true, [source(120_000, "a")])).toBe(false);
    expect(shouldStageAttachments(true, [source(120_000, "a"), source(120_000, "b")])).toBe(true);
    expect(shouldStageAttachments(false, [source(10 * 1024 * 1024, "large")])).toBe(false);
    const size = 3 * Math.floor((256 * 1024 - "data:text/plain;base64,".length) / 4);
    expect(encodedAttachmentCharacters(source(size, "edge"))).toBeLessThanOrEqual(256 * 1024);
    expect(shouldStageAttachments(true, [source(size, "edge")])).toBe(false);
    expect(shouldStageAttachments(true, [source(size + 3, "over")])).toBe(true);
    expect(encodedAttachmentCharacters(source(3, "default", ""))).toBe(
      "data:application/octet-stream;base64,".length + 4,
    );
  });

  it.effect("returns inline without upload RPCs on a server that lacks the capability", () =>
    Effect.gen(function* () {
      const h = uploadHarness(false);
      expect(
        yield* stageAttachmentsWithPort({ attachments: [source(1024 * 1024, "large")] }, h.port),
      ).toEqual({ _tag: "inline" });
      expect(h.begin).not.toHaveBeenCalled();
    }),
  );

  it.effect("stages each attachment sequentially and totals acknowledged raw progress", () =>
    Effect.gen(function* () {
      const h = uploadHarness();
      const order: string[] = [];
      const progress: UploadProgress[] = [];
      h.begin.mockImplementation(() =>
        Effect.sync(() => {
          const id = `u-${order.filter((event) => event === "begin").length + 1}`;
          order.push("begin");
          return { uploadId: id, exists: false };
        }),
      );
      h.append.mockImplementation((input) =>
        Effect.sync(() => {
          if (input.sha256) order.push("complete");
          return { receivedBytes: input.offset + Buffer.from(input.data, "base64").length };
        }),
      );
      h.get.mockImplementation((input) =>
        Effect.succeed({
          uploadId: input.uploadId,
          sizeBytes: 120_000,
          receivedBytes: 120_000,
          complete: true,
        }),
      );
      const result = yield* stageAttachmentsWithPort(
        {
          attachments: [source(120_000, "a"), source(120_000, "b")],
          onProgress: (p) => progress.push(p),
        },
        h.port,
      );
      expect(order).toEqual(["begin", "complete", "begin", "complete"]);
      expect(result).toMatchObject({
        _tag: "staged",
        attachments: [
          { id: "a", uploadId: "u-1" },
          { id: "b", uploadId: "u-2" },
        ],
      });
      expect(progress.at(-1)).toMatchObject({
        sentBytes: 240_000,
        totalBytes: 240_000,
        phase: "uploading",
      });
      expect(h.cancel).not.toHaveBeenCalled();
    }),
  );

  it.effect("releases earlier unbound stages when the next file fails", () =>
    Effect.gen(function* () {
      const h = uploadHarness();
      h.append.mockImplementation((input) =>
        input.uploadId === "u-2"
          ? Effect.fail(
              new UploadError({ reason: "digest", message: "the file changed during upload" }),
            )
          : Effect.succeed({
              receivedBytes: input.offset + Buffer.from(input.data, "base64").length,
            }),
      );
      const result = yield* Effect.exit(
        stageAttachmentsWithPort(
          { attachments: [source(120_000, "a"), source(120_000, "b")] },
          h.port,
        ),
      );
      expect(Exit.isFailure(result)).toBe(true);
      expect(h.cancel).toHaveBeenCalledWith({ uploadId: "u-1" });
      expect(h.cancel).toHaveBeenCalledWith({ uploadId: "u-2" });
    }),
  );
});
