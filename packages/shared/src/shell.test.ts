// @effect-diagnostics nodeBuiltinImport:off
import * as NodeServices from "@effect/platform-node/NodeServices";
import { it as effectIt } from "@effect/vitest";
import { HostProcessEnvironment, HostProcessPlatform } from "@coddy/shared/hostProcess";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as Effect from "effect/Effect";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";

import {
  extractPathFromShellOutput,
  CommandAvailability,
  type CommandAvailabilityChecker,
  isCommandAvailable,
  listLoginShellCandidates,
  mergePathEntries,
  mergePathValues,
  readEnvironmentFromLoginShell,
  readEnvironmentFromWindowsShell,
  readPathFromLaunchctl,
  readPathFromLoginShell,
  resolveCommandPath,
  resolveKnownWindowsCliDirs,
  resolveSpawnCommand,
  resolveWindowsEnvironment,
  SpawnExecutableResolution,
  WindowsShellEnvironment,
  type WindowsShellEnvironmentReader,
} from "./shell.ts";

const temporaryDirectories: Array<string> = [];

const createTemporaryDirectory = (): string => {
  const directory = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-shell-"));
  temporaryDirectories.push(directory);
  return directory;
};

const createWorkspaceTemporaryDirectory = (): string => {
  const directory = NodeFS.mkdtempSync(NodePath.join(process.cwd(), ".coddy-studio-shell-"));
  temporaryDirectories.push(directory);
  return directory;
};

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0)) {
    const parent = NodePath.dirname(directory);
    const base = NodePath.basename(directory);
    const isSystemTemporaryDirectory =
      parent === NodePath.resolve(NodeOS.tmpdir()) && base.startsWith("coddy-studio-shell-");
    const isWorkspaceTemporaryDirectory =
      parent === process.cwd() && base.startsWith(".coddy-studio-shell-");
    if (!isSystemTemporaryDirectory && !isWorkspaceTemporaryDirectory) {
      throw new Error(`Refusing to remove unexpected test directory: ${directory}`);
    }
    NodeFS.rmSync(directory, { recursive: true, force: true });
  }
  vi.restoreAllMocks();
});

const withWindowsEnvironmentMocks = <A, E, R>(
  effect: Effect.Effect<A, E, R>,
  readEnvironment: WindowsShellEnvironmentReader,
  commandAvailable: CommandAvailabilityChecker,
) =>
  effect.pipe(
    Effect.provideService(WindowsShellEnvironment, readEnvironment),
    Effect.provideService(CommandAvailability, commandAvailable),
  );

describe("extractPathFromShellOutput", () => {
  it("extracts the path between capture markers", () => {
    expect(
      extractPathFromShellOutput(
        "__CODDY_STUDIO_PATH_START__\n/opt/homebrew/bin:/usr/bin\n__CODDY_STUDIO_PATH_END__\n",
      ),
    ).toBe("/opt/homebrew/bin:/usr/bin");
  });

  it("ignores shell startup noise around the capture markers", () => {
    expect(
      extractPathFromShellOutput(
        "Welcome to fish\n__CODDY_STUDIO_PATH_START__\n/opt/homebrew/bin:/usr/bin\n__CODDY_STUDIO_PATH_END__\nBye\n",
      ),
    ).toBe("/opt/homebrew/bin:/usr/bin");
  });

  it("returns null when the markers are missing", () => {
    expect(extractPathFromShellOutput("/opt/homebrew/bin /usr/bin")).toBeNull();
  });

  it("returns null for a missing end marker or an empty captured path", () => {
    expect(extractPathFromShellOutput("__CODDY_STUDIO_PATH_START__/usr/bin")).toBeNull();
    expect(
      extractPathFromShellOutput("__CODDY_STUDIO_PATH_START__\n \n__CODDY_STUDIO_PATH_END__"),
    ).toBeNull();
  });
});

describe("readPathFromLoginShell", () => {
  it("uses a shell-agnostic printenv PATH probe", () => {
    const execFile = vi.fn<
      (
        file: string,
        args: ReadonlyArray<string>,
        options: { encoding: "utf8"; timeout: number },
      ) => string
    >(() => "__CODDY_STUDIO_ENV_PATH_START__\n/a:/b\n__CODDY_STUDIO_ENV_PATH_END__\n");

    expect(readPathFromLoginShell("/opt/homebrew/bin/fish", execFile)).toBe("/a:/b");
    expect(execFile).toHaveBeenCalledTimes(1);

    const firstCall = execFile.mock.calls[0] as
      | [string, ReadonlyArray<string>, { encoding: "utf8"; timeout: number }]
      | undefined;
    expect(firstCall).toBeDefined();
    if (!firstCall) {
      throw new Error("Expected execFile to be called");
    }

    const [shell, args, options] = firstCall;
    expect(shell).toBe("/opt/homebrew/bin/fish");
    expect(args).toHaveLength(2);
    expect(args?.[0]).toBe("-ilc");
    expect(args?.[1]).toContain("printenv PATH || true");
    expect(args?.[1]).toContain("__CODDY_STUDIO_ENV_PATH_START__");
    expect(args?.[1]).toContain("__CODDY_STUDIO_ENV_PATH_END__");
    expect(options).toEqual({ encoding: "utf8", timeout: 5000 });
  });
});

describe("readPathFromLaunchctl", () => {
  it("returns a trimmed PATH value from launchctl", () => {
    const execFile = vi.fn<
      (
        file: string,
        args: ReadonlyArray<string>,
        options: { encoding: "utf8"; timeout: number },
      ) => string
    >(() => "  /opt/homebrew/bin:/usr/bin  \n");

    expect(readPathFromLaunchctl(execFile)).toBe("/opt/homebrew/bin:/usr/bin");
    expect(execFile).toHaveBeenCalledWith("/bin/launchctl", ["getenv", "PATH"], {
      encoding: "utf8",
      timeout: 2000,
    });
  });

  it("returns undefined when launchctl is unavailable", () => {
    const execFile = vi.fn<
      (
        file: string,
        args: ReadonlyArray<string>,
        options: { encoding: "utf8"; timeout: number },
      ) => string
    >(() => {
      throw new Error("spawn /bin/launchctl ENOENT");
    });

    expect(readPathFromLaunchctl(execFile)).toBeUndefined();
  });
});

describe("readEnvironmentFromLoginShell", () => {
  it("does not launch a shell when no variables are requested", () => {
    const execFile = vi.fn(() => "unexpected");
    expect(readEnvironmentFromLoginShell("/bin/sh", [], execFile)).toEqual({});
    expect(execFile).not.toHaveBeenCalled();
  });

  it("rejects variable names that cannot be safely embedded in a shell command", () => {
    const execFile = vi.fn(() => "unexpected");
    expect(() => readEnvironmentFromLoginShell("/bin/sh", ["PATH; rm"], execFile)).toThrow(
      "Unsupported environment variable name",
    );
    expect(execFile).not.toHaveBeenCalled();
  });

  it("ignores captures with missing markers", () => {
    const execFile = vi.fn(() =>
      ["__CODDY_STUDIO_ENV_PATH_START__", "/usr/bin", "__CODDY_STUDIO_ENV_OTHER_END__"].join("\n"),
    );
    expect(readEnvironmentFromLoginShell("/bin/sh", ["PATH", "OTHER"], execFile)).toEqual({});
  });

  it("extracts multiple environment variables from a login shell command", () => {
    const execFile = vi.fn<
      (
        file: string,
        args: ReadonlyArray<string>,
        options: { encoding: "utf8"; timeout: number },
      ) => string
    >(() =>
      [
        "__CODDY_STUDIO_ENV_PATH_START__",
        "/a:/b",
        "__CODDY_STUDIO_ENV_PATH_END__",
        "__CODDY_STUDIO_ENV_SSH_AUTH_SOCK_START__",
        "/tmp/secretive.sock",
        "__CODDY_STUDIO_ENV_SSH_AUTH_SOCK_END__",
      ].join("\n"),
    );

    expect(readEnvironmentFromLoginShell("/bin/zsh", ["PATH", "SSH_AUTH_SOCK"], execFile)).toEqual({
      PATH: "/a:/b",
      SSH_AUTH_SOCK: "/tmp/secretive.sock",
    });
    expect(execFile).toHaveBeenCalledTimes(1);
  });

  it("omits environment variables that are missing or empty", () => {
    const execFile = vi.fn<
      (
        file: string,
        args: ReadonlyArray<string>,
        options: { encoding: "utf8"; timeout: number },
      ) => string
    >(() =>
      [
        "__CODDY_STUDIO_ENV_PATH_START__",
        "/a:/b",
        "__CODDY_STUDIO_ENV_PATH_END__",
        "__CODDY_STUDIO_ENV_SSH_AUTH_SOCK_START__",
        "__CODDY_STUDIO_ENV_SSH_AUTH_SOCK_END__",
      ].join("\n"),
    );

    expect(readEnvironmentFromLoginShell("/bin/zsh", ["PATH", "SSH_AUTH_SOCK"], execFile)).toEqual({
      PATH: "/a:/b",
    });
  });

  it("preserves surrounding whitespace in captured values", () => {
    const execFile = vi.fn<
      (
        file: string,
        args: ReadonlyArray<string>,
        options: { encoding: "utf8"; timeout: number },
      ) => string
    >(() =>
      [
        "__CODDY_STUDIO_ENV_CUSTOM_VAR_START__",
        "  padded value  ",
        "__CODDY_STUDIO_ENV_CUSTOM_VAR_END__",
      ].join("\n"),
    );

    expect(readEnvironmentFromLoginShell("/bin/zsh", ["CUSTOM_VAR"], execFile)).toEqual({
      CUSTOM_VAR: "  padded value  ",
    });
  });
});

describe("listLoginShellCandidates", () => {
  it("returns env shell, user shell, then the platform fallback without duplicates", () => {
    expect(listLoginShellCandidates("darwin", " /opt/homebrew/bin/nu ", "/bin/zsh")).toEqual([
      "/opt/homebrew/bin/nu",
      "/bin/zsh",
    ]);
  });

  it("falls back to the platform default when no shells are available", () => {
    expect(listLoginShellCandidates("linux", undefined, "")).toEqual(["/bin/bash"]);
  });

  it("has no platform fallback on Windows and removes repeated candidates", () => {
    expect(listLoginShellCandidates("win32", " pwsh.exe ", "pwsh.exe")).toEqual(["pwsh.exe"]);
    expect(listLoginShellCandidates("win32", " ", " ")).toEqual([]);
  });

  it("reads the current user shell when the caller omits it", () => {
    const candidates = listLoginShellCandidates("darwin", undefined);
    expect(candidates.at(-1)).toBe("/bin/zsh");
  });

  it("falls back when the OS user lookup fails", async () => {
    vi.resetModules();
    vi.doMock("node:os", () => ({
      ...NodeOS,
      userInfo: () => {
        throw new Error("user lookup failed");
      },
    }));
    const isolatedShell = await import("./shell.ts");
    expect(isolatedShell.listLoginShellCandidates("linux", undefined)).toEqual(["/bin/bash"]);
    vi.doUnmock("node:os");
  });
});

describe("mergePathEntries", () => {
  it("prefers login-shell PATH entries and keeps inherited extras", () => {
    expect(
      mergePathEntries("/opt/homebrew/bin:/usr/bin", "/Users/test/.local/bin:/usr/bin", "darwin"),
    ).toBe("/opt/homebrew/bin:/usr/bin:/Users/test/.local/bin");
  });

  it("uses the platform-specific delimiter", () => {
    expect(mergePathEntries("C:\\Tools;C:\\Windows", "C:\\Windows;C:\\Git", "win32")).toBe(
      "C:\\Tools;C:\\Windows;C:\\Git",
    );
  });

  it("ignores missing, blank, and duplicate entries", () => {
    expect(mergePathEntries(undefined, undefined, "linux")).toBeUndefined();
    expect(mergePathEntries(" :/usr/bin:: ", " /usr/bin:/opt/bin ", "linux")).toBe(
      "/usr/bin:/opt/bin",
    );
  });
});

describe("readEnvironmentFromWindowsShell", () => {
  it("does not launch PowerShell when no variables are requested", () => {
    const execFile = vi.fn(() => "unexpected");
    expect(readEnvironmentFromWindowsShell([], execFile)).toEqual({});
    expect(execFile).not.toHaveBeenCalled();
  });

  it("rejects variable names that cannot be safely embedded in PowerShell", () => {
    const execFile = vi.fn(() => "unexpected");
    expect(() => readEnvironmentFromWindowsShell(["PATH;Write-Host"], execFile)).toThrow(
      "Unsupported environment variable name",
    );
    expect(execFile).not.toHaveBeenCalled();
  });

  it("supports an omitted options object with an explicit executor", () => {
    const execFile = vi.fn(() => "__CODDY_STUDIO_ENV_PATH_START__\nC:\\Tools\n__CODDY_STUDIO_ENV_PATH_END__");
    expect(readEnvironmentFromWindowsShell(["PATH"], undefined, execFile)).toEqual({
      PATH: "C:\\Tools",
    });
  });

  it("extracts environment variables from a PowerShell command", () => {
    const execFile = vi.fn<
      (
        file: string,
        args: ReadonlyArray<string>,
        options: { encoding: "utf8"; timeout: number },
      ) => string
    >(
      () =>
        "__CODDY_STUDIO_ENV_PATH_START__\nC:\\Users\\testuser\\AppData\\Roaming\\npm\n__CODDY_STUDIO_ENV_PATH_END__\n",
    );

    expect(readEnvironmentFromWindowsShell(["PATH"], execFile)).toEqual({
      PATH: "C:\\Users\\testuser\\AppData\\Roaming\\npm",
    });
    expect(execFile).toHaveBeenCalledWith(
      "pwsh.exe",
      expect.arrayContaining(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]),
      { encoding: "utf8", timeout: 5000 },
    );
  });

  it("strips CRLF delimiters from captured PowerShell values", () => {
    const execFile = vi.fn<
      (
        file: string,
        args: ReadonlyArray<string>,
        options: { encoding: "utf8"; timeout: number },
      ) => string
    >(
      () =>
        "__CODDY_STUDIO_ENV_FNM_DIR_START__\r\nC:\\Users\\testuser\\AppData\\Roaming\\fnm\r\n__CODDY_STUDIO_ENV_FNM_DIR_END__\r\n",
    );

    expect(readEnvironmentFromWindowsShell(["FNM_DIR"], execFile)).toEqual({
      FNM_DIR: "C:\\Users\\testuser\\AppData\\Roaming\\fnm",
    });
  });

  it("omits requested variables that are absent from successful PowerShell output", () => {
    const execFile = vi.fn(
      () => "__CODDY_STUDIO_ENV_PATH_START__\nC:\\Tools\n__CODDY_STUDIO_ENV_PATH_END__\n",
    );
    expect(readEnvironmentFromWindowsShell(["PATH", "FNM_DIR"], execFile)).toEqual({
      PATH: "C:\\Tools",
    });
  });

  it("omits -NoProfile when loadProfile is enabled", () => {
    const execFile = vi.fn<
      (
        file: string,
        args: ReadonlyArray<string>,
        options: { encoding: "utf8"; timeout: number },
      ) => string
    >(() => "__CODDY_STUDIO_ENV_PATH_START__\nC:\\Tools\n__CODDY_STUDIO_ENV_PATH_END__\n");

    expect(readEnvironmentFromWindowsShell(["PATH"], { loadProfile: true }, execFile)).toEqual({
      PATH: "C:\\Tools",
    });
    expect(execFile).toHaveBeenCalledWith(
      "pwsh.exe",
      expect.arrayContaining(["-NoLogo", "-NonInteractive", "-Command"]),
      { encoding: "utf8", timeout: 5000 },
    );
    expect(execFile.mock.calls[0]?.[1]).not.toContain("-NoProfile");
  });

  it("falls back to Windows PowerShell when pwsh.exe is unavailable", () => {
    const execFile = vi.fn<
      (
        file: string,
        args: ReadonlyArray<string>,
        options: { encoding: "utf8"; timeout: number },
      ) => string
    >((file) => {
      if (file === "pwsh.exe") {
        throw new Error("spawn pwsh.exe ENOENT");
      }
      return "__CODDY_STUDIO_ENV_PATH_START__\nC:\\Tools\n__CODDY_STUDIO_ENV_PATH_END__\n";
    });

    expect(readEnvironmentFromWindowsShell(["PATH"], execFile)).toEqual({
      PATH: "C:\\Tools",
    });
    expect(execFile).toHaveBeenNthCalledWith(1, "pwsh.exe", expect.any(Array), {
      encoding: "utf8",
      timeout: 5000,
    });
    expect(execFile).toHaveBeenNthCalledWith(2, "powershell.exe", expect.any(Array), {
      encoding: "utf8",
      timeout: 5000,
    });
  });

  it("returns an empty environment when both PowerShell executables fail", () => {
    const execFile = vi.fn(() => {
      throw new Error("shell unavailable");
    });
    expect(readEnvironmentFromWindowsShell(["PATH"], execFile)).toEqual({});
    expect(execFile).toHaveBeenCalledTimes(2);
  });
});

describe("mergePathValues", () => {
  it("dedupes case-insensitively on Windows while preserving preferred order", () => {
    expect(
      mergePathValues(
        'C:\\Users\\testuser\\AppData\\Roaming\\npm;"C:\\Program Files\\nodejs"',
        "c:\\users\\testuser\\appdata\\roaming\\npm;C:\\Windows\\System32",
        "win32",
      ),
    ).toBe(
      'C:\\Users\\testuser\\AppData\\Roaming\\npm;"C:\\Program Files\\nodejs";C:\\Windows\\System32',
    );
  });

  it("dedupes case-sensitively on POSIX", () => {
    expect(mergePathValues("/usr/local/bin:/usr/bin", "/usr/bin:/USR/BIN", "linux")).toBe(
      "/usr/local/bin:/usr/bin:/USR/BIN",
    );
  });

  it("returns undefined for missing or quote-only paths and skips empty entries", () => {
    expect(mergePathValues(undefined, undefined, "darwin")).toBeUndefined();
    expect(mergePathValues('"";;', " ;C:\\Tools; ", "win32")).toBe("C:\\Tools");
  });
});

describe("resolveKnownWindowsCliDirs", () => {
  it("returns known Windows CLI install directories in priority order", () => {
    expect(
      resolveKnownWindowsCliDirs({
        APPDATA: "C:\\Users\\testuser\\AppData\\Roaming",
        LOCALAPPDATA: "C:\\Users\\testuser\\AppData\\Local",
        USERPROFILE: "C:\\Users\\testuser",
      }),
    ).toEqual([
      "C:\\Users\\testuser\\AppData\\Roaming\\npm",
      "C:\\Users\\testuser\\AppData\\Local\\Programs\\nodejs",
      "C:\\Users\\testuser\\AppData\\Local\\Volta\\bin",
      "C:\\Users\\testuser\\AppData\\Local\\pnpm",
      "C:\\Users\\testuser\\.bun\\bin",
      "C:\\Users\\testuser\\scoop\\shims",
    ]);
  });

  it("omits absent and whitespace-only environment roots", () => {
    expect(resolveKnownWindowsCliDirs({ APPDATA: " ", USERPROFILE: "C:\\Users\\dev" })).toEqual([
      "C:\\Users\\dev\\.bun\\bin",
      "C:\\Users\\dev\\scoop\\shims",
    ]);
    expect(resolveKnownWindowsCliDirs({})).toEqual([]);
  });
});

effectIt.layer(NodeServices.layer)("isCommandAvailable", (it) => {
  it.effect("returns false when PATH is empty", () =>
    Effect.gen(function* () {
      expect(
        yield* isCommandAvailable("definitely-not-installed", {
          env: { PATH: "", PATHEXT: ".COM;.EXE;.BAT;.CMD" },
        }).pipe(Effect.provideService(HostProcessPlatform, "win32")),
      ).toBe(false);
    }),
  );
});

effectIt.layer(NodeServices.layer)("resolveCommandPath", (it) => {
  it.effect("fails when PATH is empty", () =>
    Effect.gen(function* () {
      const result = yield* resolveCommandPath("definitely-not-installed", {
        env: { PATH: "", PATHEXT: ".COM;.EXE;.BAT;.CMD" },
      }).pipe(Effect.provideService(HostProcessPlatform, "win32"), Effect.result);

      expect(result._tag).toBe("Failure");
    }),
  );
});

effectIt.layer(NodeServices.layer)("resolveSpawnCommand", (it) => {
  it.effect("runs Windows executables directly without a shell", () =>
    Effect.gen(function* () {
      const command = yield* resolveSpawnCommand("node.exe", ["script.js", "hello & goodbye"], {
        env: { PATH: "", PATHEXT: ".COM;.EXE;.BAT;.CMD" },
      }).pipe(Effect.provideService(HostProcessPlatform, "win32"));

      expect(command).toEqual({
        command: "node.exe",
        args: ["script.js", "hello & goodbye"],
        shell: false,
      });
    }),
  );

  it.effect("invokes Windows command shims through cmd.exe without shell mode", () =>
    Effect.gen(function* () {
      const command = yield* resolveSpawnCommand(
        "vp",
        ["run", "value & calc", "%PATH%", "caret^value", 'quote"value'],
        { env: { PATH: "", PATHEXT: ".COM;.EXE;.BAT;.CMD" } },
      ).pipe(
        Effect.provideService(HostProcessPlatform, "win32"),
        Effect.provideService(
          SpawnExecutableResolution,
          () => "C:\\Program Files\\npm & tools\\vp.cmd",
        ),
      );

      expect(command.shell).toBe(false);
      expect(command.command).toBe("cmd.exe");
      expect(command.args).toEqual([
        "/d",
        "/s",
        "/v:off",
        "/c",
        "call",
        "C:\\Program Files\\npm & tools\\vp.cmd",
        "run",
        "value & calc",
        "^^%PATH^^%",
        "caret^^^^value",
        'quote"value',
      ]);
    }),
  );

  it.effect("resolves against the effective environment when extending host env", () =>
    Effect.gen(function* () {
      let resolvedEnvironment: NodeJS.ProcessEnv | undefined;
      yield* resolveSpawnCommand("codex", ["app-server"], {
        env: { CODEX_HOME: "C:\\Users\\tester\\.codex" },
        extendEnv: true,
      }).pipe(
        Effect.provideService(HostProcessPlatform, "win32"),
        Effect.provideService(HostProcessEnvironment, {
          PATH: "C:\\Users\\tester\\AppData\\Roaming\\npm",
          PATHEXT: ".COM;.EXE;.BAT;.CMD",
        }),
        Effect.provideService(SpawnExecutableResolution, (_command, _platform, env) => {
          resolvedEnvironment = env;
          return "C:\\Users\\tester\\AppData\\Roaming\\npm\\codex.cmd";
        }),
      );

      expect(resolvedEnvironment).toEqual({
        PATH: "C:\\Users\\tester\\AppData\\Roaming\\npm",
        PATHEXT: ".COM;.EXE;.BAT;.CMD",
        CODEX_HOME: "C:\\Users\\tester\\.codex",
      });
    }),
  );

  it.effect("does not fall back to a shell for unresolved Windows commands", () =>
    Effect.gen(function* () {
      const command = yield* resolveSpawnCommand("missing & calc", ["unsafe & value"], {
        env: { PATH: "", PATHEXT: ".COM;.EXE;.BAT;.CMD" },
      }).pipe(Effect.provideService(HostProcessPlatform, "win32"));

      expect(command).toEqual({
        command: "missing & calc",
        args: ["unsafe & value"],
        shell: false,
      });
    }),
  );
});

effectIt.layer(NodeServices.layer)("resolveWindowsEnvironment", (it) => {
  it.effect("returns the baseline no-profile PATH patch when node is already available", () =>
    Effect.gen(function* () {
      const readEnvironment = vi.fn(
        (_names: ReadonlyArray<string>, options?: { loadProfile?: boolean }) =>
          options?.loadProfile
            ? { PATH: "C:\\Profile\\Bin" }
            : { PATH: "C:\\Shell\\Bin;C:\\Windows\\System32" },
      );
      const commandAvailable = vi.fn(() => Effect.succeed(true));

      expect(
        yield* withWindowsEnvironmentMocks(
          resolveWindowsEnvironment({
            PATH: "C:\\Windows\\System32",
            APPDATA: "C:\\Users\\testuser\\AppData\\Roaming",
            LOCALAPPDATA: "C:\\Users\\testuser\\AppData\\Local",
            USERPROFILE: "C:\\Users\\testuser",
          }),
          readEnvironment,
          commandAvailable,
        ),
      ).toEqual({
        PATH: [
          "C:\\Users\\testuser\\AppData\\Roaming\\npm",
          "C:\\Users\\testuser\\AppData\\Local\\Programs\\nodejs",
          "C:\\Users\\testuser\\AppData\\Local\\Volta\\bin",
          "C:\\Users\\testuser\\AppData\\Local\\pnpm",
          "C:\\Users\\testuser\\.bun\\bin",
          "C:\\Users\\testuser\\scoop\\shims",
          "C:\\Shell\\Bin",
          "C:\\Windows\\System32",
        ].join(";"),
      });
      expect(readEnvironment).toHaveBeenCalledTimes(1);
      expect(readEnvironment).toHaveBeenCalledWith(["PATH"], { loadProfile: false });
      expect(commandAvailable).toHaveBeenCalledWith(
        "node",
        expect.objectContaining({ env: expect.any(Object) }),
      );
    }),
  );

  it.effect("loads the PowerShell profile when baseline env cannot resolve node", () =>
    Effect.gen(function* () {
      const readEnvironment = vi.fn(
        (_names: ReadonlyArray<string>, options?: { loadProfile?: boolean }) =>
          options?.loadProfile
            ? {
                PATH: "C:\\Profile\\Node;C:\\Windows\\System32",
                FNM_DIR: "C:\\Users\\testuser\\AppData\\Roaming\\fnm",
                FNM_MULTISHELL_PATH: "C:\\Users\\testuser\\AppData\\Local\\fnm_multishells\\123",
              }
            : { PATH: "C:\\Shell\\Bin;C:\\Windows\\System32" },
      );
      const commandAvailable = vi.fn(() => Effect.succeed(false));

      expect(
        yield* withWindowsEnvironmentMocks(
          resolveWindowsEnvironment({
            PATH: "C:\\Windows\\System32",
            APPDATA: "C:\\Users\\testuser\\AppData\\Roaming",
            LOCALAPPDATA: "C:\\Users\\testuser\\AppData\\Local",
            USERPROFILE: "C:\\Users\\testuser",
          }),
          readEnvironment,
          commandAvailable,
        ),
      ).toEqual({
        PATH: [
          "C:\\Profile\\Node",
          "C:\\Windows\\System32",
          "C:\\Users\\testuser\\AppData\\Roaming\\npm",
          "C:\\Users\\testuser\\AppData\\Local\\Programs\\nodejs",
          "C:\\Users\\testuser\\AppData\\Local\\Volta\\bin",
          "C:\\Users\\testuser\\AppData\\Local\\pnpm",
          "C:\\Users\\testuser\\.bun\\bin",
          "C:\\Users\\testuser\\scoop\\shims",
          "C:\\Shell\\Bin",
        ].join(";"),
        FNM_DIR: "C:\\Users\\testuser\\AppData\\Roaming\\fnm",
        FNM_MULTISHELL_PATH: "C:\\Users\\testuser\\AppData\\Local\\fnm_multishells\\123",
      });
      expect(readEnvironment).toHaveBeenNthCalledWith(1, ["PATH"], { loadProfile: false });
      expect(readEnvironment).toHaveBeenNthCalledWith(
        2,
        ["PATH", "FNM_DIR", "FNM_MULTISHELL_PATH"],
        {
          loadProfile: true,
        },
      );
      expect(commandAvailable).toHaveBeenCalledTimes(1);
    }),
  );

  it.effect("keeps the baseline env when profiled probe still does not resolve node", () =>
    Effect.gen(function* () {
      const readEnvironment = vi.fn(
        (_names: ReadonlyArray<string>, options?: { loadProfile?: boolean }) =>
          options?.loadProfile ? { FNM_DIR: "C:\\Users\\testuser\\AppData\\Roaming\\fnm" } : {},
      );
      const commandAvailable = vi.fn(() => Effect.succeed(false));

      expect(
        yield* withWindowsEnvironmentMocks(
          resolveWindowsEnvironment({
            PATH: "C:\\Windows\\System32",
            APPDATA: "C:\\Users\\testuser\\AppData\\Roaming",
            USERPROFILE: "C:\\Users\\testuser",
          }),
          readEnvironment,
          commandAvailable,
        ),
      ).toEqual({
        PATH: [
          "C:\\Users\\testuser\\AppData\\Roaming\\npm",
          "C:\\Users\\testuser\\.bun\\bin",
          "C:\\Users\\testuser\\scoop\\shims",
          "C:\\Windows\\System32",
        ].join(";"),
        FNM_DIR: "C:\\Users\\testuser\\AppData\\Roaming\\fnm",
      });
      expect(commandAvailable).toHaveBeenCalledTimes(1);
    }),
  );
});

effectIt.layer(NodeServices.layer)("shell service defaults", (it) => {
  it.effect("provides the default environment readers and command checker", () =>
    Effect.gen(function* () {
      expect(yield* WindowsShellEnvironment).toBe(readEnvironmentFromWindowsShell);
      expect(yield* CommandAvailability).toBe(isCommandAvailable);
      expect(typeof (yield* SpawnExecutableResolution)).toBe("function");
    }),
  );
});

effectIt.layer(NodeServices.layer)("default spawn executable resolution", (it) => {
  it.effect("resolves Windows PATH, PATHEXT, quoting, explicit paths, and non-files", () =>
    Effect.gen(function* () {
      if ((yield* HostProcessPlatform) !== "win32") return;

      const directory = createTemporaryDirectory();
      const commandPath = NodePath.join(directory, "unicode tool.CMD");
      const directoryPath = NodePath.join(directory, "folder.CMD");
      NodeFS.writeFileSync(commandPath, "");
      NodeFS.mkdirSync(directoryPath);

      const resolveExecutable = yield* SpawnExecutableResolution;
      const environment = {
        Path: ` ;"${directory}"; `,
        PATHEXT: "cmd; .EXE; ;CMD",
      };

      expect(resolveExecutable("unicode tool", "win32", environment)).toBe(commandPath);
      expect(resolveExecutable(commandPath, "win32", environment)).toBe(commandPath);
      expect(resolveExecutable(directoryPath, "win32", environment)).toBeUndefined();
      expect(
        resolveExecutable("missing", "win32", { path: directory, PATHEXT: " ; " }),
      ).toBeUndefined();
    }),
  );

  it.effect("resolves POSIX executables from direct and quoted PATH values", () =>
    Effect.gen(function* () {
      const directory = createWorkspaceTemporaryDirectory();
      const commandPath = NodePath.join(directory, "tool");
      NodeFS.writeFileSync(commandPath, "#!/bin/sh\n");
      NodeFS.chmodSync(commandPath, 0o755);
      const posixCommand = commandPath.replaceAll("\\", "/");
      const relativePosixDirectory = NodePath.relative(process.cwd(), directory).replaceAll(
        "\\",
        "/",
      );

      const resolveExecutable = yield* SpawnExecutableResolution;
      expect(resolveExecutable(posixCommand, "linux", {})).toBe(posixCommand);
      expect(resolveExecutable("tool", "darwin", { PATH: ` :"${relativePosixDirectory}"` })).toBe(
        `${relativePosixDirectory}/tool`,
      );
      expect(resolveExecutable(`${relativePosixDirectory}/missing`, "linux", {})).toBeUndefined();
    }),
  );
});

effectIt.layer(NodeServices.layer)("default POSIX executable access", (it) => {
  it.effect("treats an access error as a non-executable file", () =>
    Effect.gen(function* () {
      vi.resetModules();
      vi.doMock("node:fs", () => ({
        ...NodeFS,
        statSync: () => ({ isFile: () => true }),
        accessSync: () => {
          throw new Error("permission denied");
        },
      }));
      const isolatedShell = yield* Effect.promise(() => import("./shell.ts"));
      const resolveExecutable = yield* isolatedShell.SpawnExecutableResolution;
      expect(resolveExecutable("/virtual/tool", "linux", {})).toBeUndefined();
      vi.doUnmock("node:fs");
    }),
  );
});

describe("default Windows environment executor", () => {
  it("uses the Node child-process boundary when no executor is supplied", async () => {
    const execFileSync = vi.fn(
      () => "__CODDY_STUDIO_ENV_PATH_START__\nC:\\Default\n__CODDY_STUDIO_ENV_PATH_END__\n",
    );
    vi.resetModules();
    vi.doMock("node:child_process", async () => ({
      ...(await vi.importActual<typeof import("node:child_process")>("node:child_process")),
      execFileSync,
    }));
    const isolatedShell = await import("./shell.ts");

    expect(isolatedShell.readEnvironmentFromWindowsShell(["PATH"])).toEqual({
      PATH: "C:\\Default",
    });
    expect(execFileSync).toHaveBeenCalledTimes(1);
    vi.doUnmock("node:child_process");
  });
});

effectIt.layer(NodeServices.layer)("resolveCommandPath platform behavior", (it) => {
  it.effect("resolves Windows commands from PATH and explicit paths", () =>
    Effect.gen(function* () {
      const directory = createTemporaryDirectory();
      const commandPath = NodePath.join(directory, "tool.CMD");
      NodeFS.writeFileSync(commandPath, "");

      expect(
        yield* resolveCommandPath("tool", {
          env: { Path: ` ;"${directory}"`, PATHEXT: "CMD" },
        }).pipe(Effect.provideService(HostProcessPlatform, "win32")),
      ).toBe(commandPath);
      expect(
        yield* resolveCommandPath(commandPath, { env: { PATHEXT: ".CMD" } }).pipe(
          Effect.provideService(HostProcessPlatform, "win32"),
        ),
      ).toBe(commandPath);
    }),
  );

  it.effect("reports missing Windows paths, directories, and commands", () =>
    Effect.gen(function* () {
      const directory = createTemporaryDirectory();
      const directoryCandidate = NodePath.join(directory, "folder.CMD");
      NodeFS.mkdirSync(directoryCandidate);

      for (const [command, env] of [
        [directoryCandidate, { PATHEXT: ".CMD" }],
        ["missing", { path: directory, PATHEXT: ".CMD" }],
      ] as const) {
        const result = yield* resolveCommandPath(command, { env }).pipe(
          Effect.provideService(HostProcessPlatform, "win32"),
          Effect.result,
        );
        expect(result._tag).toBe("Failure");
      }
    }),
  );

  it.effect("uses the injected host environment when options omit env", () =>
    Effect.gen(function* () {
      const result = yield* resolveCommandPath("missing").pipe(
        Effect.provideService(HostProcessPlatform, "win32"),
        Effect.provideService(HostProcessEnvironment, { PATH: "", PATHEXT: ".CMD" }),
        Effect.result,
      );
      expect(result._tag).toBe("Failure");
    }),
  );

  it.effect("reports a missing command when no PATH spelling exists", () =>
    Effect.gen(function* () {
      const result = yield* resolveCommandPath("missing", { env: {} }).pipe(
        Effect.provideService(HostProcessPlatform, "linux"),
        Effect.result,
      );
      expect(result._tag).toBe("Failure");
    }),
  );

  it.effect("resolves a direct POSIX executable", () =>
    Effect.gen(function* () {
      const directory = createTemporaryDirectory();
      const commandPath = NodePath.join(directory, "tool");
      NodeFS.writeFileSync(commandPath, "#!/bin/sh\n");
      NodeFS.chmodSync(commandPath, 0o755);

      expect(
        yield* resolveCommandPath(commandPath, { env: {} }).pipe(
          Effect.provideService(HostProcessPlatform, "linux"),
        ),
      ).toBe(commandPath);
    }),
  );
});

effectIt.layer(NodeServices.layer)("resolveSpawnCommand platform behavior", (it) => {
  it.effect("passes POSIX commands and arguments through without a shell", () =>
    Effect.gen(function* () {
      const args = ["--label", "space and ünicode"];
      const resolved = yield* resolveSpawnCommand("/usr/bin/tool", args).pipe(
        Effect.provideService(HostProcessPlatform, "darwin"),
      );
      expect(resolved).toEqual({ command: "/usr/bin/tool", args, shell: false });
      expect(resolved.args).not.toBe(args);
    }),
  );

  it.effect("honors ComSpec and COMSPEC for Windows batch files", () =>
    Effect.gen(function* () {
      const withMixedCase = yield* resolveSpawnCommand("tool.bat", [], {
        env: { ComSpec: "C:\\Windows\\custom-cmd.exe" },
      }).pipe(Effect.provideService(HostProcessPlatform, "win32"));
      const withUpperCase = yield* resolveSpawnCommand("tool.cmd", [], {
        env: { COMSPEC: "C:\\Windows\\legacy-cmd.exe" },
      }).pipe(Effect.provideService(HostProcessPlatform, "win32"));

      expect(withMixedCase.command).toBe("C:\\Windows\\custom-cmd.exe");
      expect(withUpperCase.command).toBe("C:\\Windows\\legacy-cmd.exe");
    }),
  );

  it.effect("uses the host environment when no command environment is supplied", () =>
    Effect.gen(function* () {
      let receivedEnvironment: NodeJS.ProcessEnv | undefined;
      yield* resolveSpawnCommand("tool.exe", []).pipe(
        Effect.provideService(HostProcessPlatform, "win32"),
        Effect.provideService(HostProcessEnvironment, { PATH: "C:\\Host" }),
        Effect.provideService(SpawnExecutableResolution, (_command, _platform, env) => {
          receivedEnvironment = env;
          return "C:\\Host\\tool.exe";
        }),
      );
      expect(receivedEnvironment).toEqual({ PATH: "C:\\Host" });
    }),
  );
});

effectIt.layer(NodeServices.layer)("resolveWindowsEnvironment failure handling", (it) => {
  it.effect("returns an empty patch when both isolated environment probes fail", () =>
    Effect.gen(function* () {
      const readEnvironment = vi.fn(() => {
        throw new Error("PowerShell failed");
      });
      const commandAvailable = vi.fn(() => Effect.succeed(false));

      expect(
        yield* withWindowsEnvironmentMocks(
          resolveWindowsEnvironment({}),
          readEnvironment,
          commandAvailable,
        ),
      ).toEqual({});
      expect(readEnvironment).toHaveBeenCalledTimes(2);
      expect(commandAvailable).toHaveBeenCalledWith("node", { env: {} });
    }),
  );
});
