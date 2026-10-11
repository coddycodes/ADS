import { describe, expect, it } from "vite-plus/test";

import { syntheticElf, syntheticMachO } from "./binary-fixtures.ts";
import { binaryIdentity } from "./binary-identity.ts";

describe("binary identity", () => {
  it("reads an ELF's GNU build-id and the size of its symbol table", () => {
    const buildId = "4f9922ce3dc9648187546db7b7f2e76ed45a41f5";
    expect(binaryIdentity(syntheticElf({ buildId, symbols: 3 }))).toEqual({
      id: buildId,
      symbols: 3,
    });
    // Stripping drops `.symtab` and keeps the build-id note.
    expect(binaryIdentity(syntheticElf({ buildId, symbols: 0 }))).toEqual({
      id: buildId,
      symbols: 0,
    });
  });

  it("reads a Mach-O LC_UUID and its symbol count", () => {
    const uuid = "0e9252582cde9e06d4ba17fe44570dad";
    expect(binaryIdentity(syntheticMachO({ uuid, symbols: 12 }))).toEqual({
      id: uuid,
      symbols: 12,
    });
  });

  it("has no identity for a PE, an unknown file, or an ELF without a build-id", () => {
    expect(binaryIdentity(Buffer.from("MZ\x90\x00 portable executable"))).toBeUndefined();
    expect(binaryIdentity(Buffer.from("plain text"))).toBeUndefined();
    expect(binaryIdentity(Buffer.alloc(2))).toBeUndefined();
    expect(binaryIdentity(syntheticElf({ symbols: 3 }))).toBeUndefined();
  });
});
