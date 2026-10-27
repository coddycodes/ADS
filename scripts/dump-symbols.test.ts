// @effect-diagnostics nodeBuiltinImport:off - These tests read a real ZIP written to a temporary directory.
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as NodeZlib from "node:zlib";

import { afterEach, describe, expect, it } from "vite-plus/test";

import {
  DumpSymbolsError,
  assertUsableSymbols,
  parseDumpSymbolsArguments,
  symbolStorePath,
  writeZip,
} from "./dump-symbols.ts";

const temporaryRoots: string[] = [];

afterEach(() => {
  for (const root of temporaryRoots.splice(0)) {
    NodeFS.rmSync(root, { recursive: true, force: true });
  }
});

const LINUX_SYM = [
  "MODULE Linux x86_64 A83437E178428CE331F11613797F1E660 crash-smoke",
  "INFO CODE_ID E13734A8427817CE31F11613797F1E66",
  "FILE 0 /work/apps/server/src/bin/crash_smoke.rs",
  "FUNC 1000 20 0 crash_smoke::crash_here_in_named_function",
  "1000 20 42 0",
  "STACK CFI INIT 1000 20 .cfa: $rsp 8 + .ra: .cfa -8 + ^",
  "",
].join("\n");

describe("dump-symbols", () => {
  it("rejects a missing --binary", () => {
    expect(() =>
      parseDumpSymbolsArguments(["--module-name", "crash-smoke", "--out", "symbols"]),
    ).toThrow(DumpSymbolsError);
    expect(() =>
      parseDumpSymbolsArguments(["--module-name", "crash-smoke", "--out", "symbols"]),
    ).toThrow("--binary");
  });

  it("reads the binary, its shipped name, the store, and the optional debug files", () => {
    expect(
      parseDumpSymbolsArguments([
        "--binary",
        "target/release/crash-smoke.exe",
        "--module-name",
        "crash-smoke.exe",
        "--out",
        "symbols",
        "--pdb",
        "target/release/crash_smoke.pdb",
        "--zip",
        "symbols.zip",
      ]),
    ).toEqual({
      binary: "target/release/crash-smoke.exe",
      moduleName: "crash-smoke.exe",
      out: "symbols",
      pdb: "target/release/crash_smoke.pdb",
      zip: "symbols.zip",
    });
  });

  it("fails a .sym that names functions but has no STACK CFI", () => {
    const withoutCfi = LINUX_SYM.split("\n")
      .filter((line) => !line.startsWith("STACK"))
      .join("\n");
    expect(() => assertUsableSymbols(withoutCfi, "crash-smoke")).toThrow(DumpSymbolsError);
    expect(() => assertUsableSymbols(withoutCfi, "crash-smoke")).toThrow("STACK CFI");
  });

  it("fails a .sym without source lines: a stripped binary, or a Windows binary without its PDB", () => {
    const stripped = LINUX_SYM.split("\n")
      .filter((line) => !line.startsWith("FUNC") && !line.startsWith("1000"))
      .join("\n");
    // Without its PDB, dump_syms names every .pdata function `<unknown in ...>` and has no lines.
    const withoutPdb = [
      "MODULE windows x86_64 3F0A1B2C4D5E6F708192A3B4C5D6E7F81 crash-smoke.exe",
      "INFO CODE_ID 66F0A1B21F000 crash-smoke.exe",
      "FUNC 1000 20 0 <unknown in crash-smoke.exe>",
      "FUNC 1020 40 0 <unknown in crash-smoke.exe>",
      "STACK CFI INIT 1000 20 .cfa: $rsp 8 + .ra: .cfa -8 + ^",
      "",
    ].join("\n");
    for (const sym of [stripped, withoutPdb]) {
      expect(() => assertUsableSymbols(sym, "crash-smoke")).toThrow("source lines");
      expect(() => assertUsableSymbols(Buffer.from(sym), "crash-smoke")).toThrow("source lines");
    }
    expect(() => assertUsableSymbols(LINUX_SYM, "crash-smoke")).not.toThrow();
    expect(() => assertUsableSymbols(Buffer.from(LINUX_SYM), "crash-smoke")).not.toThrow();
  });

  it("files a .sym where dump_syms --store does, keyed by the PDB name on Windows", () => {
    expect(symbolStorePath(LINUX_SYM)).toBe(
      "crash-smoke/A83437E178428CE331F11613797F1E660/crash-smoke.sym",
    );
    expect(symbolStorePath(Buffer.from(LINUX_SYM))).toBe(symbolStorePath(LINUX_SYM));
    expect(
      symbolStorePath("MODULE windows x86_64 3F0A1B2C4D5E6F708192A3B4C5D6E7F81 crash_smoke.pdb\n"),
    ).toBe("crash_smoke.pdb/3F0A1B2C4D5E6F708192A3B4C5D6E7F81/crash_smoke.sym");
    expect(() => symbolStorePath("FUNC 1000 20 0 f\n")).toThrow("MODULE");
  });

  it("writes a deflated ZIP whose single entry round-trips", () => {
    const root = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-dump-symbols-"));
    temporaryRoots.push(root);
    const zipPath = NodePath.join(root, "nested", "symbols.zip");
    const name = symbolStorePath(LINUX_SYM);
    writeZip(zipPath, [{ name, data: Buffer.from(LINUX_SYM) }]);

    const zip = NodeFS.readFileSync(zipPath);
    // Local file header: signature, deflate, CRC and sizes, then the name and the data.
    expect(zip.readUInt32LE(0)).toBe(0x04034b50);
    expect(zip.readUInt16LE(8)).toBe(8);
    const compressedSize = zip.readUInt32LE(18);
    const nameLength = zip.readUInt16LE(26);
    expect(zip.toString("utf8", 30, 30 + nameLength)).toBe(name);
    const dataStart = 30 + nameLength + zip.readUInt16LE(28);
    const data = NodeZlib.inflateRawSync(zip.subarray(dataStart, dataStart + compressedSize));
    expect(data.toString("utf8")).toBe(LINUX_SYM);
    expect(zip.readUInt32LE(14)).toBe(NodeZlib.crc32(data));
    // The end of central directory record lists one entry.
    const end = zip.length - 22;
    expect(zip.readUInt32LE(end)).toBe(0x06054b50);
    expect(zip.readUInt16LE(end + 10)).toBe(1);
    expect(zip.readUInt32LE(zip.readUInt32LE(end + 16))).toBe(0x02014b50);
  });
});
