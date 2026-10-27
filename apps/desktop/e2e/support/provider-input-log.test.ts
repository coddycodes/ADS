// @effect-diagnostics nodeBuiltinImport:off - Desktop UI fixture tests inspect host temp files.
// @effect-diagnostics globalTimers:off - The fixture test schedules a deterministic delayed append.
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";

import { afterEach, describe, expect, it } from "vite-plus/test";

import {
  appendProviderInputLogEntry,
  readProviderInputLog,
  waitForProviderInputLogEntry,
  type ProviderInputLogEntry,
} from "./provider-input-log.ts";

const temporaryDirectories: string[] = [];

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0)) {
    NodeFS.rmSync(directory, { recursive: true, force: true });
  }
});

describe("provider input log", () => {
  it("appends one JSON object per line and preserves provider input order", () => {
    const directory = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-provider-log-"));
    temporaryDirectories.push(directory);
    const path = NodePath.join(directory, "nested", "provider-input.jsonl");
    const entries: ProviderInputLogEntry[] = [
      {
        provider: "codex",
        kind: "start",
        prompt: "$refactor",
        recordedAt: "2026-07-23T12:00:00.000Z",
      },
      {
        provider: "opencode",
        kind: "start",
        prompt: "@reviewer",
        recordedAt: "2026-07-23T12:00:01.000Z",
      },
    ];

    for (const entry of entries) {
      appendProviderInputLogEntry(path, entry);
    }

    expect(NodeFS.readFileSync(path, "utf8")).toBe(
      `${entries.map((entry) => JSON.stringify(entry)).join("\n")}\n`,
    );
    expect(readProviderInputLog(path)).toEqual(entries);
  });

  it("returns an empty inventory before a provider records input", () => {
    const directory = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-provider-log-"));
    temporaryDirectories.push(directory);

    expect(readProviderInputLog(NodePath.join(directory, "missing.jsonl"))).toEqual([]);
  });

  it("waits for one exact appended provider entry", async () => {
    const directory = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-provider-log-"));
    temporaryDirectories.push(directory);
    const path = NodePath.join(directory, "provider-input.jsonl");
    const entry: ProviderInputLogEntry = {
      provider: "cursor",
      kind: "start",
      prompt: "/review",
      recordedAt: "2026-07-23T12:00:00.000Z",
    };
    setTimeout(() => appendProviderInputLogEntry(path, entry), 5);

    await expect(
      waitForProviderInputLogEntry(
        path,
        0,
        { provider: "cursor", prompt: "/review" },
        {
          timeoutMs: 250,
          pollIntervalMs: 5,
          settleMs: 10,
        },
      ),
    ).resolves.toEqual(entry);
  });

  it("rejects duplicate, mismatched, or malformed provider entries with diagnostics", async () => {
    const directory = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-provider-log-"));
    temporaryDirectories.push(directory);
    const path = NodePath.join(directory, "provider-input.jsonl");
    const entry: ProviderInputLogEntry = {
      provider: "grok",
      kind: "start",
      prompt: "/skills",
      recordedAt: "2026-07-23T12:00:00.000Z",
    };
    appendProviderInputLogEntry(path, entry);
    appendProviderInputLogEntry(path, entry);

    await expect(
      waitForProviderInputLogEntry(
        path,
        0,
        { provider: "grok", prompt: "/skills" },
        {
          timeoutMs: 100,
          pollIntervalMs: 5,
          settleMs: 10,
        },
      ),
    ).rejects.toThrow(/exactly one appended provider input.*observed 2/iu);

    await expect(
      waitForProviderInputLogEntry(
        path,
        1,
        { provider: "codex", prompt: "$refactor" },
        {
          timeoutMs: 100,
          pollIntervalMs: 5,
          settleMs: 10,
        },
      ),
    ).rejects.toThrow(/provider input mismatch.*codex.*grok/iu);

    const malformedPath = NodePath.join(directory, "malformed-provider-input.jsonl");
    NodeFS.writeFileSync(malformedPath, "{not-json}\n", "utf8");
    await expect(
      waitForProviderInputLogEntry(
        malformedPath,
        0,
        { provider: "codex", prompt: "$refactor" },
        {
          timeoutMs: 100,
          pollIntervalMs: 5,
          settleMs: 10,
        },
      ),
    ).rejects.toThrow(/provider input log became malformed.*not-json/iu);
  });

  it("distinguishes a steer from a new start with the same prompt", async () => {
    const directory = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-provider-log-"));
    temporaryDirectories.push(directory);
    const path = NodePath.join(directory, "provider-input.jsonl");
    const entry: ProviderInputLogEntry = {
      provider: "codex",
      kind: "steer",
      prompt: "second",
      turnId: "running-turn",
      recordedAt: "2026-09-22T20:00:00.000Z",
    };
    appendProviderInputLogEntry(path, entry);

    await expect(
      waitForProviderInputLogEntry(path, 0, { provider: "codex", prompt: "second", kind: "start" }),
    ).rejects.toThrow(/Provider input mismatch/);
    await expect(
      waitForProviderInputLogEntry(
        path,
        0,
        { provider: "codex", prompt: "second", kind: "steer" },
        { settleMs: 0 },
      ),
    ).resolves.toEqual(entry);
  });
});
