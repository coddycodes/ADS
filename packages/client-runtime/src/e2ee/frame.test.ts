import { describe, expect, it } from "@effect/vitest";
import transportConstants from "../../../shared/fixtures/e2ee-transport-constants.json" with { type: "json" };

import {
  E2EE_RECORD_FLAG_CONTINUATION,
  E2EE_RECORD_FLAG_CONTROL,
  E2EE_RECORD_FLAG_DEFLATE,
  E2EE_RECORD_FLAG_FINAL,
  E2eeFrameError,
  MAX_E2EE_CHUNK_BYTES,
  MAX_E2EE_LOGICAL_MESSAGE_BYTES,
  MAX_E2EE_PREAUTH_MESSAGE_BYTES,
  MAX_E2EE_RECORDS_PER_MESSAGE,
  plaintextRecords,
  RecordAssembler,
  splitIntoRecords,
} from "./frame.ts";
import { MAX_NOISE_MESSAGE_BYTES } from "./noise.ts";

const recordAt = (records: ReadonlyArray<Uint8Array>, index: number): Uint8Array => {
  const record = records[index];
  if (record === undefined) throw new Error(`missing record ${index}`);
  return record;
};

describe("e2ee record layer", () => {
  it("matches the shared transport constant fixture", () => {
    expect(MAX_NOISE_MESSAGE_BYTES).toBe(transportConstants.maxCiphertextBytes);
    expect(MAX_E2EE_CHUNK_BYTES).toBe(transportConstants.maxChunkBytes);
    expect(MAX_E2EE_LOGICAL_MESSAGE_BYTES).toBe(transportConstants.maxLogicalMessageBytes);
    expect(MAX_E2EE_RECORDS_PER_MESSAGE).toBe(transportConstants.maxRecordsPerMessage);
    expect(MAX_E2EE_PREAUTH_MESSAGE_BYTES).toBe(transportConstants.maxPreauthMessageBytes);
    // Logical write throughput and socket timeout are server-only policies;
    // the Rust parity test asserts those fixture fields.
  });

  it("splits small payloads into one final record", () => {
    const records = splitIntoRecords(Uint8Array.from([1, 2, 3]));
    expect(records).toHaveLength(1);
    const record = recordAt(records, 0);
    expect(record[0]).toBe(E2EE_RECORD_FLAG_FINAL);
    expect(record.slice(1)).toEqual(Uint8Array.from([1, 2, 3]));
  });

  it("represents the empty message as one final empty record", () => {
    const records = splitIntoRecords(new Uint8Array(0));
    expect(records).toHaveLength(1);
    expect(recordAt(records, 0)).toEqual(Uint8Array.of(E2EE_RECORD_FLAG_FINAL));
  });

  it("splits large payloads with continuation flags and reassembles them", () => {
    const payload = new Uint8Array(MAX_E2EE_CHUNK_BYTES * 2 + 7).fill(0xab);
    const records = splitIntoRecords(payload);
    expect(records).toHaveLength(3);
    const first = recordAt(records, 0);
    const second = recordAt(records, 1);
    const third = recordAt(records, 2);
    expect(first[0]).toBe(E2EE_RECORD_FLAG_CONTINUATION);
    expect(second[0]).toBe(E2EE_RECORD_FLAG_CONTINUATION);
    expect(third[0]).toBe(E2EE_RECORD_FLAG_FINAL);
    expect(first).toHaveLength(1 + MAX_E2EE_CHUNK_BYTES);
    const assembler = new RecordAssembler();
    expect(assembler.push(first)).toBeNull();
    expect(assembler.push(second)).toBeNull();
    expect(assembler.push(third)).toEqual(payload);
  });

  it("iterates plaintext records lazily with the same wire shape", () => {
    const payload = new Uint8Array(MAX_E2EE_CHUNK_BYTES * 2 + 7).fill(0xab);
    const records = [...plaintextRecords(payload)];

    expect(records).toHaveLength(3);
    expect(records[0]?.[0]).toBe(E2EE_RECORD_FLAG_CONTINUATION);
    expect(records[1]?.[0]).toBe(E2EE_RECORD_FLAG_CONTINUATION);
    expect(records[2]?.[0]).toBe(E2EE_RECORD_FLAG_FINAL);
    expect(records.map((record) => record.length)).toEqual([
      MAX_E2EE_CHUNK_BYTES + 1,
      MAX_E2EE_CHUNK_BYTES + 1,
      8,
    ]);
  });

  it("the assembler resets between messages", () => {
    const assembler = new RecordAssembler();
    expect(assembler.push(Uint8Array.of(E2EE_RECORD_FLAG_FINAL, 1))).toEqual(Uint8Array.of(1));
    expect(assembler.push(Uint8Array.of(E2EE_RECORD_FLAG_FINAL, 2))).toEqual(Uint8Array.of(2));
  });

  it("caps pre-auth reassembly at 64 KiB", () => {
    const continuation = new Uint8Array(1 + 40 * 1024);
    continuation[0] = E2EE_RECORD_FLAG_CONTINUATION;
    const final = new Uint8Array(1 + 40 * 1024);
    final[0] = E2EE_RECORD_FLAG_FINAL;
    const assembler = new RecordAssembler(MAX_E2EE_PREAUTH_MESSAGE_BYTES);

    expect(() => assembler.push(continuation)).not.toThrow();
    expect(() => assembler.push(final)).toThrow("E2EE reassembly overflow");
  });

  it("retains the authenticated 64 MiB assembler default", () => {
    const continuation = new Uint8Array(1 + 40 * 1024);
    continuation[0] = E2EE_RECORD_FLAG_CONTINUATION;
    const final = new Uint8Array(1 + 40 * 1024);
    final[0] = E2EE_RECORD_FLAG_FINAL;
    const assembler = new RecordAssembler();

    expect(assembler.push(continuation)).toBeNull();
    expect(assembler.push(final)).toHaveLength(80 * 1024);
  });

  it("rejects empty records, unknown flags, and overflow", () => {
    const assembler = new RecordAssembler();
    expect(() => assembler.push(new Uint8Array(0))).toThrow(E2eeFrameError);
    expect(() => assembler.push(Uint8Array.of(0x02, 1))).toThrow(E2eeFrameError);
    const chunk = new Uint8Array(1 + MAX_E2EE_CHUNK_BYTES);
    chunk[0] = E2EE_RECORD_FLAG_CONTINUATION;
    const overflowing = new RecordAssembler();
    const rounds = Math.ceil(MAX_E2EE_LOGICAL_MESSAGE_BYTES / MAX_E2EE_CHUNK_BYTES) + 1;
    expect(() => {
      for (let index = 0; index < rounds; index += 1) {
        overflowing.push(chunk);
      }
    }).toThrow(E2eeFrameError);
  });

  it("rejects empty continuations and more than 2,048 records", () => {
    const emptyContinuation = Uint8Array.of(E2EE_RECORD_FLAG_CONTINUATION);
    expect(() => new RecordAssembler().push(emptyContinuation)).toThrow("empty E2EE continuation");

    const assembler = new RecordAssembler();
    for (let index = 0; index < MAX_E2EE_RECORDS_PER_MESSAGE; index += 1) {
      expect(assembler.push(Uint8Array.of(E2EE_RECORD_FLAG_CONTINUATION, 1))).toBeNull();
    }
    expect(() => assembler.push(Uint8Array.of(E2EE_RECORD_FLAG_FINAL))).toThrow(
      "E2EE record count overflow",
    );
  });

  it("refuses to split payloads beyond the logical cap", () => {
    expect(() => splitIntoRecords(new Uint8Array(MAX_E2EE_LOGICAL_MESSAGE_BYTES + 1))).toThrow(
      E2eeFrameError,
    );
  });
});

describe("RecordAssembler control records", () => {
  const encoder = new TextEncoder();
  const record = (flag: number, text: string) => {
    const bytes = encoder.encode(text);
    const out = new Uint8Array(1 + bytes.length);
    out[0] = flag;
    out.set(bytes, 1);
    return out;
  };

  it("returns a 0x02 record at once without touching the partial message", () => {
    const assembler = new RecordAssembler(undefined, { allowControlRecords: true });
    expect(assembler.push(record(0x01, "hello "))).toBeNull();
    expect(new TextDecoder().decode(assembler.push(record(0x02, "pong"))!)).toBe("pong");
    expect(new TextDecoder().decode(assembler.push(record(0x00, "world"))!)).toBe("hello world");
  });

  it("rejects 0x02 unless control records were negotiated", () => {
    const assembler = new RecordAssembler();
    expect(() => assembler.push(record(E2EE_RECORD_FLAG_CONTROL, "pong"))).toThrow(E2eeFrameError);
  });

  it("refuses a control payload larger than one record", () => {
    const assembler = new RecordAssembler(undefined, { allowControlRecords: true });
    expect(() => assembler.push(record(0x02, "x".repeat(MAX_E2EE_CHUNK_BYTES + 1)))).toThrow(
      E2eeFrameError,
    );
  });

  it("checks the byte limit before returning a control payload", () => {
    const assembler = new RecordAssembler(4, { allowControlRecords: true });
    assembler.push(record(0x01, "abc"));
    expect(() => assembler.push(record(0x02, "de"))).toThrow(E2eeFrameError);
  });

  it("checks the record limit before returning a control payload", () => {
    const assembler = new RecordAssembler(undefined, { allowControlRecords: true });
    for (let i = 0; i < MAX_E2EE_RECORDS_PER_MESSAGE; i += 1) {
      assembler.push(record(0x01, "x"));
    }
    expect(() => assembler.push(record(0x02, "pong"))).toThrow(E2eeFrameError);
  });

  it("rejects an empty control record", () => {
    const assembler = new RecordAssembler(undefined, { allowControlRecords: true });
    expect(() => assembler.push(Uint8Array.of(0x02))).toThrow(E2eeFrameError);
  });

  it("marks a message whose records carry 0x10 as compressed once deflate-v1 is agreed", () => {
    const assembler = new RecordAssembler(undefined, { allowDeflate: true });
    expect(assembler.push(record(E2EE_RECORD_FLAG_DEFLATE | 0x01, "ab"))).toBeNull();
    expect(new TextDecoder().decode(assembler.push(record(E2EE_RECORD_FLAG_DEFLATE, "cd"))!)).toBe(
      "abcd",
    );
    expect(assembler.lastMessageDeflated).toBe(true);
    expect(new TextDecoder().decode(assembler.push(record(0x00, "plain"))!)).toBe("plain");
    expect(assembler.lastMessageDeflated).toBe(false);
  });

  it("reports a control record after a compressed message as uncompressed", () => {
    const assembler = new RecordAssembler(undefined, {
      allowControlRecords: true,
      allowDeflate: true,
    });
    assembler.push(record(E2EE_RECORD_FLAG_DEFLATE, "zz"));
    expect(assembler.lastMessageDeflated).toBe(true);
    assembler.push(record(E2EE_RECORD_FLAG_DEFLATE | 0x01, "ab"));
    expect(new TextDecoder().decode(assembler.push(record(0x02, "pong"))!)).toBe("pong");
    expect(assembler.lastMessageDeflated).toBe(false);
    assembler.push(record(E2EE_RECORD_FLAG_DEFLATE, "cd"));
    expect(assembler.lastMessageDeflated).toBe(true);
  });

  it("rejects 0x10 unless deflate-v1 was negotiated", () => {
    const assembler = new RecordAssembler();
    expect(() => assembler.push(record(E2EE_RECORD_FLAG_DEFLATE, "ab"))).toThrow(E2eeFrameError);
  });

  it("rejects a message whose records disagree on compression", () => {
    const assembler = new RecordAssembler(undefined, { allowDeflate: true });
    assembler.push(record(E2EE_RECORD_FLAG_DEFLATE | 0x01, "ab"));
    expect(() => assembler.push(record(0x00, "cd"))).toThrow(E2eeFrameError);
  });
});
