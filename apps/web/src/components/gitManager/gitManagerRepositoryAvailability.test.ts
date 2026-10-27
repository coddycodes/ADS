// @effect-diagnostics nodeBuiltinImport:off - These tests verify path arguments with native shells and isolated Git configuration.
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import { VcsStatusResult } from "@coddy/contracts";
import { HostProcessEnvironment, HostProcessPlatform } from "@coddy/shared/hostProcess";
import * as Context from "effect/Context";
import * as Schema from "effect/Schema";
import { describe, expect, it } from "vite-plus/test";

import {
  gitManagerRepositoryUnavailableCopy,
  resolveGitManagerRepositoryUnavailable,
} from "./gitManagerRepositoryAvailability";

const decodeStatusResult = Schema.decodeUnknownSync(VcsStatusResult);

describe("repository availability from status", () => {
  it("keeps the loading and readable presentations", () => {
    expect(resolveGitManagerRepositoryUnavailable(null)).toBeNull();
    expect(resolveGitManagerRepositoryUnavailable({ isRepo: true })).toBeNull();
    expect(
      resolveGitManagerRepositoryUnavailable({
        isRepo: true,
        repositoryUnavailableReason: "absent",
      }),
    ).toBeNull();
  });

  it.each(["absent", "unreadable", "untrusted"] as const)(
    "preserves the unavailable reason %s",
    (reason) => {
      expect(
        resolveGitManagerRepositoryUnavailable({
          isRepo: false,
          repositoryUnavailableReason: reason,
        }),
      ).toBe(reason);
    },
  );

  it("keeps an older server's missing reason explicitly unavailable", () => {
    expect(resolveGitManagerRepositoryUnavailable({ isRepo: false })).toBe("unknown");
  });

  it("uses generic copy for an unknown reason decoded from a newer server", () => {
    const decoded = decodeStatusResult({
      isRepo: false,
      repositoryUnavailableReason: "futureReason",
      hasPrimaryRemote: false,
      isDefaultRef: false,
      refName: null,
      hasWorkingTreeChanges: false,
      workingTree: { files: [], insertions: 0, deletions: 0 },
      hasUpstream: false,
      aheadCount: 0,
      behindCount: 0,
      pr: null,
    });
    const reason = resolveGitManagerRepositoryUnavailable(decoded);

    expect(reason).toBe("unknown");
    if (reason === null) throw new Error("Expected an unavailable repository");
    expect(gitManagerRepositoryUnavailableCopy(reason, "/opaque/checkout")).toMatchObject({
      shortLabel: "Repository unavailable",
      message:
        "Git can't read this folder as a repository. Run git init to create one, or check its .git folder if it already is one.",
      command: "git init",
    });
  });
});

describe("repository unavailable copy", () => {
  it.each([
    ["absent", "Not a Git repository"],
    ["unreadable", "Repository unreadable"],
    ["untrusted", "Repository not trusted"],
    ["unknown", "Repository unavailable"],
  ] as const)("labels %s in compact rows", (reason, shortLabel) => {
    expect(gitManagerRepositoryUnavailableCopy(reason, "/opaque/checkout")).toMatchObject({
      shortLabel,
    });
  });

  it.each([
    ["absent", "This folder isn't a Git repository. Run git init to create one.", "git init"],
    [
      "unreadable",
      "Git can't read this repository. Check its .git folder, for example a damaged HEAD or config file.",
      null,
    ],
    [
      "untrusted",
      "Git doesn't trust this repository because another user owns it. Run git config --global --add safe.directory /opaque/checkout to trust it.",
      "git config --global --add safe.directory /opaque/checkout",
    ],
    [
      "unknown",
      "Git can't read this folder as a repository. Run git init to create one, or check its .git folder if it already is one.",
      "git init",
    ],
  ] as const)(
    "explains %s with the same words for panels and disabled reasons",
    (reason, message, command) => {
      expect(gitManagerRepositoryUnavailableCopy(reason, "/opaque/checkout")).toMatchObject({
        message,
        command,
      });
    },
  );

  it("uses the selected checkout's opaque path in the trust command", () => {
    expect(gitManagerRepositoryUnavailableCopy("untrusted", "D:\\Work\\project").message).toBe(
      "Git doesn't trust this repository because another user owns it. Run git config --global --add safe.directory D:/Work/project to trust it.",
    );
  });

  it.each([
    ["/opaque/checkout", "git config --global --add safe.directory /opaque/checkout"],
    ["/opaque/my checkout", "git config --global --add safe.directory '/opaque/my checkout'"],
    ["D:\\Work\\my project", "git config --global --add safe.directory 'D:/Work/my project'"],
  ])("quotes the trust path when needed: %s", (cwd, command) => {
    expect(gitManagerRepositoryUnavailableCopy("untrusted", cwd)).toMatchObject({
      command,
      message: `Git doesn't trust this repository because another user owns it. Run ${command} to trust it.`,
    });
  });

  const posixPaths = [
    ["/srv/repo", "/srv/repo"],
    ["/srv/my repo", "'/srv/my repo'"],
    ['/srv/a"b', "'/srv/a\"b'"],
    ["/srv/$HOME", "'/srv/$HOME'"],
    ["/srv/`printf expanded`", "'/srv/`printf expanded`'"],
    ["/srv/o'brien", "'/srv/o'\\''brien'"],
    ["/srv/~repo", "'/srv/~repo'"],
    ["/srv/back\\slash", "'/srv/back\\slash'"],
    ["/srv/_@%+=:,.+-", "/srv/_@%+=:,.+-"],
    [
      '/srv/my "repo"/$HOME/`printf expanded`/o\'brien/~back\\slash',
      "'/srv/my \"repo\"/$HOME/`printf expanded`/o'\\''brien/~back\\slash'",
    ],
  ] as const;

  it.each(posixPaths)("quotes a POSIX server path literally: %s", (cwd, quoted) => {
    const command = `git config --global --add safe.directory ${quoted}`;
    expect(gitManagerRepositoryUnavailableCopy("untrusted", cwd)).toMatchObject({
      command,
      message: `Git doesn't trust this repository because another user owns it. Run ${command} to trust it.`,
    });
  });

  it("round-trips POSIX server paths through sh without expansion", ({ skip }) => {
    if (Context.get(Context.empty(), HostProcessPlatform) === "win32") {
      skip("POSIX shell round-trip requires sh; skipped on Windows.");
    }

    for (const [cwd] of posixPaths) {
      const { command } = gitManagerRepositoryUnavailableCopy("untrusted", cwd);
      if (command === null) throw new Error("Expected a trust command");
      const quoted = command.slice("git config --global --add safe.directory ".length);
      try {
        expect(
          NodeChildProcess.execFileSync("sh", ["-c", `printf %s ${quoted}`], {
            encoding: "utf8",
            stdio: ["ignore", "pipe", "pipe"],
            timeout: 5000,
          }),
        ).toBe(cwd);
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code === "ENOENT") {
          skip("POSIX shell round-trip requires sh, which is not available.");
        }
        throw error;
      }
    }
  });

  it.each([
    ["C:\\Users\\me\\repo", "C:/Users/me/repo"],
    ["C:/repo-a", "C:/repo-a"],
    ["c:/repo_a.1", "c:/repo_a.1"],
    ["C:\\Users\\O'Brien\\my repo", "'C:/Users/O''Brien/my repo'"],
    ["C:\\repo\\$HOME", "'C:/repo/$HOME'"],
    ["C:\\repo\\a`b", "'C:/repo/a`b'"],
    ['C:\\repo\\a"b', "'C:/repo/a\"b'"],
    ["C:\\Users\\O‘Brien\\repo", "'C:/Users/O‘‘Brien/repo'"],
    ["C:\\Users\\O’Brien\\repo", "'C:/Users/O’’Brien/repo'"],
    ["C:\\Users\\O‚Brien\\repo", "'C:/Users/O‚‚Brien/repo'"],
    ["C:\\Users\\O‛Brien\\repo", "'C:/Users/O‛‛Brien/repo'"],
    ["C:\\repo\\_@%+=,~", "'C:/repo/_@%+=,~'"],
    ["\\\\server\\share\\repo", "'%(prefix)///server/share/repo'"],
  ])("quotes a Windows server path for PowerShell: %s", (cwd, quoted) => {
    const command = `git config --global --add safe.directory ${quoted}`;
    expect(gitManagerRepositoryUnavailableCopy("untrusted", cwd)).toMatchObject({
      command,
      message: `Git doesn't trust this repository because another user owns it. Run ${command} to trust it.`,
    });
  });

  it("round-trips drive and UNC trust commands through native Windows PowerShell and Git", ({
    skip,
  }) => {
    if (Context.get(Context.empty(), HostProcessPlatform) !== "win32") {
      skip(
        "Native Windows PowerShell and Git are required; other platforms are compatibility only.",
      );
    }
    const root = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-windows-trust-"));
    try {
      const env = Object.fromEntries(
        Object.entries(Context.get(Context.empty(), HostProcessEnvironment)).filter(
          ([key]) => !key.toUpperCase().startsWith("GIT_"),
        ),
      );
      Object.assign(env, {
        HOME: root,
        USERPROFILE: root,
        XDG_CONFIG_HOME: NodePath.join(root, "config"),
        GIT_CONFIG_GLOBAL: NodePath.join(root, "global.gitconfig"),
        GIT_CONFIG_SYSTEM: NodePath.join(root, "system.gitconfig"),
        GIT_CONFIG_NOSYSTEM: "1",
        GIT_TERMINAL_PROMPT: "0",
      });
      NodeFS.writeFileSync(NodePath.join(root, "system.gitconfig"), "");
      const cases = [
        { cwd: "C:\\work\\plain", expected: "C:/work/plain" },
        {
          cwd: "C:\\work\\space $literal `tick O'Brien",
          expected: "C:/work/space $literal `tick O'Brien",
        },
        { cwd: "C:\\work\\O’Brien\\repo", expected: "C:/work/O’Brien/repo" },
        {
          cwd: "\\\\server\\share name\\repo $literal `tick O’Brien",
          expected: "%(prefix)///server/share name/repo $literal `tick O’Brien",
        },
      ];
      const commands = cases.map(({ cwd }) => {
        const { command } = gitManagerRepositoryUnavailableCopy("untrusted", cwd);
        if (command === null) throw new Error("Expected a trust command");
        return `${command}\nif ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }`;
      });
      NodeChildProcess.execFileSync(
        "powershell.exe",
        [
          "-NoProfile",
          "-NonInteractive",
          "-EncodedCommand",
          Buffer.from(commands.join("\n"), "utf16le").toString("base64"),
        ],
        { cwd: root, env, timeout: 15_000, stdio: ["ignore", "pipe", "pipe"] },
      );
      const registered = NodeChildProcess.execFileSync(
        "git.exe",
        ["config", "--global", "--get-all", "safe.directory"],
        { cwd: root, env, encoding: "utf8", timeout: 10_000, stdio: ["ignore", "pipe", "pipe"] },
      )
        .trimEnd()
        .split(/\r?\n/);
      expect(registered).toEqual(cases.map(({ expected }) => expected));
    } finally {
      NodeFS.rmSync(root, { recursive: true, force: true });
    }
  }, 30_000);
});
