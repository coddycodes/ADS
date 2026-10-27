#!/usr/bin/env node
// @effect-diagnostics nodeBuiltinImport:off - This standalone CLI owns the dump_syms process and the symbol files.
// @effect-diagnostics globalConsole:off - The CLI reports the symbol file it stored.
//
// dump-symbols.ts --binary <path> --module-name <shipped-name> --out <store>
//                 [--pdb <path>] [--dsym <path>] [--zip <path>]
//
// Writes the Breakpad `.sym` of one release binary into a symbol store, laid out as
// `<module>/<debug id>/<module>.sym` (`<name>.pdb/<debug id>/<name>.sym` on Windows), which is
// what minidump-stackwalk and the profiler's symbolication read. `--zip` also writes a ZIP that
// holds just that file, at the same path. Run it before stripping: names and lines come from the
// binary's DWARF on Linux, from `--dsym` on macOS and from `--pdb` on Windows, and the unwind
// tables (`STACK` records) from the binary itself. Uses only Node built-ins, so CI can run it
// without installing workspace dependencies. Needs `dump_syms` on PATH, with its default
// features: it only finds the PDB that a Windows binary names when built with `http`.
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";
import * as NodeUtil from "node:util";
import * as NodeZlib from "node:zlib";

export class DumpSymbolsError extends Error {
  override readonly name = "DumpSymbolsError";
}

export interface DumpSymbolsInput {
  readonly binary: string;
  /** The file name the binary ships under; dump_syms names the module after its input. */
  readonly moduleName: string;
  readonly out: string;
  readonly pdb?: string;
  readonly dsym?: string;
  readonly zip?: string;
}

export function parseDumpSymbolsArguments(argv: ReadonlyArray<string>): DumpSymbolsInput {
  const { values } = NodeUtil.parseArgs({
    args: [...argv],
    allowPositionals: false,
    strict: true,
    options: {
      binary: { type: "string" },
      "module-name": { type: "string" },
      out: { type: "string" },
      pdb: { type: "string" },
      dsym: { type: "string" },
      zip: { type: "string" },
    },
  });
  const moduleName = values["module-name"];
  if (values.binary === undefined || moduleName === undefined || values.out === undefined) {
    throw new DumpSymbolsError("--binary, --module-name and --out are required.");
  }
  return {
    binary: values.binary,
    moduleName,
    out: values.out,
    ...(values.pdb === undefined ? {} : { pdb: values.pdb }),
    ...(values.dsym === undefined ? {} : { dsym: values.dsym }),
    ...(values.zip === undefined ? {} : { zip: values.zip }),
  };
}

// A `.sym` can outgrow the longest string V8 allows (~512 MiB), so it is searched as bytes.
function hasRecord(sym: string | Buffer, kind: string): boolean {
  return sym.indexOf(`${kind} `) === 0 || sym.indexOf(`\n${kind} `) !== -1;
}

// Line records, which follow the FUNC they belong to, are the only lines that start with a
// (lower-case hex) address.
function hasLineRecords(sym: string | Buffer): boolean {
  return [..."0123456789abcdef"].some((digit) => sym.indexOf(`\n${digit}`) !== -1);
}

/**
 * Fails unless the `.sym` can symbolicate a minidump: `STACK` records to unwind past the
 * crashing frame, and functions with source lines to name the frames. dump_syms reports a
 * missing debug file only through what it leaves out: a stripped binary or a missing dSYM gives
 * no `FUNC` records, and a Windows binary without its PDB gives `<unknown in ...>` placeholders,
 * neither with lines.
 */
export function assertUsableSymbols(sym: string | Buffer, source: string): void {
  if (!hasRecord(sym, "STACK")) {
    throw new DumpSymbolsError(
      `dump_syms produced no STACK CFI records for ${source}; dump the binary itself, not a debug-only file.`,
    );
  }
  if (!hasLineRecords(sym)) {
    throw new DumpSymbolsError(
      `dump_syms produced no functions with source lines for ${source}; dump the unstripped binary, with --dsym on macOS or --pdb on Windows.`,
    );
  }
}

/** Where `dump_syms --store` files this `.sym`, from its `MODULE <os> <arch> <id> <name>` record. */
export function symbolStorePath(sym: string | Buffer): string {
  // dump_syms writes the MODULE record first.
  const head = typeof sym === "string" ? sym : sym.subarray(0, 4096).toString("utf8");
  const module = /^MODULE \S+ \S+ (\S+) ([^\r\n]+)$/m.exec(head);
  if (module === null) throw new DumpSymbolsError("The .sym has no MODULE record.");
  const [, id, name] = module as unknown as [string, string, string];
  const leaf = name.toLowerCase().endsWith(".pdb") ? `${name.slice(0, -4)}.sym` : `${name}.sym`;
  return `${name}/${id}/${leaf}`;
}

/** Writes a deflated ZIP with a fixed timestamp, so the same symbols make the same archive. */
export function writeZip(
  zipPath: string,
  entries: ReadonlyArray<{ readonly name: string; readonly data: Buffer }>,
): void {
  const local: Buffer[] = [];
  const central: Buffer[] = [];
  let offset = 0;
  for (const { name, data } of entries) {
    const fileName = Buffer.from(name, "utf8");
    const compressed = NodeZlib.deflateRawSync(data);
    const crc = NodeZlib.crc32(data);
    // ponytail: ZIP32 only; a store past 4 GiB needs ZIP64 records.
    if (data.length > 0xffffffff || offset + compressed.length > 0xffffffff) {
      throw new DumpSymbolsError(`${name} is too large for a ZIP32 archive.`);
    }
    const header = Buffer.alloc(30);
    header.writeUInt32LE(0x04034b50, 0);
    header.writeUInt16LE(20, 4); // version needed: 2.0, deflate
    header.writeUInt16LE(0x0800, 6); // UTF-8 names
    header.writeUInt16LE(8, 8); // deflate
    header.writeUInt16LE(0, 10); // 00:00
    header.writeUInt16LE(0x21, 12); // 1980-01-01
    header.writeUInt32LE(crc, 14);
    header.writeUInt32LE(compressed.length, 18);
    header.writeUInt32LE(data.length, 22);
    header.writeUInt16LE(fileName.length, 26);
    const record = Buffer.alloc(46);
    record.writeUInt32LE(0x02014b50, 0);
    record.writeUInt16LE(20, 4); // made by: 2.0
    record.writeUInt16LE(20, 6);
    record.writeUInt16LE(0x0800, 8);
    record.writeUInt16LE(8, 10);
    record.writeUInt16LE(0x21, 14);
    record.writeUInt32LE(crc, 16);
    record.writeUInt32LE(compressed.length, 20);
    record.writeUInt32LE(data.length, 24);
    record.writeUInt16LE(fileName.length, 28);
    record.writeUInt32LE(offset, 42);
    local.push(header, fileName, compressed);
    central.push(record, fileName);
    offset += header.length + fileName.length + compressed.length;
  }
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(
    central.reduce((size, chunk) => size + chunk.length, 0),
    12,
  );
  end.writeUInt32LE(offset, 16);
  NodeFS.mkdirSync(NodePath.dirname(zipPath), { recursive: true });
  NodeFS.writeFileSync(zipPath, Buffer.concat([...local, ...central, end]));
}

function stage(source: string, directory: string, name: string): string {
  NodeFS.mkdirSync(directory, { recursive: true });
  const target = NodePath.join(directory, name);
  NodeFS.copyFileSync(source, target, NodeFS.constants.COPYFILE_FICLONE);
  return target;
}

/** The DWARF file inside a dSYM bundle, named after the hashed `deps/` binary, not the shipped one. */
function dwarfFile(dsym: string): string {
  const directory = NodePath.join(dsym, "Contents", "Resources", "DWARF");
  const files = NodeFS.readdirSync(directory);
  if (files.length !== 1) {
    throw new DumpSymbolsError(`Expected one DWARF file in ${directory}, found ${files.length}.`);
  }
  return NodePath.join(directory, files[0]!);
}

/** Runs dump_syms on the staged inputs and returns the stored `.sym` path. */
export function dumpSymbols(input: DumpSymbolsInput, dumpSyms = "dump_syms"): string {
  const work = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-dump-symbols-"));
  try {
    // dump_syms names a module after the file it reads, so every input takes the shipped name.
    const inputs = [stage(input.binary, NodePath.join(work, "binary"), input.moduleName)];
    if (input.pdb !== undefined) {
      // dump_syms looks for the PDB that a Windows binary names beside the binary.
      stage(input.pdb, NodePath.join(work, "binary"), NodePath.basename(input.pdb));
    }
    if (input.dsym !== undefined) {
      // The dSYM has the names and lines, the binary the unwind tables; dump_syms merges inputs
      // that share a debug ID.
      inputs.push(stage(dwarfFile(input.dsym), NodePath.join(work, "dsym"), input.moduleName));
    }
    const output = NodePath.join(work, "module.sym");
    const result = NodeChildProcess.spawnSync(
      dumpSyms,
      ["--inlines", "--check-cfi", "--store", input.out, "--output", output, ...inputs],
      { shell: false, stdio: ["ignore", "inherit", "inherit"] },
    );
    if (result.error) throw result.error;
    if (result.status !== 0) {
      throw new DumpSymbolsError(`dump_syms exited with ${result.status ?? result.signal}.`);
    }
    // Given several inputs, dump_syms logs a failed one and still exits 0.
    if (!NodeFS.existsSync(output)) {
      throw new DumpSymbolsError(`dump_syms wrote no symbols for ${input.binary}.`);
    }
    const bytes = NodeFS.readFileSync(output);
    assertUsableSymbols(bytes, input.binary);
    const storePath = symbolStorePath(bytes);
    if (input.zip !== undefined) writeZip(input.zip, [{ name: storePath, data: bytes }]);
    return NodePath.join(input.out, ...storePath.split("/"));
  } finally {
    NodeFS.rmSync(work, { recursive: true, force: true });
  }
}

if (
  process.argv[1] !== undefined &&
  import.meta.url === NodeURL.pathToFileURL(process.argv[1]).href
) {
  try {
    const input = parseDumpSymbolsArguments(process.argv.slice(2));
    const stored = dumpSymbols(input);
    console.log(`Stored ${stored}${input.zip === undefined ? "" : ` and ${input.zip}`}`);
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  }
}
