/**
 * What identifies one build of an executable across the copies a bundler makes, which it may
 * patch (Tauri's bundle-type marker, linuxdeploy's RPATH) or re-sign: the debug ID that
 * minidumps and Breakpad symbols share (ELF GNU build-id, Mach-O LC_UUID) and the size of the
 * symbol table, which stripping removes. PE files have no identity here.
 */
export interface BinaryIdentity {
  readonly id: string;
  readonly symbols: number;
}

export function binaryIdentity(bytes: Buffer): BinaryIdentity | undefined {
  try {
    if (bytes.readUInt32BE(0) === 0x7f454c46) return elfIdentity(bytes);
    if (bytes.readUInt32LE(0) === 0xfeedfacf) return machOIdentity(bytes);
  } catch {
    // A truncated header or table: not a binary this check can identify.
  }
  return undefined;
}

// ELF64, little-endian, as every Linux release target is.
function elfIdentity(bytes: Buffer): BinaryIdentity | undefined {
  if (bytes[4] !== 2 || bytes[5] !== 1) return undefined;
  const table = Number(bytes.readBigUInt64LE(0x28));
  const entrySize = bytes.readUInt16LE(0x3a);
  let id: string | undefined;
  let symbols = 0;
  for (let index = 0; index < bytes.readUInt16LE(0x3c); index += 1) {
    const header = table + index * entrySize;
    const type = bytes.readUInt32LE(header + 4);
    const offset = Number(bytes.readBigUInt64LE(header + 0x18));
    const size = Number(bytes.readBigUInt64LE(header + 0x20));
    if (type === 2) symbols += size / Number(bytes.readBigUInt64LE(header + 0x38)); // SHT_SYMTAB
    if (type === 7) id ??= gnuBuildId(bytes.subarray(offset, offset + size)); // SHT_NOTE
  }
  return id === undefined ? undefined : { id, symbols };
}

function gnuBuildId(notes: Buffer): string | undefined {
  const align = (size: number) => (size + 3) & ~3;
  for (let at = 0; at + 12 <= notes.length;) {
    const nameSize = notes.readUInt32LE(at);
    const descriptionSize = notes.readUInt32LE(at + 4);
    const description = at + 12 + align(nameSize);
    if (
      notes.readUInt32LE(at + 8) === 3 && // NT_GNU_BUILD_ID
      notes.toString("latin1", at + 12, at + 12 + nameSize) === "GNU\0"
    ) {
      return notes.toString("hex", description, description + descriptionSize);
    }
    at = description + align(descriptionSize);
  }
  return undefined;
}

// 64-bit Mach-O, as both macOS release targets are.
function machOIdentity(bytes: Buffer): BinaryIdentity | undefined {
  let id: string | undefined;
  let symbols = 0;
  for (let index = 0, at = 32; index < bytes.readUInt32LE(16); index += 1) {
    const command = bytes.readUInt32LE(at);
    if (command === 0x1b) id = bytes.toString("hex", at + 8, at + 24); // LC_UUID
    if (command === 0x2) symbols = bytes.readUInt32LE(at + 12); // LC_SYMTAB nsyms
    at += bytes.readUInt32LE(at + 4);
  }
  return id === undefined ? undefined : { id, symbols };
}
