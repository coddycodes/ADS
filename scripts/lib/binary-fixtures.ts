// Minimal ELF64 (little-endian) and 64-bit Mach-O images for tests: only the parts that
// `binaryIdentity` reads, a GNU build-id note or LC_UUID, and a symbol table of a given size.

export function syntheticElf(options: {
  readonly buildId?: string;
  readonly symbols: number;
}): Buffer {
  const note = options.buildId === undefined ? Buffer.alloc(0) : gnuBuildIdNote(options.buildId);
  const symbolTable = Buffer.alloc(options.symbols * 24);
  const sections = [
    { type: 0, offset: 0, size: 0, entrySize: 0 },
    ...(note.length > 0 ? [{ type: 7, offset: 0x40, size: note.length, entrySize: 0 }] : []),
    ...(options.symbols > 0
      ? [{ type: 2, offset: 0x40 + note.length, size: symbolTable.length, entrySize: 24 }]
      : []),
  ];
  const header = Buffer.alloc(0x40);
  header.writeUInt32BE(0x7f454c46, 0);
  header[4] = 2; // ELFCLASS64
  header[5] = 1; // little-endian
  header.writeBigUInt64LE(BigInt(0x40 + note.length + symbolTable.length), 0x28);
  header.writeUInt16LE(0x40, 0x3a);
  header.writeUInt16LE(sections.length, 0x3c);
  const table = Buffer.alloc(sections.length * 0x40);
  sections.forEach((section, index) => {
    table.writeUInt32LE(section.type, index * 0x40 + 4);
    table.writeBigUInt64LE(BigInt(section.offset), index * 0x40 + 0x18);
    table.writeBigUInt64LE(BigInt(section.size), index * 0x40 + 0x20);
    table.writeBigUInt64LE(BigInt(section.entrySize), index * 0x40 + 0x38);
  });
  return Buffer.concat([header, note, symbolTable, table]);
}

function gnuBuildIdNote(buildId: string): Buffer {
  const id = Buffer.from(buildId, "hex");
  const note = Buffer.alloc(16 + ((id.length + 3) & ~3));
  note.writeUInt32LE(4, 0);
  note.writeUInt32LE(id.length, 4);
  note.writeUInt32LE(3, 8); // NT_GNU_BUILD_ID
  note.write("GNU\0", 12, "latin1");
  id.copy(note, 16);
  return note;
}

export function syntheticMachO(options: {
  readonly uuid: string;
  readonly symbols: number;
}): Buffer {
  const image = Buffer.alloc(32 + 24 + 24);
  image.writeUInt32LE(0xfeedfacf, 0);
  image.writeUInt32LE(2, 16);
  image.writeUInt32LE(48, 20);
  image.writeUInt32LE(0x1b, 32); // LC_UUID
  image.writeUInt32LE(24, 36);
  Buffer.from(options.uuid, "hex").copy(image, 40);
  image.writeUInt32LE(0x2, 56); // LC_SYMTAB
  image.writeUInt32LE(24, 60);
  image.writeUInt32LE(options.symbols, 68);
  return image;
}
