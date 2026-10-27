// @effect-diagnostics nodeBuiltinImport:off
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import { afterEach, describe, expect, it } from "vite-plus/test";

import {
  RotatingFileSink,
  RotatingFileSinkCloseError,
  RotatingFileSinkConfigurationError,
  RotatingFileSinkError,
  RotatingFileSinkRollbackError,
  type RotatingFileSinkFileSystem,
} from "./logging.ts";

const tempDirectories: string[] = [];

const makeTempDirectory = (): string => {
  const directory = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-logging-"));
  tempDirectories.push(directory);
  return directory;
};

const captureError = (run: () => unknown): unknown => {
  try {
    run();
  } catch (cause) {
    return cause;
  }
  throw new Error("Expected operation to throw");
};

const injectedFileSystem = (
  overrides: Partial<RotatingFileSinkFileSystem>,
): RotatingFileSinkFileSystem =>
  ({
    mkdirSync: NodeFS.mkdirSync,
    statSync: NodeFS.statSync,
    openSync: NodeFS.openSync,
    fstatSync: NodeFS.fstatSync,
    writeSync: NodeFS.writeSync,
    ftruncateSync: NodeFS.ftruncateSync,
    closeSync: NodeFS.closeSync,
    existsSync: NodeFS.existsSync,
    rmSync: NodeFS.rmSync,
    renameSync: NodeFS.renameSync,
    readdirSync: NodeFS.readdirSync,
    ...overrides,
  }) as RotatingFileSinkFileSystem;

afterEach(() => {
  for (const directory of tempDirectories.splice(0)) {
    NodeFS.rmSync(directory, { recursive: true, force: true });
  }
});

describe("RotatingFileSink", () => {
  it.each([
    { option: "maxBytes" as const, maxBytes: 0, maxFiles: 1 },
    { option: "maxFiles" as const, maxBytes: 1, maxFiles: 0 },
  ])("reports invalid $option configuration structurally", (input) => {
    const thrown = captureError(
      () =>
        new RotatingFileSink({
          filePath: "/unused/log.ndjson",
          maxBytes: input.maxBytes,
          maxFiles: input.maxFiles,
        }),
    );

    expect(thrown).toBeInstanceOf(RotatingFileSinkConfigurationError);
    expect(thrown).toMatchObject({
      option: input.option,
      received: 0,
      minimum: 1,
    });
    expect((thrown as Error).message).toBe(`${input.option} must be >= 1 (received 0)`);
  });

  it("preserves directory initialization failures", () => {
    const directory = makeTempDirectory();
    const parentFile = NodePath.join(directory, "not-a-directory");
    const filePath = NodePath.join(parentFile, "log.ndjson");
    NodeFS.writeFileSync(parentFile, "occupied");

    const thrown = captureError(() => new RotatingFileSink({ filePath, maxBytes: 1, maxFiles: 1 }));

    expect(thrown).toBeInstanceOf(RotatingFileSinkError);
    expect(thrown).toMatchObject({ operation: "initialize", filePath });
    expect((thrown as RotatingFileSinkError).cause).toBeInstanceOf(Error);
  });

  it("only treats a missing log file as an empty current size", () => {
    const directory = makeTempDirectory();
    // 40000 chars exceeds both the POSIX NAME_MAX/PATH_MAX limits and the
    // Windows 32767-char extended path limit, so statSync raises
    // ENAMETOOLONG on every platform (a 300-char name maps to ENOENT on
    // Windows, which the sink intentionally treats as "missing file").
    const filePath = NodePath.join(directory, "a".repeat(40000));

    const thrown = captureError(() => new RotatingFileSink({ filePath, maxBytes: 1, maxFiles: 1 }));

    expect(thrown).toBeInstanceOf(RotatingFileSinkError);
    expect(thrown).toMatchObject({ operation: "read", filePath });
    expect((thrown as RotatingFileSinkError).cause).toMatchObject({ code: "ENAMETOOLONG" });
  });

  it("starts an absent log file at zero bytes", () => {
    const directory = makeTempDirectory();
    const filePath = NodePath.join(directory, "log.ndjson");
    const sink = new RotatingFileSink({ filePath, maxBytes: 100, maxFiles: 1 });

    sink.write("entry");

    expect(NodeFS.readFileSync(filePath, "utf8")).toBe("entry");
  });

  it("accepts buffers and ignores empty writes", () => {
    const directory = makeTempDirectory();
    const filePath = NodePath.join(directory, "log.ndjson");
    const sink = new RotatingFileSink({ filePath, maxBytes: 100, maxFiles: 1 });

    sink.write("");
    sink.write(Buffer.alloc(0));
    expect(NodeFS.existsSync(filePath)).toBe(false);

    sink.write(Buffer.from("entry"));
    expect(NodeFS.readFileSync(filePath, "utf8")).toBe("entry");
  });

  it("rolls a partially written batch back before a duplicate-free retry", () => {
    const directory = makeTempDirectory();
    const filePath = NodePath.join(directory, "log.ndjson");
    NodeFS.writeFileSync(filePath, "existing\n");
    let fail = true;
    let writes = 0;
    let truncates = 0;
    let closes = 0;
    const fileSystem = injectedFileSystem({
      writeSync: ((
        fd: number,
        buffer: Uint8Array,
        offset: number,
        length: number,
        position: number | null,
      ) => {
        writes += 1;
        if (fail && writes === 1) {
          return NodeFS.writeSync(fd, buffer, offset, Math.min(4, length), position ?? null);
        }
        if (fail) {
          throw new Error("injected partial write failure");
        }
        return NodeFS.writeSync(fd, buffer, offset, length, position);
      }) as unknown as typeof NodeFS.writeSync,
      ftruncateSync: ((fd, length) => {
        truncates += 1;
        NodeFS.ftruncateSync(fd, length);
      }) as typeof NodeFS.ftruncateSync,
      closeSync: ((fd) => {
        closes += 1;
        NodeFS.closeSync(fd);
      }) as typeof NodeFS.closeSync,
    });
    const sink = new RotatingFileSink({
      filePath,
      maxBytes: 1_024,
      maxFiles: 1,
      throwOnError: true,
      fileSystem,
    });

    const thrown = captureError(() => sink.write("first\nsecond\n"));
    expect(thrown).toMatchObject({ operation: "write", filePath });
    expect(NodeFS.readFileSync(filePath, "utf8")).toBe("existing\n");
    expect(truncates).toBe(1);
    expect(closes).toBe(1);

    fail = false;
    sink.write("first\nsecond\n");
    expect(NodeFS.readFileSync(filePath, "utf8")).toBe("existing\nfirst\nsecond\n");
    expect(closes).toBe(2);
  });

  it("treats zero-progress writes as failures and rolls them back", () => {
    const directory = makeTempDirectory();
    const filePath = NodePath.join(directory, "log.ndjson");
    NodeFS.writeFileSync(filePath, "valid\n");
    let truncates = 0;
    const sink = new RotatingFileSink({
      filePath,
      maxBytes: 1_024,
      maxFiles: 1,
      throwOnError: true,
      fileSystem: injectedFileSystem({
        writeSync: (() => 0) as typeof NodeFS.writeSync,
        ftruncateSync: ((fd, length) => {
          truncates += 1;
          NodeFS.ftruncateSync(fd, length);
        }) as typeof NodeFS.ftruncateSync,
      }),
    });

    const thrown = captureError(() => sink.write("next\n"));
    expect(thrown).toMatchObject({ operation: "write", filePath });
    expect(NodeFS.readFileSync(filePath, "utf8")).toBe("valid\n");
    expect(truncates).toBe(1);
  });

  it("makes close failure terminal while preserving an earlier rollback failure", () => {
    const directory = makeTempDirectory();
    const filePath = NodePath.join(directory, "log.ndjson");
    NodeFS.writeFileSync(filePath, "valid\n");
    let writes = 0;
    let closes = 0;
    const sink = new RotatingFileSink({
      filePath,
      maxBytes: 1_024,
      maxFiles: 1,
      throwOnError: true,
      fileSystem: injectedFileSystem({
        writeSync: ((
          fd: number,
          buffer: Uint8Array,
          offset: number,
          length: number,
          position: number | null,
        ) => {
          writes += 1;
          if (writes === 1) {
            return NodeFS.writeSync(fd, buffer, offset, Math.min(3, length), position ?? null);
          }
          throw new Error("injected write failure");
        }) as unknown as typeof NodeFS.writeSync,
        ftruncateSync: (() => {
          throw new Error("injected rollback failure");
        }) as typeof NodeFS.ftruncateSync,
        closeSync: ((fd) => {
          closes += 1;
          NodeFS.closeSync(fd);
          throw new Error("injected close failure after rollback failure");
        }) as typeof NodeFS.closeSync,
      }),
    });

    const thrown = captureError(() => sink.write("broken\n"));
    expect(thrown).toBeInstanceOf(RotatingFileSinkCloseError);
    expect(thrown).toMatchObject({ filePath });
    expect((thrown as RotatingFileSinkCloseError).cause).toMatchObject({
      message: "injected close failure after rollback failure",
    });
    expect((thrown as RotatingFileSinkCloseError).transactionCause).toBeInstanceOf(
      RotatingFileSinkRollbackError,
    );
    expect(
      ((thrown as RotatingFileSinkCloseError).transactionCause as RotatingFileSinkRollbackError)
        .message,
    ).toBe(`Failed to roll back partial rotating log write ${filePath}`);
    expect(
      ((thrown as RotatingFileSinkCloseError).transactionCause as RotatingFileSinkRollbackError)
        .writeCause,
    ).toBeInstanceOf(Error);
    expect(closes).toBe(1);
    expect(NodeFS.readFileSync(filePath, "utf8")).toBe("valid\nbro");
  });

  it("makes close failure terminal after a write failure and successful rollback", () => {
    const directory = makeTempDirectory();
    const filePath = NodePath.join(directory, "log.ndjson");
    NodeFS.writeFileSync(filePath, "valid\n");
    let writes = 0;
    const sink = new RotatingFileSink({
      filePath,
      maxBytes: 1_024,
      maxFiles: 1,
      throwOnError: true,
      fileSystem: injectedFileSystem({
        writeSync: ((
          fd: number,
          buffer: Uint8Array,
          offset: number,
          length: number,
          position: number | null,
        ) => {
          writes += 1;
          if (writes === 1) {
            return NodeFS.writeSync(fd, buffer, offset, 2, position ?? null);
          }
          throw new Error("injected write failure before close failure");
        }) as unknown as typeof NodeFS.writeSync,
        closeSync: ((fd) => {
          NodeFS.closeSync(fd);
          throw new Error("injected close failure after rollback");
        }) as typeof NodeFS.closeSync,
      }),
    });

    const thrown = captureError(() => sink.write("broken\n"));
    expect(thrown).toBeInstanceOf(RotatingFileSinkCloseError);
    expect((thrown as RotatingFileSinkCloseError).cause).toMatchObject({
      message: "injected close failure after rollback",
    });
    expect((thrown as RotatingFileSinkCloseError).transactionCause).toMatchObject({
      message: "injected write failure before close failure",
    });
    expect(NodeFS.readFileSync(filePath, "utf8")).toBe("valid\n");
    expect(writes).toBe(2);
  });

  it("does not hide a descriptor that may still be open when close throws before release", () => {
    const directory = makeTempDirectory();
    const filePath = NodePath.join(directory, "log.ndjson");
    let unreleasedDescriptor: number | undefined;
    let closeAttempts = 0;
    const sink = new RotatingFileSink({
      filePath,
      maxBytes: 1_024,
      maxFiles: 1,
      throwOnError: true,
      fileSystem: injectedFileSystem({
        closeSync: ((fd) => {
          closeAttempts += 1;
          unreleasedDescriptor = fd;
          throw new Error("injected close failure before release");
        }) as typeof NodeFS.closeSync,
      }),
    });

    try {
      const thrown = captureError(() => sink.write("complete\n"));
      expect(thrown).toBeInstanceOf(RotatingFileSinkCloseError);
      expect((thrown as RotatingFileSinkCloseError).transactionCause).toBeUndefined();
      expect(closeAttempts).toBe(1);
      expect(unreleasedDescriptor).toBeTypeOf("number");
    } finally {
      if (unreleasedDescriptor !== undefined) {
        NodeFS.closeSync(unreleasedDescriptor);
      }
    }
  });

  it("closes a completed write handle and reports a close failure", () => {
    const directory = makeTempDirectory();
    const filePath = NodePath.join(directory, "log.ndjson");
    const sink = new RotatingFileSink({
      filePath,
      maxBytes: 1_024,
      maxFiles: 1,
      throwOnError: true,
      fileSystem: injectedFileSystem({
        closeSync: ((fd) => {
          NodeFS.closeSync(fd);
          throw new Error("injected close failure");
        }) as typeof NodeFS.closeSync,
      }),
    });

    const thrown = captureError(() => sink.write("complete\n"));
    expect(thrown).toBeInstanceOf(RotatingFileSinkCloseError);
    expect((thrown as Error).message).toBe(`Failed to close rotating log file ${filePath}`);
    expect((thrown as RotatingFileSinkCloseError).cause).toMatchObject({
      message: "injected close failure",
    });
    expect(NodeFS.readFileSync(filePath, "utf8")).toBe("complete\n");
  });

  it("preserves non-missing open failures without attempting creation", () => {
    const directory = makeTempDirectory();
    const filePath = NodePath.join(directory, "log.ndjson");
    let openCalls = 0;
    const sink = new RotatingFileSink({
      filePath,
      maxBytes: 1_024,
      maxFiles: 1,
      throwOnError: true,
      fileSystem: injectedFileSystem({
        openSync: (() => {
          openCalls += 1;
          throw Object.assign(new Error("permission denied"), { code: "EACCES" });
        }) as typeof NodeFS.openSync,
      }),
    });

    const thrown = captureError(() => sink.write("entry"));
    expect(thrown).toMatchObject({ operation: "write", filePath });
    expect((thrown as RotatingFileSinkError).cause).toMatchObject({ code: "EACCES" });
    expect(openCalls).toBe(1);
  });

  it("rotates complete backups and prunes only numeric overflow files", () => {
    const directory = makeTempDirectory();
    const filePath = NodePath.join(directory, "log.ndjson");
    NodeFS.writeFileSync(`${filePath}.1`, "kept-one");
    NodeFS.writeFileSync(`${filePath}.2`, "kept-two");
    NodeFS.writeFileSync(`${filePath}.3`, "overflow");
    NodeFS.writeFileSync(`${filePath}.invalid`, "not-a-backup");
    NodeFS.writeFileSync(NodePath.join(directory, "other.3"), "unrelated");

    const sink = new RotatingFileSink({ filePath, maxBytes: 3, maxFiles: 2 });

    expect(NodeFS.existsSync(`${filePath}.3`)).toBe(false);
    expect(NodeFS.existsSync(`${filePath}.invalid`)).toBe(true);
    expect(NodeFS.existsSync(NodePath.join(directory, "other.3"))).toBe(true);

    sink.write("abc");
    sink.write("d");
    sink.write("efg");
    sink.write("h");

    expect(NodeFS.readFileSync(filePath, "utf8")).toBe("h");
    expect(NodeFS.readFileSync(`${filePath}.1`, "utf8")).toBe("efg");
    expect(NodeFS.readFileSync(`${filePath}.2`, "utf8")).toBe("d");
  });

  it("rotates an oversized first chunk after writing it", () => {
    const directory = makeTempDirectory();
    const filePath = NodePath.join(directory, "log.ndjson");
    const sink = new RotatingFileSink({ filePath, maxBytes: 3, maxFiles: 2 });

    sink.write("oversized");

    expect(NodeFS.existsSync(filePath)).toBe(false);
    expect(NodeFS.readFileSync(`${filePath}.1`, "utf8")).toBe("oversized");
  });

  it("recovers when the active file is removed between writes", () => {
    const directory = makeTempDirectory();
    const filePath = NodePath.join(directory, "log.ndjson");
    const sink = new RotatingFileSink({ filePath, maxBytes: 1, maxFiles: 1 });

    sink.write("a");
    NodeFS.rmSync(filePath);
    sink.write("b");

    expect(NodeFS.readFileSync(filePath, "utf8")).toBe("b");
    expect(NodeFS.existsSync(`${filePath}.1`)).toBe(false);
  });

  it("preserves write failures", () => {
    const directory = makeTempDirectory();
    const filePath = NodePath.join(directory, "log.ndjson");
    NodeFS.mkdirSync(filePath);
    const sink = new RotatingFileSink({
      filePath,
      maxBytes: Number.MAX_SAFE_INTEGER,
      maxFiles: 1,
      throwOnError: true,
    });

    const thrown = captureError(() => sink.write("entry"));

    expect(thrown).toBeInstanceOf(RotatingFileSinkError);
    expect(thrown).toMatchObject({ operation: "write", filePath });
    expect((thrown as Error).message).toBe(`Failed to write rotating log file ${filePath}`);
    expect((thrown as RotatingFileSinkError).cause).toMatchObject({ code: "EISDIR" });
  });

  it("recovers its size after a best-effort write failure", () => {
    const directory = makeTempDirectory();
    const filePath = NodePath.join(directory, "log.ndjson");
    NodeFS.mkdirSync(filePath);
    const sink = new RotatingFileSink({
      filePath,
      maxBytes: Number.MAX_SAFE_INTEGER,
      maxFiles: 1,
    });

    expect(() => sink.write("entry")).not.toThrow();
    expect(NodeFS.statSync(filePath).isDirectory()).toBe(true);
  });

  it("preserves rotation failures without an artificial write wrapper", () => {
    const directory = makeTempDirectory();
    const filePath = NodePath.join(directory, "log.ndjson");
    NodeFS.writeFileSync(filePath, "a");
    NodeFS.mkdirSync(`${filePath}.1`);
    const sink = new RotatingFileSink({
      filePath,
      maxBytes: 1,
      maxFiles: 1,
      throwOnError: true,
    });

    const thrown = captureError(() => sink.write("b"));

    expect(thrown).toBeInstanceOf(RotatingFileSinkError);
    expect(thrown).toMatchObject({ operation: "rotate", filePath });
    expect((thrown as RotatingFileSinkError).cause).toBeInstanceOf(Error);
  });

  it("continues after a best-effort rotation failure", () => {
    const directory = makeTempDirectory();
    const filePath = NodePath.join(directory, "log.ndjson");
    NodeFS.writeFileSync(filePath, "a");
    NodeFS.mkdirSync(`${filePath}.1`);
    NodeFS.writeFileSync(NodePath.join(`${filePath}.1`, "entry"), "occupied");
    const sink = new RotatingFileSink({ filePath, maxBytes: 1, maxFiles: 1 });

    expect(() => sink.write("b")).not.toThrow();
    expect(NodeFS.readFileSync(filePath, "utf8")).toBe("ab");
  });

  it("preserves backup pruning failures", () => {
    const directory = makeTempDirectory();
    const filePath = NodePath.join(directory, "log.ndjson");
    const overflowBackup = `${filePath}.2`;
    NodeFS.mkdirSync(overflowBackup);
    NodeFS.writeFileSync(NodePath.join(overflowBackup, "entry"), "occupied");

    const thrown = captureError(
      () =>
        new RotatingFileSink({
          filePath,
          maxBytes: 1,
          maxFiles: 1,
          throwOnError: true,
        }),
    );

    expect(thrown).toBeInstanceOf(RotatingFileSinkError);
    expect(thrown).toMatchObject({ operation: "prune", filePath });
    expect((thrown as RotatingFileSinkError).cause).toBeInstanceOf(Error);
  });

  it("continues after a best-effort backup pruning failure", () => {
    const directory = makeTempDirectory();
    const filePath = NodePath.join(directory, "log.ndjson");
    const overflowBackup = `${filePath}.2`;
    NodeFS.mkdirSync(overflowBackup);
    NodeFS.writeFileSync(NodePath.join(overflowBackup, "entry"), "occupied");

    expect(() => new RotatingFileSink({ filePath, maxBytes: 1, maxFiles: 1 })).not.toThrow();
    expect(NodeFS.existsSync(overflowBackup)).toBe(true);
  });
});
