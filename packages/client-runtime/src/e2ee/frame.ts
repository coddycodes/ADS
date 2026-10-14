import { MAX_NOISE_MESSAGE_BYTES, NOISE_TAG_BYTES } from "./noise.ts";

export const E2EE_RECORD_FLAG_FINAL = 0x00;
export const E2EE_RECORD_FLAG_CONTINUATION = 0x01;
export const E2EE_RECORD_FLAG_CONTROL = 0x02;
/** Set on every record of a raw-deflate compressed message (`deflate-v1`). */
export const E2EE_RECORD_FLAG_DEFLATE = 0x10;
export const MAX_E2EE_CHUNK_BYTES = MAX_NOISE_MESSAGE_BYTES - NOISE_TAG_BYTES - 1;
export const MAX_E2EE_LOGICAL_MESSAGE_BYTES = 64 * 1024 * 1024;
export const MAX_E2EE_PREAUTH_MESSAGE_BYTES = 64 * 1024;
export const MAX_E2EE_RECORDS_PER_MESSAGE = 2_048;

export class E2eeFrameError extends Error {}

export const plaintextRecords = (plaintext: Uint8Array): Iterable<Uint8Array> => {
  if (plaintext.length > MAX_E2EE_LOGICAL_MESSAGE_BYTES) {
    throw new E2eeFrameError(`outbound message of ${plaintext.length} bytes exceeds the E2EE cap`);
  }

  return (function* () {
    if (plaintext.length === 0) {
      yield Uint8Array.of(E2EE_RECORD_FLAG_FINAL);
      return;
    }

    for (let offset = 0; offset < plaintext.length; offset += MAX_E2EE_CHUNK_BYTES) {
      const chunk = plaintext.subarray(offset, offset + MAX_E2EE_CHUNK_BYTES);
      const final = offset + MAX_E2EE_CHUNK_BYTES >= plaintext.length;
      const record = new Uint8Array(1 + chunk.length);
      record[0] = final ? E2EE_RECORD_FLAG_FINAL : E2EE_RECORD_FLAG_CONTINUATION;
      record.set(chunk, 1);
      yield record;
    }
  })();
};

export const splitIntoRecords = (plaintext: Uint8Array): Array<Uint8Array> => [
  ...plaintextRecords(plaintext),
];

export interface RecordAssemblerOptions {
  /** Accept `0x02` stand-alone control records between the records of another message. */
  readonly allowControlRecords?: boolean;
  /** Accept compressed messages, whose records carry `0x10` (`deflate-v1`). */
  readonly allowDeflate?: boolean;
}

export class RecordAssembler {
  private parts: Array<Uint8Array> = [];
  private assembledBytes = 0;
  private recordCount = 0;
  private readonly maxMessageBytes: number;
  private readonly allowControlRecords: boolean;
  private readonly allowDeflate: boolean;
  /** Whether the message in progress is compressed; `null` before its first record. */
  private deflated: boolean | null = null;
  /** Whether the message the last `push` returned is compressed. */
  lastMessageDeflated = false;

  constructor(maxMessageBytes = MAX_E2EE_LOGICAL_MESSAGE_BYTES, options?: RecordAssemblerOptions) {
    this.maxMessageBytes = maxMessageBytes;
    this.allowControlRecords = options?.allowControlRecords ?? false;
    this.allowDeflate = options?.allowDeflate ?? false;
  }

  push(recordPlaintext: Uint8Array): Uint8Array | null {
    if (recordPlaintext.length === 0) throw new E2eeFrameError("empty E2EE record");

    const flag = recordPlaintext[0]!;
    const chunk = recordPlaintext.subarray(1);
    // Apply the existing 64 MiB / 2,048-record bounds before any early
    // control return. The control itself is one complete record and does
    // not mutate the in-progress data message.
    if (this.recordCount >= MAX_E2EE_RECORDS_PER_MESSAGE) {
      throw new E2eeFrameError("E2EE record count overflow");
    }
    if (this.assembledBytes + chunk.length > this.maxMessageBytes) {
      throw new E2eeFrameError("E2EE reassembly overflow");
    }
    if (flag === E2EE_RECORD_FLAG_CONTROL && this.allowControlRecords) {
      if (chunk.length > MAX_E2EE_CHUNK_BYTES) {
        throw new E2eeFrameError("E2EE control record overflow");
      }
      if (chunk.length === 0) throw new E2eeFrameError("empty E2EE control record");
      // A control message is complete in one record and leaves the partial message alone.
      this.lastMessageDeflated = false;
      return chunk.slice();
    }
    const deflated = this.allowDeflate && (flag & E2EE_RECORD_FLAG_DEFLATE) !== 0;
    const recordFlag = deflated ? flag & ~E2EE_RECORD_FLAG_DEFLATE : flag;
    // Every record of a message agrees on compression.
    if (this.deflated !== null && this.deflated !== deflated) {
      throw new E2eeFrameError("E2EE records disagree on compression");
    }
    if (recordFlag === E2EE_RECORD_FLAG_CONTINUATION && chunk.length === 0) {
      throw new E2eeFrameError("empty E2EE continuation");
    }
    this.recordCount += 1;
    this.deflated = deflated;
    if (recordFlag === E2EE_RECORD_FLAG_CONTINUATION) {
      if (chunk.length > 0) this.parts.push(chunk.slice());
      this.assembledBytes += chunk.length;
      return null;
    }
    if (recordFlag !== E2EE_RECORD_FLAG_FINAL) {
      throw new E2eeFrameError(`unknown E2EE record flag ${String(flag)}`);
    }

    const message = new Uint8Array(this.assembledBytes + chunk.length);
    let offset = 0;
    for (const part of this.parts) {
      message.set(part, offset);
      offset += part.length;
    }
    message.set(chunk, offset);
    this.parts = [];
    this.assembledBytes = 0;
    this.recordCount = 0;
    this.lastMessageDeflated = deflated;
    this.deflated = null;
    return message;
  }
}
