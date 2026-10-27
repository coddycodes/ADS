import * as Schema from "effect/Schema";
import { describe, expect, it } from "vite-plus/test";

import {
  DEFAULT_TERMINAL_ID,
  TERMINAL_LAUNCH_ARGUMENT_MAX_COUNT,
  TERMINAL_LAUNCH_ARGUMENT_MAX_LENGTH,
  TERMINAL_LAUNCH_EXECUTABLE_MAX_LENGTH,
  TERMINAL_LAUNCH_LABEL_MAX_LENGTH,
  ProviderTerminalActivityLaunch,
  TerminalAttachInput,
  TerminalAttachStreamEvent,
  TerminalClearInput,
  TerminalCloseInput,
  TerminalCwdNotDirectoryError,
  TerminalCwdNotFoundError,
  TerminalCwdStatError,
  TerminalError,
  TerminalEvent,
  TerminalHistoryError,
  TerminalLaunchCommand,
  TerminalNotRunningError,
  TerminalOpenInput,
  TerminalResizeError,
  TerminalResizeInput,
  TerminalRestartInput,
  TerminalSessionSnapshot,
  TerminalSessionLookupError,
  TerminalSpawnError,
  TerminalThreadInput,
  TerminalWriteError,
  TerminalWriteInput,
  TerminalBeginInput,
  TerminalCancelInput,
  TerminalInputAcknowledgement,
  TerminalInputLease,
  TerminalWriteInputFrame,
} from "./terminal.ts";
import {
  expectDecodeFailure,
  expectEncodeFailure,
  makeInvalidClassInstance,
} from "./test/schemaAssertions.ts";

// The Rust server rejects unknown fields; Effect 4 parse options are per operation.
const decodeStrictProviderTerminalActivityLaunch = Schema.decodeUnknownSync(
  ProviderTerminalActivityLaunch,
  { onExcessProperty: "error" },
);

function decodeSync<S extends Schema.Top>(schema: S, input: unknown): Schema.Schema.Type<S> {
  return Schema.decodeUnknownSync(schema as never)(input) as Schema.Schema.Type<S>;
}

function decodes<S extends Schema.Top>(schema: S, input: unknown): boolean {
  try {
    Schema.decodeUnknownSync(schema as never)(input);
    return true;
  } catch {
    return false;
  }
}

const encodeTerminalRestartInput = Schema.encodeSync(TerminalRestartInput);
const encodeTerminalSnapshot = Schema.encodeSync(TerminalSessionSnapshot);
const encodeTerminalResize = Schema.encodeSync(TerminalResizeInput);
const encodeTerminalAttachEvent = Schema.encodeSync(TerminalAttachStreamEvent);
const decodeTerminalError = Schema.decodeUnknownSync(TerminalError);
const encodeTerminalError = Schema.encodeUnknownSync(TerminalError);

describe("ordered terminal input", () => {
  it("requires a nonnegative safe attachment sequence on every begin", () => {
    for (const attachmentSequence of [undefined, -1, 0.5, Number.MAX_SAFE_INTEGER + 1]) {
      expect(
        decodes(TerminalBeginInput, { threadId: "thread", terminalId: "term", attachmentSequence }),
      ).toBe(false);
    }
    expect(
      decodes(TerminalBeginInput, {
        threadId: "thread",
        terminalId: "term",
        attachmentSequence: Number.MAX_SAFE_INTEGER,
      }),
    ).toBe(true);
  });
  it("decodes lease lifecycle payloads and acknowledgements", () => {
    expect(
      decodeSync(TerminalBeginInput, {
        threadId: "thread",
        terminalId: "term-1",
        attachmentSequence: 0,
      }),
    ).toEqual({
      threadId: "thread",
      terminalId: "term-1",
      attachmentSequence: 0,
    });
    expect(decodeSync(TerminalInputLease, { inputId: "lease" })).toEqual({ inputId: "lease" });
    expect(
      decodeSync(TerminalWriteInputFrame, {
        threadId: "thread",
        terminalId: "term-1",
        inputId: "lease",
        sequence: 0,
        data: "x",
      }),
    ).toMatchObject({ inputId: "lease", sequence: 0, data: "x" });
    expect(decodeSync(TerminalInputAcknowledgement, { inputId: "lease", sequence: 0 })).toEqual({
      inputId: "lease",
      sequence: 0,
    });
    expect(
      decodeSync(TerminalCancelInput, {
        threadId: "thread",
        terminalId: "term-1",
        inputId: "lease",
      }),
    ).toMatchObject({ inputId: "lease" });
  });

  it("rejects invalid frame sequences and data", () => {
    const frame = {
      threadId: "thread",
      terminalId: "term-1",
      inputId: "lease",
      sequence: 0,
      data: "x",
    };
    for (const sequence of [-1, 0.5, Number.MAX_SAFE_INTEGER + 1]) {
      expect(() => decodeSync(TerminalWriteInputFrame, { ...frame, sequence })).toThrow();
    }
    for (const data of ["", "x".repeat(16_385)]) {
      expect(() => decodeSync(TerminalWriteInputFrame, { ...frame, data })).toThrow();
    }
  });
});

describe("TerminalOpenInput", () => {
  it("accepts valid open input", () => {
    expect(
      decodes(TerminalOpenInput, {
        threadId: "thread-1",
        terminalId: DEFAULT_TERMINAL_ID,
        cwd: "/tmp/project",
        cols: 120,
        rows: 40,
      }),
    ).toBe(true);
  });

  it("accepts ultrawide terminal dimensions from xterm fit", () => {
    expect(
      decodes(TerminalOpenInput, {
        threadId: "thread-1",
        terminalId: DEFAULT_TERMINAL_ID,
        cwd: "/tmp/project",
        cols: 423,
        rows: 40,
      }),
    ).toBe(true);
  });

  it("reports invalid row bounds at the rows path", () => {
    expectDecodeFailure(
      TerminalOpenInput,
      {
        threadId: "thread-1",
        terminalId: DEFAULT_TERMINAL_ID,
        cwd: "/tmp/project",
        cols: 10,
        rows: 0,
      },
      { rootTag: "Composite", paths: [["rows"]], containsTag: "InvalidValue" },
    );
  });

  it("reports a missing client-selected terminalId", () => {
    expectDecodeFailure(
      TerminalOpenInput,
      {
        threadId: "thread-1",
        cwd: "/tmp/project",
        cols: 100,
        rows: 24,
      },
      { rootTag: "Composite", paths: [["terminalId"]], containsTag: "MissingKey" },
    );
  });

  it("accepts optional env overrides", () => {
    const parsed = decodeSync(TerminalOpenInput, {
      threadId: "thread-1",
      terminalId: DEFAULT_TERMINAL_ID,
      cwd: "/tmp/project",
      worktreePath: "/tmp/project/.coddy/studio/worktrees/feature-a",
      cols: 100,
      rows: 24,
      env: {
        CODDY_STUDIO_PROJECT_ROOT: "/tmp/project",
        CUSTOM_FLAG: "1",
      },
    });
    expect(parsed.env).toMatchObject({
      CODDY_STUDIO_PROJECT_ROOT: "/tmp/project",
      CUSTOM_FLAG: "1",
    });
    expect(parsed.worktreePath).toBe("/tmp/project/.coddy/studio/worktrees/feature-a");
  });

  it("reports invalid environment keys at the complete nested path", () => {
    expectDecodeFailure(
      TerminalOpenInput,
      {
        threadId: "thread-1",
        terminalId: DEFAULT_TERMINAL_ID,
        cwd: "/tmp/project",
        cols: 100,
        rows: 24,
        env: {
          "bad-key": "1",
        },
      },
      {
        rootTag: "Composite",
        paths: [["env", "bad-key"]],
        containsTag: "InvalidValue",
      },
    );
  });
});

describe("TerminalAttachInput", () => {
  it("preserves an optional attachment claim and rejects an empty claim", () => {
    const input = { threadId: "thread", terminalId: "term", sizeClaim: "renderer" };
    expect(decodeSync(TerminalAttachInput, input)).toEqual(input);
    expect(() => decodeSync(TerminalAttachInput, { ...input, sizeClaim: " " })).toThrow();
    expect(() =>
      decodeSync(TerminalAttachInput, { ...input, sizeClaim: "x".repeat(129) }),
    ).toThrow();
  });

  it("accepts explicit inactive-session restart intent", () => {
    const parsed = decodeSync(TerminalAttachInput, {
      threadId: "thread-1",
      terminalId: DEFAULT_TERMINAL_ID,
      cwd: "/tmp/project",
      restartIfNotRunning: true,
    });

    expect(parsed.restartIfNotRunning).toBe(true);
  });
});

describe("TerminalLaunchCommand", () => {
  const command = {
    executable: "/Applications/Cursor Agent/cursor-agent",
    args: ["--yolo"],
    label: "Cursor Terminal",
  };

  it("round-trips on open, attach, and restart inputs", () => {
    for (const [schema, input] of [
      [
        TerminalOpenInput,
        {
          threadId: "thread-1",
          terminalId: "term-1",
          cwd: "/tmp/project",
          command,
        },
      ],
      [
        TerminalAttachInput,
        {
          threadId: "thread-1",
          terminalId: "term-1",
          cwd: "/tmp/project",
          command,
        },
      ],
      [
        TerminalRestartInput,
        {
          threadId: "thread-1",
          terminalId: "term-1",
          cwd: "/tmp/project",
          cols: 120,
          rows: 30,
          command,
        },
      ],
    ] as const) {
      expect(decodeSync(schema, input).command).toEqual(command);
    }
  });

  it("keeps legacy shell payloads valid", () => {
    expect(
      decodeSync(TerminalOpenInput, {
        threadId: "thread-1",
        terminalId: "term-1",
        cwd: "/tmp/project",
      }).command,
    ).toBeUndefined();
  });

  it("round-trips a bounded Codex provider activity launch hint", () => {
    const decoded = decodeSync(TerminalLaunchCommand, {
      executable: "codex",
      args: ["--model", "gpt-5"],
      env: { SHARED: "command", CODEX_HOME: "/tmp/codex" },
      activity: {
        driverKind: "codex",
        providerInstanceId: "codex_personal",
      },
    });

    expect(decoded).toEqual({
      executable: "codex",
      args: ["--model", "gpt-5"],
      env: { SHARED: "command", CODEX_HOME: "/tmp/codex" },
      activity: {
        driverKind: "codex",
        providerInstanceId: "codex_personal",
      },
    });
  });

  it("keeps the provider activity launch hint optional", () => {
    expect(
      decodeSync(TerminalLaunchCommand, {
        executable: "codex",
        args: [],
      }).activity,
    ).toBeUndefined();
  });

  it("rejects malformed provider activity driver and instance identifiers", () => {
    for (const activity of [
      { driverKind: "unknown.driver", providerInstanceId: "codex" },
      { driverKind: "codex", providerInstanceId: "" },
      { driverKind: "codex", providerInstanceId: "x".repeat(65) },
      { driverKind: "codex" },
      null,
      "codex",
    ]) {
      expect(() =>
        decodeSync(TerminalLaunchCommand, {
          executable: "codex",
          args: [],
          activity,
        }),
      ).toThrow();
    }
  });

  it("rejects secret, token, and topology fields on provider activity hints", () => {
    expect(
      decodeSync(ProviderTerminalActivityLaunch, {
        driverKind: "codex",
        providerInstanceId: "codex",
      }),
    ).toEqual({
      driverKind: "codex",
      providerInstanceId: "codex",
    });

    for (const extra of [
      { token: "secret" },
      { secret: "secret" },
      { strategy: "remote" },
      { url: "http://127.0.0.1:1234" },
      { port: 1234 },
      { socket: "/tmp/codex.sock" },
      { nativeSessionId: "native-1" },
    ]) {
      expect(() =>
        decodeStrictProviderTerminalActivityLaunch({
          driverKind: "codex",
          providerInstanceId: "codex",
          ...extra,
        }),
      ).toThrow();
    }
  });

  it("accepts an optional launch environment and rejects invalid env keys", () => {
    const parsed = decodeSync(TerminalLaunchCommand, {
      executable: "opencode",
      args: [],
      env: { OPENCODE_CONFIG_CONTENT: '{"theme":"system"}' },
    });
    expect(parsed.env).toEqual({ OPENCODE_CONFIG_CONTENT: '{"theme":"system"}' });
    expect(decodeSync(TerminalLaunchCommand, { executable: "opencode", args: [] }).env).toBe(
      undefined,
    );
    expect(() =>
      decodeSync(TerminalLaunchCommand, {
        executable: "opencode",
        args: [],
        env: { "invalid key!": "value" },
      }),
    ).toThrow();
  });

  it("rejects invalid executable, argument, count, and label bounds", () => {
    expect(() => decodeSync(TerminalLaunchCommand, { executable: " ", args: [] })).toThrow();
    expect(() =>
      decodeSync(TerminalLaunchCommand, {
        executable: "x".repeat(TERMINAL_LAUNCH_EXECUTABLE_MAX_LENGTH + 1),
        args: [],
      }),
    ).toThrow();
    expect(() =>
      decodeSync(TerminalLaunchCommand, {
        executable: "codex",
        args: ["x".repeat(TERMINAL_LAUNCH_ARGUMENT_MAX_LENGTH + 1)],
      }),
    ).toThrow();
    expect(() =>
      decodeSync(TerminalLaunchCommand, {
        executable: "codex",
        args: Array.from({ length: TERMINAL_LAUNCH_ARGUMENT_MAX_COUNT + 1 }, () => "x"),
      }),
    ).toThrow();
    expect(() =>
      decodeSync(TerminalLaunchCommand, {
        executable: "codex",
        args: [],
        label: "x".repeat(TERMINAL_LAUNCH_LABEL_MAX_LENGTH + 1),
      }),
    ).toThrow();
  });

  it("measures command string bounds in UTF-16 code units", () => {
    const astral = "😀";
    const atBoundary = decodeSync(TerminalLaunchCommand, {
      executable: astral.repeat(TERMINAL_LAUNCH_EXECUTABLE_MAX_LENGTH / 2),
      args: [astral.repeat(TERMINAL_LAUNCH_ARGUMENT_MAX_LENGTH / 2)],
      label: astral.repeat(TERMINAL_LAUNCH_LABEL_MAX_LENGTH / 2),
    });

    expect(atBoundary.executable.length).toBe(TERMINAL_LAUNCH_EXECUTABLE_MAX_LENGTH);
    expect(atBoundary.args[0]?.length).toBe(TERMINAL_LAUNCH_ARGUMENT_MAX_LENGTH);
    expect(atBoundary.label?.length).toBe(TERMINAL_LAUNCH_LABEL_MAX_LENGTH);
    for (const value of [
      {
        executable: astral.repeat(TERMINAL_LAUNCH_EXECUTABLE_MAX_LENGTH / 2 + 1),
        args: [],
      },
      {
        executable: "codex",
        args: [astral.repeat(TERMINAL_LAUNCH_ARGUMENT_MAX_LENGTH / 2 + 1)],
      },
      {
        executable: "codex",
        args: [],
        label: astral.repeat(TERMINAL_LAUNCH_LABEL_MAX_LENGTH / 2 + 1),
      },
    ]) {
      expect(() => decodeSync(TerminalLaunchCommand, value)).toThrow();
    }
  });

  it("uses ECMAScript trim semantics without trimming arguments", () => {
    const decoded = decodeSync(TerminalLaunchCommand, {
      executable: "\uFEFFcodex\uFEFF",
      args: ["\uFEFF--model\uFEFF"],
      label: "\uFEFFCodex Terminal\uFEFF",
    });

    expect(decoded).toEqual({
      executable: "codex",
      args: ["\uFEFF--model\uFEFF"],
      label: "Codex Terminal",
    });
    expect(
      decodeSync(TerminalLaunchCommand, {
        executable: "\u0085codex\u0085",
        args: [],
        label: "\u0085Codex Terminal\u0085",
      }),
    ).toEqual({
      executable: "\u0085codex\u0085",
      args: [],
      label: "\u0085Codex Terminal\u0085",
    });
    expect(() =>
      decodeSync(TerminalLaunchCommand, {
        executable: "\uFEFF",
        args: [],
      }),
    ).toThrow();
  });
});

describe("TerminalWriteInput", () => {
  it("accepts non-empty data", () => {
    expect(
      decodes(TerminalWriteInput, {
        threadId: "thread-1",
        terminalId: DEFAULT_TERMINAL_ID,
        data: "echo hello\n",
      }),
    ).toBe(true);
  });

  it("reports empty data at the data path", () => {
    expectDecodeFailure(
      TerminalWriteInput,
      {
        threadId: "thread-1",
        terminalId: DEFAULT_TERMINAL_ID,
        data: "",
      },
      { rootTag: "Composite", paths: [["data"]], containsTag: "InvalidValue" },
    );
  });

  it("reports a missing terminalId at its key path", () => {
    expectDecodeFailure(
      TerminalWriteInput,
      { threadId: "thread-1", data: "echo hello\n" },
      { rootTag: "Composite", paths: [["terminalId"]], containsTag: "MissingKey" },
    );
  });
});

describe("TerminalThreadInput", () => {
  it("trims thread ids", () => {
    const parsed = decodeSync(TerminalThreadInput, { threadId: " thread-1 " });
    expect(parsed.threadId).toBe("thread-1");
  });
});

describe("TerminalResizeInput", () => {
  it("accepts valid size", () => {
    expect(
      decodes(TerminalResizeInput, {
        threadId: "thread-1",
        terminalId: DEFAULT_TERMINAL_ID,
        cols: 80,
        rows: 24,
      }),
    ).toBe(true);
  });

  it("reports a missing terminalId at its key path", () => {
    expectDecodeFailure(
      TerminalResizeInput,
      { threadId: "thread-1", cols: 80, rows: 24 },
      { rootTag: "Composite", paths: [["terminalId"]], containsTag: "MissingKey" },
    );
  });
});

describe("TerminalClearInput", () => {
  it("reports a missing terminalId at its key path", () => {
    expectDecodeFailure(
      TerminalClearInput,
      { threadId: "thread-1" },
      { rootTag: "Composite", paths: [["terminalId"]], containsTag: "MissingKey" },
    );
  });

  it("accepts an explicit terminalId", () => {
    const parsed = decodeSync(TerminalClearInput, {
      threadId: "thread-1",
      terminalId: DEFAULT_TERMINAL_ID,
    });
    expect(parsed.terminalId).toBe(DEFAULT_TERMINAL_ID);
  });
});

describe("TerminalCloseInput", () => {
  it("accepts optional deleteHistory", () => {
    expect(
      decodes(TerminalCloseInput, {
        threadId: "thread-1",
        deleteHistory: true,
      }),
    ).toBe(true);
  });
});

describe("TerminalSessionSnapshot", () => {
  const isoTimestamp = "2026-01-01T00:00:00.000Z";

  it("round-trips applied size and claim while accepting older snapshots", () => {
    const legacy = {
      threadId: "thread-1",
      terminalId: "term-1",
      cwd: "/repo",
      worktreePath: null,
      status: "running",
      pid: 1,
      history: "",
      exitCode: null,
      exitSignal: null,
      label: "Terminal 1",
      updatedAt: isoTimestamp,
    };
    expect(decodeSync(TerminalSessionSnapshot, legacy)).toEqual({
      ...legacy,
      oscColorResponderActive: false,
      firstAttachmentGrant: false,
    });
    for (const size of [{ cols: 151 }, { rows: 50 }, { cols: 151, rows: 50 }]) {
      expect(() => decodeSync(TerminalSessionSnapshot, { ...legacy, size })).toThrow();
    }
    for (const sizeClaim of [null, "renderer-a"]) {
      const current = {
        ...legacy,
        size: { cols: 151, rows: 50, sizeClaim },
        oscColorResponderActive: true,
        firstAttachmentGrant: true,
      };
      expect(encodeTerminalSnapshot(decodeSync(TerminalSessionSnapshot, current))).toEqual(current);
    }
  });

  it("accepts running snapshots", () => {
    expect(
      decodes(TerminalSessionSnapshot, {
        threadId: "thread-1",
        terminalId: DEFAULT_TERMINAL_ID,
        cwd: "/tmp/project",
        worktreePath: null,
        status: "running",
        pid: 1234,
        history: "hello\n",
        exitCode: null,
        exitSignal: null,
        label: "Primary",
        updatedAt: isoTimestamp,
      }),
    ).toBe(true);
  });

  it("accepts the optional Windows console launch theme", () => {
    expect(
      decodes(TerminalSessionSnapshot, {
        threadId: "thread-1",
        terminalId: DEFAULT_TERMINAL_ID,
        cwd: "/tmp/project",
        worktreePath: null,
        status: "running",
        pid: 1234,
        history: "hello\n",
        exitCode: null,
        exitSignal: null,
        consoleTheme: "light",
        label: "Primary",
        updatedAt: isoTimestamp,
      }),
    ).toBe(true);
  });
});

describe("TerminalEvent", () => {
  const isoTimestamp = "2026-01-01T00:00:00.000Z";

  it("round-trips resized events and optional bounded resize claims", () => {
    const input = {
      threadId: "thread-1",
      terminalId: "term-1",
      cols: 91,
      rows: 42,
      sizeClaim: "renderer-b",
    };
    expect(encodeTerminalResize(decodeSync(TerminalResizeInput, input))).toEqual(input);
    for (const sizeClaim of [null, "renderer-b"]) {
      const event = {
        threadId: input.threadId,
        terminalId: input.terminalId,
        type: "resized",
        sequence: 12,
        size: { cols: input.cols, rows: input.rows, sizeClaim },
      };
      expect(() => decodeSync(TerminalEvent, event)).toThrow();
      expect(encodeTerminalAttachEvent(decodeSync(TerminalAttachStreamEvent, event))).toEqual(
        event,
      );
    }
    for (const sizeClaim of ["", " ", "x".repeat(129), null]) {
      expect(() => decodeSync(TerminalResizeInput, { ...input, sizeClaim })).toThrow();
    }
    expect(() =>
      decodeSync(TerminalAttachStreamEvent, { ...input, type: "resized", cols: 0 }),
    ).toThrow();
  });

  it("accepts output events", () => {
    expect(
      decodes(TerminalEvent, {
        type: "output",
        threadId: "thread-1",
        terminalId: DEFAULT_TERMINAL_ID,
        data: "line\n",
      }),
    ).toBe(true);
  });

  it("accepts exited events", () => {
    expect(
      decodes(TerminalEvent, {
        type: "exited",
        threadId: "thread-1",
        terminalId: DEFAULT_TERMINAL_ID,
        exitCode: 0,
        exitSignal: null,
      }),
    ).toBe(true);
  });

  it("accepts closed events", () => {
    expect(
      decodes(TerminalEvent, {
        type: "closed",
        threadId: "thread-1",
        terminalId: DEFAULT_TERMINAL_ID,
      }),
    ).toBe(true);
  });

  it("accepts activity events", () => {
    expect(
      decodes(TerminalEvent, {
        type: "activity",
        threadId: "thread-1",
        terminalId: DEFAULT_TERMINAL_ID,
        hasRunningSubprocess: true,
        label: "vim",
      }),
    ).toBe(true);
  });

  it("accepts started events with snapshot worktree metadata", () => {
    expect(
      decodes(TerminalEvent, {
        type: "started",
        threadId: "thread-1",
        terminalId: DEFAULT_TERMINAL_ID,
        snapshot: {
          threadId: "thread-1",
          terminalId: DEFAULT_TERMINAL_ID,
          cwd: "/tmp/project/.coddy/studio/worktrees/feature-a",
          worktreePath: "/tmp/project/.coddy/studio/worktrees/feature-a",
          status: "running",
          pid: 1234,
          history: "",
          exitCode: null,
          exitSignal: null,
          label: "Primary",
          updatedAt: isoTimestamp,
        },
      }),
    ).toBe(true);
  });

  it("accepts error, cleared, and restarted union alternatives", () => {
    const base = { threadId: "thread-1", terminalId: DEFAULT_TERMINAL_ID };
    expect(
      decodeSync(TerminalEvent, { ...base, type: "error", message: "shell failed" }).type,
    ).toBe("error");
    expect(decodeSync(TerminalEvent, { ...base, type: "cleared" }).type).toBe("cleared");
    expect(
      decodeSync(TerminalEvent, {
        ...base,
        type: "restarted",
        snapshot: {
          ...base,
          cwd: "/tmp/project",
          worktreePath: null,
          status: "running",
          pid: 1234,
          history: "",
          exitCode: null,
          exitSignal: null,
          label: "Primary",
          updatedAt: isoTimestamp,
        },
      }).type,
    ).toBe("restarted");
  });

  it("decodes attach snapshot and output stream alternatives", () => {
    expect(
      decodeSync(TerminalAttachStreamEvent, {
        type: "snapshot",
        snapshot: {
          threadId: "thread-1",
          terminalId: DEFAULT_TERMINAL_ID,
          cwd: "/tmp/project",
          worktreePath: null,
          status: "running",
          pid: 1234,
          history: "",
          exitCode: null,
          exitSignal: null,
          label: "Primary",
          updatedAt: isoTimestamp,
        },
      }).type,
    ).toBe("snapshot");
    expect(
      decodeSync(TerminalAttachStreamEvent, {
        type: "output",
        threadId: "thread-1",
        terminalId: DEFAULT_TERMINAL_ID,
        data: "ready\n",
      }).type,
    ).toBe("output");
  });
});

describe("terminal boundary schemas", () => {
  it("round-trips restart input with optional metadata", () => {
    const input = {
      threadId: "thread-1",
      terminalId: DEFAULT_TERMINAL_ID,
      cwd: "/tmp/project",
      worktreePath: null,
      cols: 120,
      rows: 40,
      env: { TERM: "xterm-256color" },
    };
    const decoded = decodeSync(TerminalRestartInput, input);
    expect(encodeTerminalRestartInput(decoded)).toEqual(input);
  });

  it("reports invalid dimensions on decode and encode", () => {
    const invalid = {
      threadId: "thread-1",
      terminalId: DEFAULT_TERMINAL_ID,
      cols: 0,
      rows: 24,
    };
    const expected = {
      rootTag: "Composite" as const,
      paths: [["cols"]],
      containsTag: "InvalidValue" as const,
    };
    expectDecodeFailure(TerminalResizeInput, invalid, expected);
    expectEncodeFailure(TerminalResizeInput, invalid, expected);
  });
});

describe("terminal errors", () => {
  it("decodes a terminal close failure", () => {
    expect(
      decodeTerminalError({
        _tag: "TerminalCloseError",
        reason: "Terminal processes did not exit before cleanup timed out.",
      }),
    ).toMatchObject({
      _tag: "TerminalCloseError",
      reason: "Terminal processes did not exit before cleanup timed out.",
    });
  });

  it("decodes a bounded terminal spawn failure without exposing process details", () => {
    expect(
      decodeTerminalError({
        _tag: "TerminalSpawnError",
        reason: "Terminal process could not be started.",
      }),
    ).toMatchObject({
      _tag: "TerminalSpawnError",
      reason: "Terminal process could not be started.",
    });
  });

  it("rejects terminal spawn reasons beyond the wire bound", () => {
    const invalid = {
      _tag: "TerminalSpawnError",
      reason: "x".repeat(513),
    };
    const expected = {
      rootTag: "AnyOf" as const,
      paths: [["reason"]],
      containsTag: "InvalidValue" as const,
    };
    expectDecodeFailure(TerminalError, invalid, expected);
  });

  const errors = [
    new TerminalCwdNotFoundError({ cwd: "/missing" }),
    new TerminalCwdNotDirectoryError({ cwd: "/file.txt" }),
    new TerminalCwdStatError({ cwd: "/denied", cause: "permission denied" }),
    new TerminalSpawnError({ reason: "Terminal process could not be started." }),
    new TerminalHistoryError({
      operation: "migrate",
      threadId: "thread-1",
      terminalId: DEFAULT_TERMINAL_ID,
    }),
    new TerminalSessionLookupError({ threadId: "thread-1", terminalId: "term-9" }),
    new TerminalNotRunningError({ threadId: "thread-1", terminalId: "term-2" }),
    new TerminalWriteError({
      threadId: "thread-1",
      terminalId: DEFAULT_TERMINAL_ID,
      terminalPid: 1234,
      cause: "broken pipe",
    }),
    new TerminalResizeError({
      threadId: "thread-1",
      terminalId: DEFAULT_TERMINAL_ID,
      terminalPid: 1234,
      cols: 120,
      rows: 40,
      cause: "resize failed",
    }),
  ] as const;

  it("constructs every tagged error with operation context", () => {
    expect(errors.map((error) => error.message)).toEqual([
      "Terminal cwd does not exist: /missing",
      "Terminal cwd is not a directory: /file.txt",
      "Failed to access terminal cwd: /denied",
      "Terminal process could not be started.",
      "Failed to migrate terminal history for thread: thread-1, terminal: term-1",
      "Unknown terminal thread: thread-1, terminal: term-9",
      "Terminal is not running for thread: thread-1, terminal: term-2",
      "Failed to write to terminal for thread: thread-1, terminal: term-1, PID: 1234",
      "Failed to resize terminal for thread: thread-1, terminal: term-1, PID: 1234 to 120x40",
    ]);
  });

  it("round-trips every terminal error union alternative", () => {
    for (const error of errors) {
      const encoded = encodeTerminalError(error);
      const decoded = decodeTerminalError(encoded);
      expect(decoded._tag).toBe(error._tag);
    }
  });

  it("reports invalid history operation paths on decode and encode", () => {
    const invalid = {
      _tag: "TerminalHistoryError",
      operation: "delete",
      threadId: "thread-1",
      terminalId: DEFAULT_TERMINAL_ID,
    };
    const expected = {
      rootTag: "AnyOf" as const,
      paths: [["operation"]],
      containsTag: "AnyOf" as const,
    };
    expectDecodeFailure(TerminalError, invalid, expected);
    expectEncodeFailure(
      TerminalError,
      makeInvalidClassInstance(TerminalHistoryError.prototype, invalid),
      expected,
    );
  });
});
