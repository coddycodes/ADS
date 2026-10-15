/**
 * Device media frames as the server's media socket sends them:
 * `[kind u8][pts u64 big-endian, µs][Annex-B payload]`, kind 0 config, 1 key, 2 delta.
 */
export type MediaFrameKind = "config" | "key" | "delta";

export interface MediaFrame {
  readonly kind: MediaFrameKind;
  readonly ptsUs: bigint;
  readonly data: Uint8Array;
}

export class MediaFrameError extends Error {}

const FRAME_HEADER_BYTES = 9;
/** The server refuses scrcpy packets over 8 MiB; a frame adds its 9-byte header. */
export const MAX_DEVICE_MEDIA_FRAME_BYTES = 8 * 1024 * 1024 + FRAME_HEADER_BYTES;

const KINDS: ReadonlyArray<MediaFrameKind> = ["config", "key", "delta"];
const NAL_TYPE_SPS = 7;

export function parseMediaFrame(bytes: Uint8Array): MediaFrame {
  if (bytes.length < FRAME_HEADER_BYTES) {
    throw new MediaFrameError(`media frame of ${bytes.length} bytes is shorter than its header`);
  }
  const kind = KINDS[bytes[0]!];
  if (kind === undefined) throw new MediaFrameError(`unknown media frame kind ${bytes[0]}`);
  const ptsUs = new DataView(bytes.buffer, bytes.byteOffset + 1, 8).getBigUint64(0);
  return { kind, ptsUs, data: bytes.subarray(FRAME_HEADER_BYTES) };
}

const hexByte = (byte: number) => byte.toString(16).padStart(2, "0");

/** `avc1.PPCCLL` (profile, constraint flags, level) from an SPS NAL unit, header byte first. */
export function avcCodecString(sps: Uint8Array): string {
  if (sps.length < 4 || (sps[0]! & 0x1f) !== NAL_TYPE_SPS) {
    throw new MediaFrameError("not an H.264 sequence parameter set");
  }
  return `avc1.${hexByte(sps[1]!)}${hexByte(sps[2]!)}${hexByte(sps[3]!)}`;
}

/** The SPS NAL unit of a config unit's Annex-B byte stream. */
export function configSps(annexB: Uint8Array): Uint8Array {
  let index = 0;
  while (index + 3 <= annexB.length) {
    // A start code is 00 00 01 (a 4-byte 00 00 00 01 ends the same way).
    if (annexB[index] !== 0 || annexB[index + 1] !== 0 || annexB[index + 2] !== 1) {
      index += 1;
      continue;
    }
    const start = index + 3;
    let end = start;
    while (
      end + 3 <= annexB.length &&
      !(annexB[end] === 0 && annexB[end + 1] === 0 && annexB[end + 2] === 1)
    ) {
      end += 1;
    }
    if (end + 3 > annexB.length) end = annexB.length;
    // The zero before a 4-byte start code belongs to that start code, not to this unit.
    while (end > start && annexB[end - 1] === 0) end -= 1;
    if (start < end && (annexB[start]! & 0x1f) === NAL_TYPE_SPS) return annexB.subarray(start, end);
    index = end;
  }
  throw new MediaFrameError("config unit has no sequence parameter set");
}
