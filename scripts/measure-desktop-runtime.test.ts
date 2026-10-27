// @effect-diagnostics nodeBuiltinImport:off
import * as NodeEvents from "node:events";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as NodeTimersPromises from "node:timers/promises";

import { assert, it } from "@effect/vitest";
import { expect } from "vite-plus/test";

import {
  buildWindowsProcessRowsCommand,
  cleanupRuntimeProcess,
  collectOwnedProcessTree,
  collectProcessTree,
  currentEpochMs,
  currentSampledAt,
  formatBytes,
  measureDesktopRuntime,
  makeRuntimeMeasurementHost,
  makeRuntimeProcessRowsCommandRunner,
  makeRuntimeProcessRowsReader,
  makeRuntimeSleep,
  monitorRuntimeProcessOwnership,
  normalizeWindowsProcessRows,
  parsePosixProcessRows,
  parseMeasureDesktopRuntimeArgs,
  renderMeasurementMarkdown,
  runMeasureDesktopRuntimeCli,
  runMeasureDesktopRuntimeMain,
  spawnDesktopProcess,
  setProcessExitCode,
  summarizeProcessTree,
  waitForReadyUrl,
  waitForWindowTitle,
  writeTextFile,
  type RuntimeMeasurementHost,
  type RuntimeMeasurementSystem,
  type RuntimeProcessIdentity,
  type RuntimeProcessRow,
} from "./measure-desktop-runtime.ts";

const rows: ReadonlyArray<RuntimeProcessRow> = [
  {
    pid: 10,
    ppid: 1,
    processName: "Coddy Studio",
    command: "Coddy Studio.exe",
    privateBytes: 100,
    workingSetBytes: 150,
    mainWindowTitle: "Coddy Studio",
  },
  {
    pid: 11,
    ppid: 10,
    processName: "node",
    command: "node server.js",
    privateBytes: 200,
    workingSetBytes: 250,
    mainWindowTitle: "",
  },
  {
    pid: 12,
    ppid: 11,
    processName: "powershell",
    command: "powershell diagnostics",
    privateBytes: 50,
    workingSetBytes: 75,
    mainWindowTitle: "",
  },
  {
    pid: 99,
    ppid: 1,
    processName: "unrelated",
    command: "other.exe",
    privateBytes: 999,
    workingSetBytes: 999,
    mainWindowTitle: "",
  },
];

it("collects a process tree rooted at the launched desktop process", () => {
  assert.deepStrictEqual(
    collectProcessTree(rows, 10).map((row) => row.pid),
    [10, 11, 12],
  );
  assert.deepStrictEqual(
    collectProcessTree(
      [...rows, { ...rows[0]!, pid: 13, ppid: 12 }, { ...rows[0]!, pid: 10, ppid: 13 }],
      10,
    ).map((row) => row.pid),
    [10, 11, 12, 13, 10],
  );
});

it("ignores stale children that predate the launched desktop process", () => {
  const measuredRows: ReadonlyArray<RuntimeProcessRow> = [
    {
      pid: 10,
      ppid: 1,
      processName: "Coddy Studio",
      command: "Coddy Studio.exe",
      privateBytes: 100,
      workingSetBytes: 150,
      mainWindowTitle: "Coddy Studio",
      startedAtEpochMs: 10_000,
    },
    {
      pid: 11,
      ppid: 10,
      processName: "node",
      command: "node server.js",
      privateBytes: 200,
      workingSetBytes: 250,
      mainWindowTitle: "",
      startedAtEpochMs: 11_000,
    },
    {
      pid: 12,
      ppid: 11,
      processName: "WebView2",
      command: "msedgewebview2.exe",
      privateBytes: 300,
      workingSetBytes: 350,
      mainWindowTitle: "",
      startedAtEpochMs: 12_000,
    },
    {
      pid: 20,
      ppid: 10,
      processName: "stale-child",
      command: "old.exe",
      privateBytes: 999,
      workingSetBytes: 999,
      mainWindowTitle: "",
      startedAtEpochMs: 1_000,
    },
  ];

  assert.deepStrictEqual(
    collectProcessTree(measuredRows, 10).map((row) => row.pid),
    [10, 11, 12],
  );
});

it("summarizes private and working-set memory for a desktop process tree", () => {
  const summary = summarizeProcessTree({
    label: "tauri-win",
    rootPid: 10,
    rows,
    sampledAt: "2026-07-09T08:00:00.000Z",
    startupMs: 1_250,
    idleMs: 30_000,
  });

  assert.deepStrictEqual(summary.processIds, [10, 11, 12]);
  assert.equal(summary.processCount, 3);
  assert.equal(summary.totalPrivateBytes, 350);
  assert.equal(summary.totalWorkingSetBytes, 475);
  assert.equal(summary.privateBytesMetric, "private");
  assert.equal(summary.topProcesses[0]?.pid, 11);
  assert.equal(summary.startupMs, 1_250);
});

it("parses runtime measurement CLI flags with repeated args and environment entries", () => {
  assert.deepStrictEqual(
    parseMeasureDesktopRuntimeArgs([
      "--label",
      "tauri-win",
      "--command",
      "Coddy Studio.exe",
      "--arg",
      "--inspect",
      "--arg",
      "value",
      "--cwd",
      "X:/repo",
      "--ready-url",
      "http://127.0.0.1:3773/.well-known/coddy-studio/environment",
      "--window-title",
      "Coddy Studio",
      "--idle-ms",
      "5000",
      "--timeout-ms",
      "60000",
      "--env",
      "CODDY_STUDIO_HOME=X:/tmp/coddy-studio",
      "--keep-running",
      "--json-out",
      "out/runtime.json",
      "--markdown-out",
      "out/runtime.md",
    ]),
    {
      label: "tauri-win",
      command: "Coddy Studio.exe",
      args: ["--inspect", "value"],
      cwd: "X:/repo",
      readyUrl: "http://127.0.0.1:3773/.well-known/coddy-studio/environment",
      windowTitle: "Coddy Studio",
      idleMs: 5_000,
      timeoutMs: 60_000,
      env: { CODDY_STUDIO_HOME: "X:/tmp/coddy-studio" },
      keepRunning: true,
      jsonOut: "out/runtime.json",
      markdownOut: "out/runtime.md",
    },
  );
});

it("builds a PowerShell process sampler command without invalid hash literals", () => {
  const command = buildWindowsProcessRowsCommand();

  assert.equal(/@\{;/.test(command), false);
  assert.equal(/\{\s*;/.test(command), false);
  assert.equal(/\[pscustomobject\]@\{/.test(command), true);
  assert.equal(command.includes("CreationTimeIso"), true);
});

it("formats measurement summaries for baseline documentation", () => {
  const summary = summarizeProcessTree({
    label: "tauri-win",
    rootPid: 10,
    rows,
    sampledAt: "2026-07-09T08:00:00.000Z",
    startupMs: 1_250,
    idleMs: 30_000,
  });

  assert.equal(formatBytes(45_395_200), "43.3 MiB");
  assert.equal(formatBytes(2_048), "2.0 KiB");
  assert.equal(formatBytes(2 * 1024 * 1024 * 1024), "2.00 GiB");
  assert.include(renderMeasurementMarkdown(summary), "| Total private bytes | 350 bytes |");
  assert.include(renderMeasurementMarkdown(summary), "| Startup readiness | 1.25 s |");
  assert.include(renderMeasurementMarkdown(summary), "node server.js");
  assert.include(
    renderMeasurementMarkdown(
      summarizeProcessTree({
        label: "empty",
        rootPid: 404,
        rows: [],
        sampledAt: "2026-07-14T00:00:00.000Z",
        idleMs: 1,
      }),
    ),
    "| - | - | - | - | - |",
  );
});

it("reports missing desktop commands as measurement errors", async () => {
  await expect(
    measureDesktopRuntime(
      {
        label: "missing-command",
        command: "coddy-studio-missing-desktop-command-for-test",
        args: [],
        idleMs: 1,
        timeoutMs: 1,
        env: {},
        keepRunning: false,
      },
      makeMeasurementHost({
        spawn: async () => {
          throw new Error(
            "Failed to start desktop process 'coddy-studio-missing-desktop-command-for-test'",
          );
        },
      }),
    ),
  ).rejects.toThrow("Failed to start desktop process 'coddy-studio-missing-desktop-command-for-test'");
});

it("rejects incomplete and malformed CLI values", () => {
  const invalidCases = [
    [[], "--label is required"],
    [["--label", "desktop"], "--command is required"],
    [["--label", "desktop", "--command", "app", "--idle-ms", "0"], "positive integer"],
    [["--label", "desktop", "--command", "app", "--timeout-ms", "1.5"], "positive integer"],
    [["--label", "desktop", "--command", "app", "--env", "BROKEN"], "Expected KEY=VALUE"],
    [["--label", "desktop", "--command", "app", "--arg"], "--arg requires a value"],
  ] as const;

  for (const [argv, message] of invalidCases) {
    assert.throws(() => parseMeasureDesktopRuntimeArgs(argv), message);
  }

  assert.deepStrictEqual(
    parseMeasureDesktopRuntimeArgs([
      "--label=desktop",
      "--command=app",
      "--env=EMPTY=",
      "--verbose",
    ]),
    {
      label: "desktop",
      command: "app",
      args: [],
      idleMs: 30_000,
      timeoutMs: 120_000,
      env: { EMPTY: "" },
      keepRunning: false,
      verbose: true,
    },
  );
});

it("normalizes Windows and POSIX process sampler rows", () => {
  assert.deepStrictEqual(normalizeWindowsProcessRows(0), []);
  assert.deepStrictEqual(normalizeWindowsProcessRows(null), []);
  assert.deepStrictEqual(normalizeWindowsProcessRows([null, "bad", { ProcessId: 0 }]), []);
  assert.deepStrictEqual(
    normalizeWindowsProcessRows({
      ProcessId: 42,
      ParentProcessId: 7,
      ProcessName: "coddy-studio",
      CommandLine: "",
      PrivateMemorySize64: 100,
      WorkingSet64: 0,
      WorkingSetSize: 200,
      MainWindowTitle: "Coddy Studio",
      CreationTimeIso: "2026-07-14T00:00:00.000Z",
    }),
    [
      {
        pid: 42,
        ppid: 7,
        processName: "coddy-studio",
        command: "coddy-studio",
        privateBytes: 100,
        workingSetBytes: 200,
        mainWindowTitle: "Coddy Studio",
        startedAtEpochMs: Date.parse("2026-07-14T00:00:00.000Z"),
      },
    ],
  );
  assert.deepStrictEqual(
    normalizeWindowsProcessRows({
      ProcessId: 8,
      ParentProcessId: Number.NaN,
      ProcessName: 99,
      CommandLine: "explicit command",
      PrivateMemorySize64: Number.POSITIVE_INFINITY,
      WorkingSet64: 12,
      MainWindowTitle: null,
      CreationTimeIso: "invalid",
    }),
    [
      {
        pid: 8,
        ppid: 0,
        processName: "",
        command: "explicit command",
        privateBytes: 0,
        workingSetBytes: 12,
        mainWindowTitle: "",
      },
    ],
  );
  assert.deepStrictEqual(parsePosixProcessRows(" 10 1 32 app /bin/app --flag\ninvalid\n"), [
    {
      pid: 10,
      ppid: 1,
      processName: "app",
      command: "/bin/app --flag",
      privateBytes: 32 * 1024,
      privateBytesMetric: "rss-approximation",
      workingSetBytes: 32 * 1024,
      mainWindowTitle: "",
    },
  ]);
});

it("executes both platform sampler command plans through the abortable reader", async () => {
  const calls: Array<{ command: string; signal: AbortSignal }> = [];
  const run = async (
    command: string,
    _args: ReadonlyArray<string>,
    options: { signal: AbortSignal },
  ) => {
    calls.push({ command, signal: options.signal });
    return command === "powershell.exe"
      ? JSON.stringify({ ProcessId: 42, ParentProcessId: 1, ProcessName: "coddy-studio" })
      : " 9 1 4 app /bin/app\n";
  };
  const signal = new AbortController().signal;
  assert.equal((await makeRuntimeProcessRowsReader("win32", run)(signal))[0]?.pid, 42);
  assert.equal((await makeRuntimeProcessRowsReader("linux", run)(signal))[0]?.pid, 9);
  assert.deepStrictEqual(await makeRuntimeProcessRowsReader("win32", async () => "")(signal), []);
  assert.deepStrictEqual(
    calls.map(({ command }) => command),
    ["powershell.exe", "ps"],
  );
  assert.isTrue(calls.every((call) => call.signal === signal));
});

it("does not export duplicate synchronous process-row readers", async () => {
  const runtimeModule: Record<string, unknown> = await import("./measure-desktop-runtime.ts");
  for (const name of ["readWindowsProcessRows", "readPosixProcessRows", "readCurrentProcessRows"]) {
    assert.isFalse(name in runtimeModule, `${name} must not be exported`);
  }
});

it("builds cancellable default runtime adapters around injected Node primitives", async () => {
  const sleepCalls: unknown[] = [];
  const sleep = makeRuntimeSleep(async (...args) => {
    sleepCalls.push(args);
  });
  const signal = new AbortController().signal;
  await sleep(25, signal);
  await sleep(10);
  assert.deepStrictEqual(sleepCalls, [
    [25, undefined, { signal }],
    [10, undefined, undefined],
  ]);

  const commandCalls: string[] = [];
  const run = makeRuntimeProcessRowsCommandRunner((command, _args, _options, callback) => {
    commandCalls.push(command);
    callback(null, "rows", "");
    return {} as import("node:child_process").ChildProcess;
  });
  assert.equal(await run("ps", ["-axo"], { encoding: "utf8", maxBuffer: 10, signal }), "rows");
  assert.deepStrictEqual(commandCalls, ["ps"]);
  const failedRun = makeRuntimeProcessRowsCommandRunner((_command, _args, _options, callback) => {
    callback(new Error("sampler failed"), "", "");
    return {} as import("node:child_process").ChildProcess;
  });
  await expect(failedRun("ps", [], { encoding: "utf8", maxBuffer: 10, signal })).rejects.toThrow(
    "sampler failed",
  );
  assert.isAbove(currentEpochMs(), 0);
});

it("polls readiness deterministically and cancels every response body", async () => {
  let now = 0;
  let fetchCalls = 0;
  let cancelCalls = 0;
  let scheduledAbort: (() => void) | undefined;
  const duration = await waitForReadyUrl("http://localhost/ready", 5_000, {
    now: () => now,
    sleep: async (ms) => {
      now += ms;
    },
    fetch: async () => {
      fetchCalls += 1;
      if (fetchCalls === 1) throw new Error("not ready");
      return {
        status: fetchCalls === 2 ? 503 : 204,
        body: {
          cancel: async () => {
            cancelCalls += 1;
            throw new Error("body already closed");
          },
        },
      };
    },
    scheduleAbort: (abort) => {
      scheduledAbort = abort;
      return 1;
    },
    clearAbort: () => undefined,
  });

  assert.equal(duration, 1_000);
  assert.equal(fetchCalls, 3);
  assert.equal(cancelCalls, 2);
  scheduledAbort?.();

  assert.equal(
    await waitForReadyUrl("http://localhost/no-body", 500, {
      now: () => 0,
      sleep: async () => undefined,
      fetch: async () => ({ status: 204 }),
      scheduleAbort: () => 1,
      clearAbort: () => undefined,
    }),
    0,
  );

  await expect(
    waitForReadyUrl("http://localhost/never", 500, {
      now: () => now,
      sleep: async (ms) => {
        now += ms;
      },
      fetch: async () => {
        throw new Error("offline");
      },
      scheduleAbort: () => 1,
      clearAbort: () => undefined,
    }),
  ).rejects.toThrow("Timed out waiting for ready URL");
});

it("polls window readiness and reports timeout", async () => {
  let now = 0;
  let reads = 0;
  const duration = await waitForWindowTitle(10, "coddy-studio", 2_000, {
    now: () => now,
    sleep: async (ms) => {
      now += ms;
    },
    readProcessRows: async () => {
      reads += 1;
      return reads === 1
        ? rows.map((row) => ({ ...row, mainWindowTitle: "" }))
        : [{ ...rows[0]!, mainWindowTitle: "Coddy Studio ready" }];
    },
  });
  assert.equal(duration, 500);

  await expect(
    waitForWindowTitle(10, "missing", 500, {
      now: () => now,
      sleep: async (ms) => {
        now += ms;
      },
      readProcessRows: async () => rows,
    }),
  ).rejects.toThrow("Timed out waiting for window title");
});

it("cancels readiness polling and removes timers and abort listeners", async () => {
  const controller = new AbortController();
  let clearedTimers = 0;
  const polling = waitForReadyUrl(
    "http://localhost/ready",
    5_000,
    {
      now: () => 0,
      sleep: async () => undefined,
      fetch: async (_url, options) =>
        new Promise((_, reject) => {
          options.signal.addEventListener("abort", () => reject(options.signal.reason), {
            once: true,
          });
        }),
      scheduleAbort: () => 1,
      clearAbort: () => {
        clearedTimers += 1;
      },
    },
    controller.signal,
  );
  await Promise.resolve();
  controller.abort(new Error("cancel all readiness"));
  await expect(polling).rejects.toThrow("cancel all readiness");
  assert.equal(clearedTimers, 1);
});

it("rejects readiness work that is already cancelled", async () => {
  const controller = new AbortController();
  controller.abort(new Error("cancelled before polling"));
  await expect(
    waitForReadyUrl(
      "http://localhost/ready",
      1,
      {
        now: () => 0,
        sleep: async () => undefined,
        fetch: async () => ({ status: 204 }),
        scheduleAbort: () => 1,
        clearAbort: () => undefined,
      },
      controller.signal,
    ),
  ).rejects.toThrow("cancelled before polling");
  await expect(
    waitForWindowTitle(
      10,
      "Coddy Studio",
      1,
      { now: () => 0, sleep: async () => undefined, readProcessRows: async () => rows },
      controller.signal,
    ),
  ).rejects.toThrow("cancelled before polling");
});

it("cleans only the owned process group or verified Windows descendants", async () => {
  const identity: RuntimeProcessIdentity = {
    pid: 42,
    processGroupId: 42,
    startedAtEpochMs: 10_000,
  };
  const posixSignals: Array<[number, NodeJS.Signals | 0]> = [];
  let graceSleeps = 0;
  await cleanupRuntimeProcess(identity, {
    platform: "linux",
    execFile: () => "",
    kill(pid, signal) {
      posixSignals.push([pid, signal]);
    },
    readProcessRows: async () => [],
    sleep: async () => {
      graceSleeps += 1;
    },
  });
  assert.deepStrictEqual(posixSignals, [
    [-42, "SIGTERM"],
    [-42, 0],
    [-42, "SIGKILL"],
  ]);
  assert.equal(graceSleeps, 1);

  const windowsCalls: string[] = [];
  await cleanupRuntimeProcess(identity, {
    platform: "win32",
    execFile(command, args) {
      assert.equal(command, "powershell.exe");
      windowsCalls.push(args.at(-1) ?? "");
      return "";
    },
    kill: () => undefined,
    sleep: async () => undefined,
    readProcessRows: async () => [
      { ...rows[0]!, pid: 42, ppid: 1, startedAtEpochMs: 10_000 },
      { ...rows[1]!, pid: 43, ppid: 42, startedAtEpochMs: 10_100 },
      { ...rows[2]!, pid: 44, ppid: 43, startedAtEpochMs: 10_200 },
      { ...rows[3]!, pid: 99, ppid: 1, startedAtEpochMs: 10_200 },
    ],
  });
  assert.include(windowsCalls[0] ?? "", "Stop-Process -Id 44");
  assert.include(windowsCalls[0] ?? "", "10200");
  assert.include(windowsCalls[1] ?? "", "Stop-Process -Id 43");
  assert.include(windowsCalls[2] ?? "", "Stop-Process -Id 42");
  assert.include(windowsCalls.join("\n"), "$actual -eq $expected");
  assert.notInclude(windowsCalls.join("\n"), "TotalMilliseconds");
  assert.deepStrictEqual(
    collectOwnedProcessTree([{ ...rows[0]!, pid: 42, startedAtEpochMs: 1_000 }], identity),
    [],
  );
});

it("cleans retained Windows descendants after the root exits without killing reused identities", async () => {
  const identity: RuntimeProcessIdentity = {
    pid: 42,
    startedAtEpochMs: 10_000,
    windowsOwnedProcessStartTimes: new Map([[42, 10_000]]),
  };
  const observedRows = [
    { ...rows[0]!, pid: 42, ppid: 1, startedAtEpochMs: 10_000 },
    { ...rows[1]!, pid: 43, ppid: 42, startedAtEpochMs: 10_100 },
    { ...rows[2]!, pid: 44, ppid: 43, startedAtEpochMs: 10_200 },
    { ...rows[3]!, pid: 46, ppid: 42, startedAtEpochMs: 10_300 },
  ];
  assert.deepStrictEqual(
    collectOwnedProcessTree(observedRows, identity).map((row) => row.pid),
    [42, 43, 44, 46],
  );

  const windowsCalls: string[] = [];
  await cleanupRuntimeProcess(identity, {
    platform: "win32",
    execFile(_command, args) {
      windowsCalls.push(args.at(-1) ?? "");
      return "";
    },
    kill: () => undefined,
    sleep: async () => undefined,
    readProcessRows: async () => [
      { ...rows[1]!, pid: 43, ppid: 42, startedAtEpochMs: 10_100 },
      { ...rows[2]!, pid: 44, ppid: 43, startedAtEpochMs: 10_200 },
      { ...rows[3]!, pid: 45, ppid: 42, startedAtEpochMs: 10_250 },
      { ...rows[0]!, pid: 46, ppid: 1, startedAtEpochMs: 90_000 },
      { ...rows[0]!, pid: 99, ppid: 1, startedAtEpochMs: 10_100 },
    ],
  });

  assert.equal(windowsCalls.length, 2);
  assert.include(windowsCalls[0] ?? "", "Stop-Process -Id 44");
  assert.include(windowsCalls[1] ?? "", "Stop-Process -Id 43");
  for (const pid of [42, 45, 46, 99]) {
    assert.notInclude(windowsCalls.join("\n"), `Stop-Process -Id ${pid}`);
  }
});

it("rejects near-immediate Windows PID reuse using exact creation identity", () => {
  const rootIdentity: RuntimeProcessIdentity = {
    pid: 42,
    startedAtEpochMs: 10_100,
    windowsOwnedProcessStartTimes: new Map([[42, 10_100]]),
  };
  assert.deepStrictEqual(
    collectOwnedProcessTree(
      [{ ...rows[0]!, pid: 42, ppid: 1, startedAtEpochMs: 10_999 }],
      rootIdentity,
    ),
    [],
  );

  const descendantIdentity: RuntimeProcessIdentity = {
    pid: 42,
    startedAtEpochMs: 10_000,
    windowsOwnedProcessStartTimes: new Map([
      [42, 10_000],
      [43, 10_100],
    ]),
  };
  assert.deepStrictEqual(
    collectOwnedProcessTree(
      [{ ...rows[1]!, pid: 43, ppid: 42, startedAtEpochMs: 10_999 }],
      descendantIdentity,
    ),
    [],
  );
});

it("rejects stale Windows PPID rows and descendants older than their verified parent", () => {
  const identity: RuntimeProcessIdentity = {
    pid: 42,
    startedAtEpochMs: 10_000,
    windowsOwnedProcessStartTimes: new Map([[42, 10_000]]),
  };
  const sampledRows: ReadonlyArray<RuntimeProcessRow> = [
    { ...rows[0]!, pid: 42, ppid: 1, startedAtEpochMs: 10_000 },
    { ...rows[1]!, pid: 43, ppid: 42, startedAtEpochMs: 9_999 },
    { ...rows[2]!, pid: 44, ppid: 43, startedAtEpochMs: 10_500 },
    { ...rows[1]!, pid: 45, ppid: 42, startedAtEpochMs: 10_100 },
    { ...rows[2]!, pid: 46, ppid: 45, startedAtEpochMs: 10_099 },
    { ...rows[3]!, pid: 47, ppid: 45, startedAtEpochMs: 10_200 },
  ];

  assert.deepStrictEqual(
    collectOwnedProcessTree(sampledRows, identity).map((row) => row.pid),
    [42, 45, 47],
  );
  assert.deepStrictEqual(
    [...identity.windowsOwnedProcessStartTimes!.entries()],
    [
      [42, 10_000],
      [45, 10_100],
      [47, 10_200],
    ],
  );
});

it("handles cleanup races without expanding process ownership", async () => {
  const identity: RuntimeProcessIdentity = {
    pid: 42,
    processGroupId: 42,
    startedAtEpochMs: 10_000,
  };
  const base = {
    execFile: () => "",
    readProcessRows: async () => [],
    sleep: async () => undefined,
  };

  await cleanupRuntimeProcess(identity, {
    ...base,
    platform: "win32",
    readProcessRows: async () => Promise.reject(new Error("sampler unavailable")),
    kill: () => undefined,
  });
  await cleanupRuntimeProcess(identity, {
    ...base,
    platform: "win32",
    readProcessRows: async () => [{ ...rows[0]!, pid: 42, startedAtEpochMs: 10_000 }],
    execFile: () => {
      throw new Error("already exited");
    },
    kill: () => undefined,
  });

  let signals = 0;
  await cleanupRuntimeProcess(
    { pid: 42, startedAtEpochMs: 10_000 },
    {
      ...base,
      platform: "linux",
      kill: () => {
        signals += 1;
      },
    },
  );
  assert.equal(signals, 0);

  for (const failAt of ["term", "probe", "force"] as const) {
    const calls: Array<NodeJS.Signals | 0> = [];
    await cleanupRuntimeProcess(identity, {
      ...base,
      platform: "linux",
      kill: (_pid, signal) => {
        calls.push(signal);
        if (
          (failAt === "term" && signal === "SIGTERM") ||
          (failAt === "probe" && signal === 0) ||
          (failAt === "force" && signal === "SIGKILL")
        ) {
          throw new Error("process race");
        }
      },
    });
    assert.equal(calls[0], "SIGTERM");
  }
});

it("creates an owned process group and monitors error, exit, and close until disposal", async () => {
  class FakeChild extends NodeEvents.EventEmitter {
    pid: number | undefined;
    killed = false;
    unrefCalls = 0;
    constructor(pid: number | undefined) {
      super();
      this.pid = pid;
    }
    unref() {
      this.unrefCalls += 1;
    }
    kill() {
      this.killed = true;
      return true;
    }
  }
  const input = {
    label: "desktop",
    command: "app",
    args: ["--flag"],
    idleMs: 1,
    timeoutMs: 1,
    env: { TEST: "1" },
    keepRunning: false,
  } as const;

  const spawnOptions: Array<import("node:child_process").SpawnOptions> = [];
  const child = new FakeChild(77);
  const launched = await spawnDesktopProcess(input, {
    platform: "linux",
    epochNow: () => 12_345,
    spawn: (_command, _args, options) => {
      spawnOptions.push(options);
      return child;
    },
  });
  assert.deepStrictEqual(launched.identity, {
    pid: 77,
    processGroupId: 77,
    startedAtEpochMs: 12_345,
  });
  assert.equal(spawnOptions[0]?.detached, true);
  launched.unref();
  assert.equal(child.unrefCalls, 1);
  child.emit("error", new Error("late error"));
  await expect(launched.termination).rejects.toThrow("late error");
  launched.dispose();
  launched.dispose();
  assert.equal(child.listenerCount("error"), 0);
  assert.equal(child.listenerCount("exit"), 0);
  assert.equal(child.listenerCount("close"), 0);

  const exactWindowsChild = new FakeChild(79);
  const exactWindowsLaunch = await spawnDesktopProcess(input, {
    platform: "win32",
    epochNow: () => 10_000,
    spawn: () => exactWindowsChild,
    readProcessRows: async () => [
      { ...rows[0]!, pid: 79, ppid: 1, startedAtEpochMs: 10_100 },
      { ...rows[1]!, pid: 80, ppid: 79, startedAtEpochMs: 10_200 },
      { ...rows[2]!, pid: 81, ppid: 80, startedAtEpochMs: 10_300 },
    ],
  });
  assert.equal(exactWindowsLaunch.identity.startedAtEpochMs, 10_100);
  assert.deepStrictEqual(
    [...exactWindowsLaunch.identity.windowsOwnedProcessStartTimes!.entries()],
    [
      [79, 10_100],
      [80, 10_200],
      [81, 10_300],
    ],
  );
  const firstMonitorRows = [
    { ...rows[1]!, pid: 80, ppid: 1, startedAtEpochMs: 10_200 },
    { ...rows[2]!, pid: 81, ppid: 80, startedAtEpochMs: 10_300 },
  ];
  const firstMonitorController = new AbortController();
  await expect(
    monitorRuntimeProcessOwnership(
      exactWindowsLaunch.identity,
      {
        readProcessRows: async () => firstMonitorRows,
        sleep: async () => {
          firstMonitorController.abort(new Error("first monitor iteration completed"));
          throw firstMonitorController.signal.reason;
        },
      },
      firstMonitorController.signal,
    ),
  ).rejects.toThrow("first monitor iteration completed");
  assert.deepStrictEqual(
    collectOwnedProcessTree(firstMonitorRows, exactWindowsLaunch.identity).map((row) => row.pid),
    [80, 81],
  );
  exactWindowsLaunch.dispose();

  const missingReaderChild = new FakeChild(80);
  await expect(
    spawnDesktopProcess(input, {
      platform: "win32",
      epochNow: () => 1,
      spawn: () => missingReaderChild,
    }),
  ).rejects.toThrow("Cannot verify Windows process identity");
  assert.isTrue(missingReaderChild.killed);

  const missingRootChild = new FakeChild(81);
  await expect(
    spawnDesktopProcess(input, {
      platform: "win32",
      epochNow: () => 1,
      spawn: () => missingRootChild,
      readProcessRows: async () => [],
    }),
  ).rejects.toThrow("Cannot verify Windows process identity");
  assert.isTrue(missingRootChild.killed);

  const failedSamplerChild = new FakeChild(82);
  failedSamplerChild.kill = () => {
    throw new Error("already exited");
  };
  await expect(
    spawnDesktopProcess(input, {
      platform: "win32",
      epochNow: () => 1,
      spawn: () => failedSamplerChild,
      readProcessRows: async () => Promise.reject(new Error("sampler failed")),
    }),
  ).rejects.toThrow("sampler failed");

  const stringFailureChild = new FakeChild(83);
  await expect(
    spawnDesktopProcess(input, {
      platform: "win32",
      epochNow: () => 1,
      spawn: () => stringFailureChild,
      readProcessRows: async () => Promise.reject("sampler unavailable"),
    }),
  ).rejects.toThrow("sampler unavailable");

  for (const event of ["exit", "close"] as const) {
    const terminatedChild = new FakeChild(78);
    const monitored = await spawnDesktopProcess(input, {
      platform: "win32",
      epochNow: () => 12_346,
      spawn: () => terminatedChild,
      readProcessRows: async () => [{ ...rows[0]!, pid: 78, ppid: 1, startedAtEpochMs: 12_346 }],
    });
    terminatedChild.emit(event, event === "exit" ? 17 : null, event === "close" ? "SIGTERM" : null);
    terminatedChild.emit(event === "exit" ? "close" : "exit", null, null);
    await expect(monitored.termination).rejects.toThrow(
      event === "exit" ? "exited unexpectedly" : "closed unexpectedly",
    );
    monitored.dispose();
  }

  await expect(
    spawnDesktopProcess(input, {
      platform: "win32",
      epochNow: () => 1,
      spawn: () => {
        const failed = new FakeChild(undefined);
        queueMicrotask(() => failed.emit("error", new Error("spawn denied")));
        return failed;
      },
    }),
  ).rejects.toThrow("spawn denied");

  await expect(
    spawnDesktopProcess(input, {
      platform: "win32",
      epochNow: () => 1,
      spawn: () => new FakeChild(undefined),
    }),
  ).rejects.toThrow("process id was not assigned");
});

function makeLaunchedProcess(
  termination: Promise<never> = new Promise(() => undefined),
): Awaited<ReturnType<RuntimeMeasurementHost["spawn"]>> {
  return {
    pid: 10,
    killed: false,
    identity: { pid: 10, processGroupId: 10, startedAtEpochMs: 1_000 },
    termination,
    dispose: () => undefined,
    unref: () => undefined,
  };
}

it("cancels and awaits sibling readiness gates before process cleanup", async () => {
  let siblingSettled = false;
  let cleanupSawSiblingSettled = false;
  const host = makeMeasurementHost({
    waitForReadyUrl: async () => {
      throw new Error("ready URL failed");
    },
    waitForWindowTitle: async (_rootPid, _title, _timeout, signal) =>
      new Promise((_, reject) => {
        signal.addEventListener(
          "abort",
          () => {
            queueMicrotask(() => {
              siblingSettled = true;
              reject(signal.reason);
            });
          },
          { once: true },
        );
      }),
    cleanup: async () => {
      cleanupSawSiblingSettled = siblingSettled;
    },
  });

  await expect(
    measureDesktopRuntime(
      {
        label: "desktop",
        command: "app",
        args: [],
        readyUrl: "http://localhost/ready",
        windowTitle: "Coddy Studio",
        idleMs: 1,
        timeoutMs: 10,
        env: {},
        keepRunning: false,
      },
      host,
    ),
  ).rejects.toThrow("ready URL failed");
  assert.isTrue(siblingSettled);
  assert.isTrue(cleanupSawSiblingSettled);
});

it("retains reparented descendants when the root exits during every pre-sample phase", async () => {
  for (const phase of ["url", "window", "idle"] as const) {
    let rejectTermination!: (cause: unknown) => void;
    const termination = new Promise<never>((_resolve, reject) => {
      rejectTermination = reject;
    });
    const identity: RuntimeProcessIdentity = {
      pid: 10,
      startedAtEpochMs: 10_000,
      windowsOwnedProcessStartTimes: new Map([[10, 10_000]]),
    };
    const launched = { ...makeLaunchedProcess(termination), identity };
    const observedRows: ReadonlyArray<RuntimeProcessRow> = [
      { ...rows[0]!, pid: 10, ppid: 1, startedAtEpochMs: 10_000 },
      { ...rows[1]!, pid: 11, ppid: 10, startedAtEpochMs: 10_100 },
      { ...rows[2]!, pid: 12, ppid: 10, startedAtEpochMs: 10_200 },
    ];
    const cleanupRows: ReadonlyArray<RuntimeProcessRow> = [
      { ...rows[1]!, pid: 11, ppid: 1, startedAtEpochMs: 10_100 },
      { ...rows[2]!, pid: 12, ppid: 1, startedAtEpochMs: 10_999 },
      { ...rows[3]!, pid: 99, ppid: 1, startedAtEpochMs: 10_100 },
    ];
    let terminationScheduled = false;
    let cleanupPids: ReadonlyArray<number> = [];
    const scheduleTermination = () => {
      if (terminationScheduled) return;
      terminationScheduled = true;
      queueMicrotask(() => rejectTermination(new Error(`root exited during ${phase}`)));
    };
    const waitForAbort = (signal: AbortSignal) =>
      new Promise<void>((resolve) => {
        if (signal.aborted) {
          resolve();
          return;
        }
        signal.addEventListener("abort", () => resolve(), { once: true });
      });
    const host = makeMeasurementHost({
      spawn: async () => launched,
      readProcessRows: async () => observedRows,
      monitorOwnership: (retainedIdentity, signal) =>
        monitorRuntimeProcessOwnership(
          retainedIdentity,
          {
            readProcessRows: async () => observedRows,
            sleep: async (_ms, monitorSignal) => {
              if (monitorSignal) await waitForAbort(monitorSignal);
            },
          },
          signal,
        ),
      waitForReadyUrl: async (_url, _timeout, signal) => {
        scheduleTermination();
        await waitForAbort(signal);
        return 0;
      },
      waitForWindowTitle: async (_pid, _title, _timeout, signal) => {
        scheduleTermination();
        await waitForAbort(signal);
        return 0;
      },
      sleep: async (ms, signal) => {
        if (ms === 777) scheduleTermination();
        if (signal) await waitForAbort(signal);
      },
      cleanup: async (retainedIdentity) => {
        cleanupPids = collectOwnedProcessTree(cleanupRows, retainedIdentity).map((row) => row.pid);
      },
    });

    await expect(
      measureDesktopRuntime(
        {
          label: phase,
          command: "app",
          args: [],
          ...(phase === "url" ? { readyUrl: "http://localhost:5733/ready" } : {}),
          ...(phase === "window" ? { windowTitle: "Coddy Studio" } : {}),
          idleMs: 777,
          timeoutMs: 100,
          env: {},
          keepRunning: false,
        },
        host,
      ),
    ).rejects.toThrow(`root exited during ${phase}`);
    assert.deepStrictEqual(cleanupPids, [11]);
  }
});

it("settles ownership monitoring on cancellation and propagates sampler failures", async () => {
  const identity: RuntimeProcessIdentity = {
    pid: 10,
    startedAtEpochMs: 10_000,
    windowsOwnedProcessStartTimes: new Map([[10, 10_000]]),
  };
  const preCancelled = new AbortController();
  preCancelled.abort(new Error("cancelled before ownership sampling"));
  await expect(
    monitorRuntimeProcessOwnership(
      identity,
      { readProcessRows: async () => rows, sleep: async () => undefined },
      preCancelled.signal,
    ),
  ).rejects.toThrow("cancelled before ownership sampling");

  const duringRead = new AbortController();
  await expect(
    monitorRuntimeProcessOwnership(
      identity,
      {
        readProcessRows: async (signal) => {
          duringRead.abort(new Error("cancelled during ownership sampling"));
          throw signal.reason;
        },
        sleep: async () => undefined,
      },
      duringRead.signal,
    ),
  ).rejects.toThrow("cancelled during ownership sampling");

  await expect(
    monitorRuntimeProcessOwnership(
      identity,
      {
        readProcessRows: async () => Promise.reject(new Error("ownership sampler failed")),
        sleep: async () => undefined,
      },
      new AbortController().signal,
    ),
  ).rejects.toThrow("ownership sampler failed");
});

it("cancels an in-flight process-table read when the child dies", async () => {
  let rejectTermination!: (cause: Error) => void;
  const termination = new Promise<never>((_resolve, reject) => {
    rejectTermination = reject;
  });
  let readSettled = false;
  let cleanupSawReadSettled = false;
  const host = makeMeasurementHost({
    spawn: async () => makeLaunchedProcess(termination),
    readProcessRows: async (signal) =>
      new Promise((_, reject) => {
        signal.addEventListener(
          "abort",
          () => {
            readSettled = true;
            reject(signal.reason);
          },
          { once: true },
        );
        queueMicrotask(() => rejectTermination(new Error("desktop exited during sampling")));
      }),
    cleanup: async () => {
      cleanupSawReadSettled = readSettled;
    },
  });

  await expect(
    measureDesktopRuntime(
      {
        label: "desktop",
        command: "app",
        args: [],
        idleMs: 1,
        timeoutMs: 10,
        env: {},
        keepRunning: false,
      },
      host,
    ),
  ).rejects.toThrow("desktop exited during sampling");
  assert.isTrue(readSettled);
  assert.isTrue(cleanupSawReadSettled);
});

it("rejects empty and zero-memory process samples", async () => {
  let cleanupCalls = 0;
  for (const sampledRows of [[], [{ ...rows[0]!, privateBytes: 0, workingSetBytes: 0 }]]) {
    const host = makeMeasurementHost({
      readProcessRows: async () => sampledRows,
      cleanup: async () => {
        cleanupCalls += 1;
      },
    });
    await expect(
      measureDesktopRuntime(
        {
          label: "desktop",
          command: "app",
          args: [],
          idleMs: 1,
          timeoutMs: 10,
          env: {},
          keepRunning: false,
        },
        host,
      ),
    ).rejects.toThrow(/process sample|memory totals/);
  }
  assert.equal(cleanupCalls, 2);
});

it("labels POSIX RSS compatibility values as an approximation", () => {
  const posixRows = parsePosixProcessRows(" 10 1 32 app /bin/app\n");
  const summary = summarizeProcessTree({
    label: "posix",
    rootPid: 10,
    rows: posixRows,
    sampledAt: "2026-07-14T00:00:00.000Z",
    idleMs: 1,
  });
  assert.equal(summary.privateBytesMetric, "rss-approximation");
  const markdown = renderMeasurementMarkdown(summary);
  assert.include(markdown, "RSS approximation");
  assert.include(markdown, "| PID | Name | RSS (approx.) | Working Set | Command |");
  assert.notInclude(markdown, "| PID | Name | Private | Working Set | Command |");
});

function makeMeasurementHost(
  overrides: Partial<RuntimeMeasurementHost> = {},
): RuntimeMeasurementHost {
  const child = makeLaunchedProcess();
  return {
    spawn: async () => child,
    monitorOwnership: async (_identity, signal) => {
      if (signal.aborted) throw signal.reason;
      await new Promise<void>((_resolve, reject) => {
        signal.addEventListener("abort", () => reject(signal.reason), { once: true });
      });
    },
    waitForReadyUrl: async () => 125,
    waitForWindowTitle: async () => 250,
    sleep: async () => undefined,
    now: () => 50,
    sampledAt: () => "2026-07-14T00:00:00.000Z",
    readProcessRows: async () =>
      rows.map((row) => (row.pid === 10 ? { ...row, startedAtEpochMs: 1_000 } : row)),
    cleanup: async () => undefined,
    writeTextFile(filePath, text) {
      NodeFS.mkdirSync(NodePath.dirname(filePath), { recursive: true });
      NodeFS.writeFileSync(filePath, text);
    },
    stdout: () => undefined,
    stderr: () => undefined,
    ...overrides,
  };
}

it("composes every runtime host operation from injected system dependencies", async () => {
  class FakeChild extends NodeEvents.EventEmitter {
    pid = 10;
    killed = false;
    unref() {}
    kill() {
      this.killed = true;
      return true;
    }
  }
  const calls: string[] = [];
  let now = 0;
  const system: RuntimeMeasurementSystem = {
    platform: "win32",
    spawnProcess: () => new FakeChild(),
    epochNow: () => 1_000,
    now: () => now,
    sampledAt: () => "2026-07-14T00:00:00.000Z",
    sleep: async (ms) => {
      now += ms;
    },
    fetch: async () => ({ status: 204 }),
    scheduleAbort: () => 1,
    clearAbort: () => undefined,
    readProcessRows: async () =>
      rows.map((row) => (row.pid === 10 ? { ...row, startedAtEpochMs: 1_000 } : row)),
    execFile: (command) => {
      calls.push(command);
      return "";
    },
    kill: () => undefined,
    writeTextFile: (filePath) => calls.push(filePath),
    stdout: (text) => calls.push(text),
    stderr: (text) => calls.push(text),
  };
  const host = makeRuntimeMeasurementHost(system);

  assert.equal(
    (
      await host.spawn({
        label: "desktop",
        command: "app",
        args: [],
        idleMs: 1,
        timeoutMs: 1,
        env: {},
        keepRunning: false,
      })
    ).pid,
    10,
  );
  const signal = new AbortController().signal;
  assert.equal(await host.waitForReadyUrl("http://localhost/ready", 1, signal), 0);
  assert.equal(await host.waitForWindowTitle(10, "Coddy Studio", 1, signal), 0);
  const monitorController = new AbortController();
  monitorController.abort(new Error("stop composed monitor"));
  if (!host.monitorOwnership) throw new Error("Windows host did not compose ownership monitoring.");
  await expect(
    host.monitorOwnership(
      {
        pid: 10,
        startedAtEpochMs: 1_000,
        windowsOwnedProcessStartTimes: new Map([[10, 1_000]]),
      },
      monitorController.signal,
    ),
  ).rejects.toThrow("stop composed monitor");
  await host.cleanup({ pid: 10, processGroupId: 10, startedAtEpochMs: 1_000 });
  const linuxSignals: Array<NodeJS.Signals | 0> = [];
  const linuxHost = makeRuntimeMeasurementHost({
    ...system,
    platform: "linux",
    kill: (_pid, signal) => {
      linuxSignals.push(signal);
    },
  });
  await linuxHost.cleanup({ pid: 10, processGroupId: 10, startedAtEpochMs: 1_000 });
  assert.deepStrictEqual(linuxSignals, ["SIGTERM", 0, "SIGKILL"]);
  host.writeTextFile("artifact.json", "{}");
  host.stdout("out");
  host.stderr("err");
  assert.include(calls, "powershell.exe");
  assert.include(calls, "artifact.json");
});

it("skips periodic ownership sampling for POSIX measurements", async () => {
  class FakeChild extends NodeEvents.EventEmitter {
    pid = 10;
    killed = false;
    unref() {}
    kill() {
      this.killed = true;
      return true;
    }
  }
  let processReads = 0;
  const host = makeRuntimeMeasurementHost({
    platform: "linux",
    spawnProcess: () => new FakeChild(),
    epochNow: () => 1_000,
    now: () => 0,
    sampledAt: () => "2026-07-14T00:00:00.000Z",
    sleep: async (ms) => {
      if (ms === 100) return;
      await NodeTimersPromises.setTimeout(0);
    },
    fetch: async () => ({ status: 204 }),
    scheduleAbort: () => 1,
    clearAbort: () => undefined,
    readProcessRows: async () => {
      processReads += 1;
      if (processReads > 1) throw new Error("periodic ps read failed");
      return [{ ...rows[0]!, pid: 10, ppid: 1, startedAtEpochMs: 1_000 }];
    },
    execFile: () => "",
    kill: () => undefined,
    writeTextFile: () => undefined,
    stdout: () => undefined,
    stderr: () => undefined,
  });

  const summary = await measureDesktopRuntime(
    {
      label: "posix",
      command: "app",
      args: [],
      idleMs: 1,
      timeoutMs: 10,
      env: {},
      keepRunning: true,
    },
    host,
  );
  assert.notProperty(host, "monitorOwnership");
  assert.equal(summary.processCount, 1);
  assert.equal(processReads, 1);
});

it("writes text files, records timestamps, and restores process exit state", () => {
  const root = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-measure-write-"));
  const originalExitCode = process.exitCode;
  try {
    const filePath = NodePath.join(root, "nested", "output.txt");
    writeTextFile(filePath, "output");
    assert.equal(NodeFS.readFileSync(filePath, "utf8"), "output");
    assert.match(currentSampledAt(), /^\d{4}-\d{2}-\d{2}T/);
    setProcessExitCode(7);
    assert.equal(process.exitCode, 7);
  } finally {
    process.exitCode = originalExitCode;
    NodeFS.rmSync(root, { recursive: true, force: true });
  }
});

it("measures readiness, sampling, and cleanup through an injected host", async () => {
  let cleanupCalls = 0;
  const host = makeMeasurementHost({
    cleanup: async () => {
      cleanupCalls += 1;
    },
  });
  const summary = await measureDesktopRuntime(
    {
      label: "desktop",
      command: "app",
      args: [],
      readyUrl: "http://localhost:5733/ready",
      windowTitle: "Coddy Studio",
      idleMs: 5,
      timeoutMs: 100,
      env: {},
      keepRunning: false,
      verbose: true,
    },
    host,
  );
  assert.equal(summary.startupMs, 125);
  assert.equal(summary.windowReadyMs, 250);
  assert.equal(cleanupCalls, 1);

  const noGate = await measureDesktopRuntime(
    {
      label: "desktop",
      command: "app",
      args: [],
      idleMs: 5,
      timeoutMs: 100,
      env: {},
      keepRunning: true,
    },
    host,
  );
  assert.equal(noGate.startupMs, 0);
  assert.equal(cleanupCalls, 1);
});

it("writes CLI artifacts and reports main failures without mutating process state", async () => {
  const root = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-measure-cli-"));
  const stdout: string[] = [];
  const stderr: string[] = [];
  const host = makeMeasurementHost({
    stdout: (text) => stdout.push(text),
    stderr: (text) => stderr.push(text),
  });
  try {
    const jsonOut = NodePath.join(root, "out", "runtime.json");
    const markdownOut = NodePath.join(root, "out", "runtime.md");
    await runMeasureDesktopRuntimeCli(
      [
        "--label=desktop",
        "--command=app",
        "--idle-ms=1",
        "--timeout-ms=1",
        `--json-out=${jsonOut}`,
        `--markdown-out=${markdownOut}`,
      ],
      host,
    );
    assert.include(NodeFS.readFileSync(jsonOut, "utf8"), '"label": "desktop"');
    assert.include(NodeFS.readFileSync(markdownOut, "utf8"), "Desktop Runtime Measurement");
    assert.include(stdout.join(""), '"label": "desktop"');

    await runMeasureDesktopRuntimeCli(
      ["--label=plain", "--command=app", "--idle-ms=1", "--timeout-ms=1"],
      host,
    );

    const exitCodes: number[] = [];
    assert.equal(
      await runMeasureDesktopRuntimeMain(false, [], host, (code) => exitCodes.push(code)),
      false,
    );
    assert.equal(
      await runMeasureDesktopRuntimeMain(true, [], host, (code) => exitCodes.push(code)),
      true,
    );
    assert.deepStrictEqual(exitCodes, [1]);
    assert.include(stderr.join(""), "--label is required");

    assert.equal(
      await runMeasureDesktopRuntimeMain(
        true,
        ["--label=ok", "--command=app", "--idle-ms=1", "--timeout-ms=1"],
        host,
        (code) => exitCodes.push(code),
      ),
      true,
    );
    assert.deepStrictEqual(exitCodes, [1]);

    const stringFailureHost = makeMeasurementHost({
      spawn: async () => Promise.reject("string failure"),
      stderr: (text) => stderr.push(text),
    });
    await runMeasureDesktopRuntimeMain(
      true,
      ["--label=bad", "--command=app", "--idle-ms=1", "--timeout-ms=1"],
      stringFailureHost,
      (code) => exitCodes.push(code),
    );
    assert.include(stderr.join(""), "string failure");
  } finally {
    NodeFS.rmSync(root, { recursive: true, force: true });
  }
});
