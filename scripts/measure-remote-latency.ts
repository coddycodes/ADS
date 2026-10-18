// @effect-diagnostics nodeBuiltinImport:off - A development harness owns processes and temp files.
// @effect-diagnostics globalTimers:off - Scenario pacing owns short, bounded timers.
// @effect-diagnostics globalDate:off - Measurements use wall-clock milliseconds.
// @effect-diagnostics globalConsole:off - The CLI prints its measurements for the operator.
/**
 * High-latency measurement harness (development only).
 *
 * Starts a disposable `bibcode serve` with the packaged-UI provider fixtures, puts
 * `scripts/throttle-proxy.ts` with a one-way delay between it and a headless
 * Chromium, then drives user actions and records, per action, the RPCs sent,
 * the strictly sequential round trips on its critical path, bytes in each
 * direction, and the time until the result is visible.
 *
 *   node scripts/measure-remote-latency.ts --server-bin target/debug/bibcode \
 *     --static-dir apps/web/dist [--rtt 0,250] [--jitter 0] [--out report.json]
 *
 * See docs/reference/scripts.md and docs/testing/cross-platform-validation.md.
 */
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeHttp from "node:http";
import * as NodeNet from "node:net";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as NodeZlib from "node:zlib";

import { launchChromium, type CdpPage, type WebSocketFrame } from "./lib/cdp.ts";
import { startThrottleProxy } from "./throttle-proxy.ts";

/** One RPC as seen from the browser. */
export interface RpcTrace {
  readonly id: string;
  readonly tag: string;
  readonly sentAt: number;
  /** First server message for this request (Chunk or Exit), if any. */
  readonly firstReplyAt: number | undefined;
  /** Every server message for this request, in arrival order. */
  readonly replyTimes: ReadonlyArray<number>;
}

interface DecodedMessage {
  readonly at: number;
  readonly direction: "sent" | "received";
  readonly socket: string;
  readonly bytes: number;
  readonly message: { readonly _tag?: string; readonly [key: string]: unknown };
}

const RECORD_FLAG_CONTINUATION = 0x01;
const RECORD_FLAG_CONTROL = 0x02;
const RECORD_FLAG_DEFLATE = 0x10;

/** Decodes RPC messages from text frames and plain `/ws` records (`bibcode.rpc.chunked.v1`). */
export function decodeFrames(frames: ReadonlyArray<WebSocketFrame>): Array<DecodedMessage> {
  const decoded: Array<DecodedMessage> = [];
  const partial = new Map<string, { parts: Array<Buffer>; bytes: number; deflated: boolean }>();
  const push = (frame: WebSocketFrame, text: string, bytes: number) => {
    for (const line of text.split("\n")) {
      if (line.trim().length === 0) continue;
      let value: unknown;
      try {
        value = JSON.parse(line);
      } catch {
        continue;
      }
      for (const message of Array.isArray(value) ? value : [value]) {
        decoded.push({
          at: frame.at,
          direction: frame.direction,
          socket: frame.socket,
          bytes,
          message,
        });
        bytes = 0;
      }
    }
  };
  for (const frame of frames) {
    if (frame.opcode === 1) {
      push(frame, frame.payload, frame.bytes);
      continue;
    }
    if (frame.opcode !== 2) continue;
    const record = Buffer.from(frame.payload, "base64");
    const key = `${frame.socket}:${frame.direction}`;
    const flag = record[0] ?? 0;
    // A control record is a whole message between another message's records.
    if (flag === RECORD_FLAG_CONTROL) {
      push(frame, record.subarray(1).toString("utf8"), frame.bytes);
      continue;
    }
    const state = partial.get(key) ?? { parts: [], bytes: 0, deflated: false };
    state.parts.push(record.subarray(1));
    state.bytes += frame.bytes;
    state.deflated = (flag & RECORD_FLAG_DEFLATE) !== 0;
    if ((flag & ~RECORD_FLAG_DEFLATE) === RECORD_FLAG_CONTINUATION) {
      partial.set(key, state);
      continue;
    }
    partial.delete(key);
    let body = Buffer.concat(state.parts);
    if (state.deflated) {
      try {
        body = NodeZlib.inflateRawSync(body.subarray(4));
      } catch {
        // The tail of a message whose first records precede the frames given.
        continue;
      }
    }
    push(frame, body.toString("utf8"), state.bytes);
  }
  return decoded;
}

/** Pairs requests with their replies. */
export function traceRpcs(messages: ReadonlyArray<DecodedMessage>): Array<RpcTrace> {
  const traces = new Map<
    string,
    { id: string; tag: string; sentAt: number; replyTimes: Array<number> }
  >();
  // Request ids restart on every socket.
  for (const { at, direction, socket, message } of messages) {
    if (direction === "sent" && message._tag === "Request") {
      const id = `${socket}:${String(message.id)}`;
      traces.set(id, { id, tag: String(message.tag), sentAt: at, replyTimes: [] });
    } else if (direction === "received" && typeof message.requestId === "string") {
      traces.get(`${socket}:${message.requestId}`)?.replyTimes.push(at);
    }
  }
  return [...traces.values()].map((trace) => ({
    ...trace,
    firstReplyAt: trace.replyTimes[0],
  }));
}

/**
 * Length of the longest chain of requests where each was sent within
 * `causalGapMs` after a reply to the previous one arrived. A chain of N requests
 * costs N round trips however short the link.
 */
export function criticalPath(
  traces: ReadonlyArray<RpcTrace>,
  causalGapMs: number,
): { readonly depth: number; readonly chain: ReadonlyArray<string> } {
  const ordered = [...traces].toSorted((left, right) => left.sentAt - right.sentAt);
  const best = new Map<string, { depth: number; chain: Array<string> }>();
  let result = { depth: 0, chain: [] as Array<string> };
  for (const trace of ordered) {
    let entry = { depth: 1, chain: [trace.tag] };
    for (const earlier of ordered) {
      if (earlier === trace || earlier.sentAt >= trace.sentAt) continue;
      const triggered = earlier.replyTimes.some(
        (replyAt) => replyAt <= trace.sentAt && trace.sentAt - replyAt <= causalGapMs,
      );
      const previous = best.get(earlier.id);
      if (triggered && previous && previous.depth + 1 > entry.depth) {
        entry = { depth: previous.depth + 1, chain: [...previous.chain, trace.tag] };
      }
    }
    best.set(trace.id, entry);
    if (entry.depth > result.depth) result = entry;
  }
  return result;
}

/** Timer-driven polls that run whatever the user does; they are not part of any action. */
/** The name the page uses for its server; Chromium maps it to 127.0.0.1. */
const REMOTE_HOST = "bibcode-remote.test";

/** Requests an action sends this long after its result shows still belong to it. */
const SETTLE_MS = 500;

const BACKGROUND_POLLS = new Set([
  "server.getProcessDiagnostics",
  "server.getProviderUsage",
  "server.refreshProviderUsage",
]);

export interface ActionMeasurement {
  readonly action: string;
  readonly rttMs: number;
  readonly visibleMs: number;
  readonly rpcs: number;
  readonly rpcTags: Readonly<Record<string, number>>;
  readonly criticalPath: number;
  readonly criticalChain: ReadonlyArray<string>;
  readonly bytesSent: number;
  readonly bytesReceived: number;
  /** Typing rows: delay from each keystroke until its echo arrived, median and 95th percentile. */
  readonly echoLagP50Ms?: number;
  readonly echoLagP95Ms?: number;
}

/** Nearest-rank percentile of `values` (0..100). */
export function percentile(values: ReadonlyArray<number>, rank: number): number {
  const sorted = [...values].toSorted((left, right) => left - right);
  return (
    sorted[Math.min(sorted.length - 1, Math.max(0, Math.ceil((rank / 100) * sorted.length) - 1))] ??
    0
  );
}

/**
 * For each keystroke time, the delay until the echoed text (normalised by `normalize`)
 * first contained everything typed up to and including that key.
 */
export function echoLags(
  keyTimes: ReadonlyArray<number>,
  typed: string,
  echoes: ReadonlyArray<{ readonly at: number; readonly text: string }>,
  normalize: (text: string) => string,
): Array<number> {
  let received = "";
  const arrivals: Array<{ at: number; text: string }> = [];
  for (const echo of echoes) {
    received += echo.text;
    arrivals.push({ at: echo.at, text: normalize(received) });
  }
  return keyTimes.flatMap((keyAt, index) => {
    // A key the normaliser drops (a space) has no echo of its own to time.
    if (normalize(typed[index] ?? "").length === 0) return [];
    const prefix = normalize(typed.slice(0, index + 1));
    const arrival = arrivals.find((candidate) => candidate.text.includes(prefix));
    return arrival === undefined ? [] : [Math.max(0, arrival.at - keyAt)];
  });
}

export function summarizeWindow(
  action: string,
  rttMs: number,
  frames: ReadonlyArray<WebSocketFrame>,
  startedAt: number,
  visibleAt: number,
  settledAt: number,
): ActionMeasurement {
  const socketFrames = frames.filter((frame) => frame.url.includes("/ws"));
  const inWindow = socketFrames.filter((frame) => frame.at >= startedAt && frame.at <= settledAt);
  // Decoded from the whole stream, so a message whose records began before the action
  // still reassembles; requests count when the action, or its settling, sent them.
  const traces = traceRpcs(decodeFrames(socketFrames)).filter(
    (trace) =>
      trace.sentAt >= startedAt && trace.sentAt <= settledAt && !BACKGROUND_POLLS.has(trace.tag),
  );
  const rpcTags: Record<string, number> = {};
  for (const trace of traces) rpcTags[trace.tag] = (rpcTags[trace.tag] ?? 0) + 1;
  // A follow-up request leaves within a few milliseconds of the reply that triggered it.
  // Only answered requests can have held up the result; an optimistic one still counts above.
  const path = criticalPath(
    traces.filter(
      (trace) =>
        trace.sentAt <= visibleAt &&
        trace.firstReplyAt !== undefined &&
        trace.firstReplyAt <= visibleAt,
    ),
    Math.max(40, rttMs / 5),
  );
  return {
    action,
    rttMs,
    visibleMs: visibleAt - startedAt,
    rpcs: traces.length,
    rpcTags,
    criticalPath: path.depth,
    criticalChain: path.chain,
    bytesSent: inWindow
      .filter((frame) => frame.direction === "sent")
      .reduce((sum, frame) => sum + frame.bytes, 0),
    bytesReceived: inWindow
      .filter((frame) => frame.direction === "received")
      .reduce((sum, frame) => sum + frame.bytes, 0),
  };
}

const MIME_TYPES: Readonly<Record<string, string>> = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".css": "text/css",
  ".json": "application/json",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".ico": "image/x-icon",
  ".woff2": "font/woff2",
  ".wasm": "application/wasm",
};

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/** JS that finds the last visible button whose text starts with, or aria-label equals, `label`. */
const findButton = (label: string) => `(() => {
  const label = ${JSON.stringify(label)};
  const candidates = [...document.querySelectorAll("button, [role=button], [role=menuitem], [role=tab]")]
    .filter((element) => {
      const rect = element.getBoundingClientRect();
      return rect.width > 0 && rect.height > 0 &&
        (element.getAttribute("aria-label") === label || element.innerText.trim().startsWith(label));
    });
  return candidates[candidates.length - 1] ?? null;
})()`;

async function clickButton(page: CdpPage, label: string, timeoutMs = 30_000): Promise<void> {
  await page.waitFor(findButton(label), timeoutMs);
  await page.click(findButton(label));
}

async function freePort(): Promise<number> {
  const server = NodeNet.createServer();
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = (server.address() as NodeNet.AddressInfo).port;
  await new Promise<void>((resolve) => server.close(() => resolve()));
  return port;
}

interface Harness {
  readonly page: CdpPage;
  readonly debugPort: string;
  readonly origin: string;
  readonly projectPath: string;
  readonly setDelay: (oneWayMs: number, jitterMs: number) => void;
  readonly dropSockets: () => void;
  readonly close: () => Promise<void>;
}

async function startHarness(options: {
  readonly serverBin: string;
  readonly staticDir: string;
  readonly chromium: string;
  readonly runRoot: string;
  /**
   * Every acquired resource pushes its release here at once, so the caller can release
   * a harness still starting (a signal) and a failure part-way through releases the rest.
   */
  readonly releases: Array<() => Promise<void>>;
}): Promise<Harness> {
  const environment: NodeJS.ProcessEnv = {
    PATH: process.env.PATH,
    BIBCODE_E2E_RUN_ROOT: options.runRoot,
  };
  // The packaged-UI provider fixtures live with the desktop e2e suite, its own TS project.
  const fixtures = (await import(
    new URL("../apps/desktop/e2e/support/test-project.ts", import.meta.url).href
  )) as {
    readonly prepareDesktopUiTestContext: (environment: NodeJS.ProcessEnv) => {
      readonly projectPath: string;
    };
  };
  const context = fixtures.prepareDesktopUiTestContext(environment);
  // A second repository gives "switch thread" a different project to move to, and both
  // repositories some files and changes for the Files and Source Control panels.
  const secondProject = NodePath.join(NodePath.dirname(context.projectPath), "Second Fixture");
  for (const project of [context.projectPath, secondProject]) {
    NodeFS.mkdirSync(NodePath.join(project, "src"), { recursive: true });
    for (let index = 0; index < 40; index++) {
      NodeFS.writeFileSync(
        NodePath.join(project, "src", `module-${index}.ts`),
        `export const value${index} = ${index};\n`.repeat(20),
      );
    }
    const git = (...args: Array<string>) =>
      NodeChildProcess.spawnSync(
        "git",
        [
          "-C",
          project,
          "-c",
          "user.name=Fixture",
          "-c",
          "user.email=fixture@example.test",
          ...args,
        ],
        { stdio: "ignore" },
      );
    if (!NodeFS.existsSync(NodePath.join(project, ".git"))) git("init", "--initial-branch=main");
    git("add", ".");
    git("commit", "-q", "-m", "fixture files");
    // Uncommitted edits, so Source Control has something to list.
    for (let index = 0; index < 5; index++) {
      NodeFS.appendFileSync(NodePath.join(project, "src", `module-${index}.ts`), "// edited\n");
    }
  }
  const { releases } = options;
  const close = async () => {
    for (const release of releases.splice(0).toReversed()) {
      await release().catch(() => undefined);
    }
  };
  const serverPort = await freePort();
  const server = NodeChildProcess.spawn(
    options.serverBin,
    ["serve", "--mode", "web", "--host", "127.0.0.1", "--port", String(serverPort), "--no-browser"],
    {
      env: {
        ...environment,
        RUST_LOG: "warn",
        BIBCODE_LOG: "warn",
        // The compression kill switch passes through, to measure with it off.
        ...(process.env.BIBCODE_REMOTE_COMPRESSION === undefined
          ? {}
          : { BIBCODE_REMOTE_COMPRESSION: process.env.BIBCODE_REMOTE_COMPRESSION }),
      },
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  releases.push(async () => {
    if (server.exitCode !== null || server.signalCode !== null) return;
    const exited = new Promise((resolve) => server.once("exit", resolve));
    server.kill("SIGTERM");
    await exited;
  });
  const serverLog = NodeFS.createWriteStream(NodePath.join(options.runRoot, "server.log"));
  server.stderr.pipe(serverLog);
  try {
    const pairingPath = await new Promise<string>((resolve, reject) => {
      let output = "";
      server.stdout.on("data", (data: Buffer) => {
        output += data.toString();
        serverLog.write(data);
        const match = output.match(/https?:\/\/[^\s"]*\/pair#[^\s"]+/);
        if (match) resolve(new URL(match[0]).hash);
      });
      server.once("exit", (code) => reject(new Error(`server exited early (${code}): ${output}`)));
    });
    const proxy = await startThrottleProxy({
      listenHost: "127.0.0.1",
      listenPort: 0,
      targetHost: "127.0.0.1",
      targetPort: serverPort,
    });
    releases.push(proxy.close);
    // The page loads its assets from this local, undelayed front server, as the desktop app
    // loads them from disk; only the environment's HTTP and RPC traffic crosses the proxy.
    const forwarded = /^\/(api\/|ws|\.well-known\/|mcp|oauth|runtime)/;
    const front = NodeHttp.createServer((request, response) => {
      const path = new URL(request.url ?? "/", "http://front.invalid").pathname;
      if (forwarded.test(path)) {
        const upstream = NodeHttp.request(
          {
            host: "127.0.0.1",
            port: proxy.port,
            path: request.url,
            method: request.method,
            headers: request.headers,
          },
          (reply) => {
            response.writeHead(reply.statusCode ?? 502, reply.headers);
            reply.pipe(response);
          },
        );
        upstream.on("error", () => response.destroy());
        request.pipe(upstream);
        return;
      }
      const file = NodePath.join(options.staticDir, NodePath.normalize(path));
      const served =
        file.startsWith(options.staticDir) &&
        NodeFS.existsSync(file) &&
        NodeFS.statSync(file).isFile()
          ? file
          : NodePath.join(options.staticDir, "index.html");
      response.writeHead(200, {
        "content-type": MIME_TYPES[NodePath.extname(served)] ?? "application/octet-stream",
      });
      NodeFS.createReadStream(served).pipe(response);
    });
    // WebSocket upgrades pass through as raw bytes.
    front.on("upgrade", (request, socket, head) => {
      const upstream = NodeNet.connect(proxy.port, "127.0.0.1", () => {
        const headers = Object.entries(request.headers).flatMap(([name, value]) =>
          (Array.isArray(value) ? value : [value ?? ""]).map((item) => `${name}: ${item}`),
        );
        upstream.write(
          `${request.method} ${request.url} HTTP/1.1\r\n${headers.join("\r\n")}\r\n\r\n`,
        );
        upstream.write(head);
        socket.pipe(upstream).pipe(socket);
      });
      const destroy = () => {
        socket.destroy();
        upstream.destroy();
      };
      upstream.on("error", destroy);
      socket.on("error", destroy);
      upstream.on("close", destroy);
      socket.on("close", destroy);
    });
    await new Promise<void>((resolve) => front.listen(0, "127.0.0.1", resolve));
    releases.push(
      () =>
        new Promise<void>((resolve) => {
          front.closeAllConnections();
          front.close(() => resolve());
        }),
    );
    // The page reaches the front server by a name that is not loopback, as a remote
    // server would be, so the client treats it as remote (for example, it offers
    // compression); Chromium resolves the name to the local front server.
    const origin = `http://${REMOTE_HOST}:${(front.address() as NodeNet.AddressInfo).port}`;
    const browser = await launchChromium(
      options.chromium,
      NodePath.join(options.runRoot, "chromium"),
      (release) => releases.push(release),
      [`--host-resolver-rules=MAP ${REMOTE_HOST} 127.0.0.1`],
    );
    releases.push(browser.close);
    const page = browser.page;
    try {
      await page.navigate(`${origin}/`);
      await page.waitFor("document.readyState === 'complete'");
      const credential = new URLSearchParams(pairingPath.slice(1)).get("token");
      const paired = await page.evaluate<number>(
        `fetch("/api/auth/browser-session", { method: "POST", credentials: "include",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ credential: ${JSON.stringify(credential)} }) }).then((r) => r.status)`,
      );
      if (paired !== 200) throw new Error(`pairing failed with HTTP ${paired}`);
      await page.navigate(`${origin}/`);
      for (const projectPath of [context.projectPath, secondProject]) {
        await page.waitFor(`document.querySelector('[data-testid="sidebar-add-project-trigger"]')`);
        // The trigger renders before its handler is ready on a fresh load; retry the click.
        for (let attempt = 0; ; attempt++) {
          await clickButton(page, "Add project");
          try {
            await page.waitFor(findButton("Browse folder"), 2_000);
            break;
          } catch (error) {
            if (attempt >= 10) throw error;
          }
        }
        await clickButton(page, "Browse folder");
        await clickButton(page, "Type a path instead");
        const input = `document.querySelector('input[placeholder="/path/to/project/on/host"]')`;
        await page.waitFor(input);
        await page.evaluate(`(${input}.focus(), ${input}.select())`);
        await page.send("Input.insertText", { text: projectPath });
        await page.waitFor(`${input}.value === ${JSON.stringify(projectPath)}`);
        const listed = `[...document.querySelectorAll('[data-sidebar="menu-button"]')].some((element) =>
          element.innerText.includes(${JSON.stringify(NodePath.basename(projectPath))}))`;
        // The dialog ignores a submit until the environment's project catalog is ready.
        for (let attempt = 0; ; attempt++) {
          await page.evaluate(`${input}?.focus()`);
          await page.pressKey("Enter", "Enter", 13);
          try {
            await page.waitFor(listed, 3_000);
            break;
          } catch (error) {
            if (attempt >= 10) throw error;
          }
        }
      }
    } catch (error) {
      const text = await page
        .evaluate<string>(
          `JSON.stringify([...document.querySelectorAll("input")].map((input) => input.value)) +
          "\\n" + document.activeElement?.outerHTML.slice(0, 300) + "\\n" + document.body.innerText`,
        )
        .catch(() => "");
      NodeFS.writeFileSync(
        NodePath.join(options.runRoot, "setup-failure.txt"),
        `${String(error)}\n\n${text}`,
      );
      throw error;
    }
    return {
      page: browser.page,
      debugPort: browser.debugPort,
      origin,
      projectPath: context.projectPath,
      setDelay: (delay, jitter) => {
        proxy.update({ delay, jitter });
      },
      dropSockets: () => proxy.drop(),
      close,
    };
  } catch (error) {
    await close();
    throw error;
  }
}

function parseArgs(argv: ReadonlyArray<string>): Map<string, string> {
  const args = new Map<string, string>();
  for (let index = 0; index < argv.length; index++) {
    const key = argv[index]!;
    if (!key.startsWith("--")) continue;
    const value = argv[index + 1];
    if (value === undefined || value.startsWith("--")) args.set(key, "1");
    else {
      args.set(key, value);
      index++;
    }
  }
  return args;
}

export async function runMeasureRemoteLatencyMain(
  isMain: boolean,
  argv: ReadonlyArray<string> = process.argv.slice(2),
): Promise<boolean> {
  if (!isMain) return false;
  const args = parseArgs(argv);
  // Always a fresh directory of our own, so cleanup never removes anything else.
  const parent = args.get("--run-root") ?? NodeOS.tmpdir();
  NodeFS.mkdirSync(parent, { recursive: true });
  const runRoot = NodeFS.mkdtempSync(NodePath.join(parent, "bibcode-latency-"));
  const rtts = (args.get("--rtt") ?? "0,250").split(",").map(Number);
  const all: Array<ActionMeasurement> = [];
  for (const [index, rttMs] of rtts.entries()) {
    const harnessRoot = NodePath.join(runRoot, `rtt-${rttMs}-${index}`);
    NodeFS.mkdirSync(harnessRoot, { recursive: true });
    const releases: Array<() => Promise<void>> = [];
    const stop = () => {
      void (async () => {
        for (const release of releases.splice(0).toReversed()) {
          await release().catch(() => undefined);
        }
      })().finally(() => process.exit(130));
    };
    process.once("SIGINT", stop);
    process.once("SIGTERM", stop);
    let harness: Harness | undefined;
    try {
      harness = await startHarness({
        serverBin: NodePath.resolve(args.get("--server-bin") ?? "target/debug/bibcode"),
        staticDir: NodePath.resolve(args.get("--static-dir") ?? "apps/web/dist"),
        chromium: args.get("--chromium") ?? "/usr/bin/chromium",
        runRoot: harnessRoot,
        releases,
      });
      if (args.has("--explore")) {
        console.log(
          `origin ${harness.origin} devtools ${harness.debugPort} project ${harness.projectPath}`,
        );
        harness.setDelay(rttMs / 2, Number(args.get("--jitter") ?? 0));
        await sleep(Number(args.get("--explore")) * 1000);
        return true;
      }
      all.push(
        ...(await runScenarios(harness, rttMs, {
          jitterMs: Number(args.get("--jitter") ?? 0),
          typingIntervalMs: Number(args.get("--typing-interval") ?? 100),
          seedMessages: Number(args.get("--seed-messages") ?? 3),
        })),
      );
    } finally {
      process.off("SIGINT", stop);
      process.off("SIGTERM", stop);
      await harness?.close();
    }
  }
  const out = args.get("--out");
  if (out) NodeFS.writeFileSync(out, `${JSON.stringify(all, null, 2)}\n`);
  console.log(formatTable(all));
  if (!args.has("--keep")) NodeFS.rmSync(runRoot, { recursive: true, force: true });
  return true;
}

/** A Markdown table with one row per action and one column group per RTT. */
export function formatTable(measurements: ReadonlyArray<ActionMeasurement>): string {
  const rtts = [...new Set(measurements.map((measurement) => measurement.rttMs))];
  const actions = [...new Set(measurements.map((measurement) => measurement.action))];
  const header = [
    "| Action | " +
      rtts
        .map(
          (rtt) =>
            `${rtt} ms: RPCs | ${rtt} ms: sequential | ${rtt} ms: sent/recv | ${rtt} ms: visible | ${rtt} ms: echo lag p50/p95`,
        )
        .join(" | ") +
      " |",
    "| --- | " + rtts.map(() => "---: | ---: | ---: | ---: | ---:").join(" | ") + " |",
  ];
  const kb = (bytes: number) => `${(bytes / 1024).toFixed(1)} KB`;
  const rows = actions.map((action) => {
    const cells = rtts.map((rtt) => {
      const measurement = measurements.find(
        (candidate) => candidate.action === action && candidate.rttMs === rtt,
      );
      return measurement
        ? `${measurement.rpcs} | ${measurement.criticalPath} | ${kb(measurement.bytesSent)} / ${kb(measurement.bytesReceived)} | ${measurement.visibleMs} ms | ${
            measurement.echoLagP50Ms === undefined
              ? ""
              : `${measurement.echoLagP50Ms} / ${measurement.echoLagP95Ms} ms`
          }`
        : " | | | | ";
    });
    return `| ${action} | ${cells.join(" | ")} |`;
  });
  return [...header, ...rows].join("\n");
}

const TYPED = "the quick brown fox jumps over a lazy dog";
const RESPONSE = "BiBCode deterministic streamed fixture response.";

/** The sidebar thread button of the primary thread in the project named `project`. */
const threadButton = (project: string) => `(() => {
  const projectButton = [...document.querySelectorAll('[data-sidebar="menu-button"]')]
    .find((element) => element.innerText.trim() === ${JSON.stringify(project)});
  return projectButton?.closest('[data-sidebar="menu-item"]')
    ?.querySelector('[data-thread-item="true"] button') ?? null;
})()`;
const bodyIncludes = (text: string) => `document.body.innerText.includes(${JSON.stringify(text)})`;
/** The transcript shows a response after the last occurrence of `prompt`. */
const answered = (prompt: string) => `(() => {
  // The timeline is virtualised (its newest rows are the ones in the DOM) and its DOM
  // order need not be its visual order, so order rows by position.
  const rows = [...document.querySelectorAll("[data-message-role]")].toSorted(
    (left, right) => left.getBoundingClientRect().top - right.getBoundingClientRect().top);
  const asked = rows.findLastIndex((row) =>
    row.dataset.messageRole === "user" && row.innerText.includes(${JSON.stringify(prompt)}));
  return asked >= 0 && rows.slice(asked + 1).some((row) =>
    row.dataset.messageRole === "assistant" && row.innerText.includes(${JSON.stringify(RESPONSE)}));
})()`;
/** The timeline shows `prompt` as a sent user message. */
const sent = (prompt: string) =>
  `[...document.querySelectorAll('[data-message-role="user"]')].some((row) =>
    row.innerText.includes(${JSON.stringify(prompt)}))`;

/** Terminal output received since `since`, one entry per output event. */
function terminalOutputEvents(
  frames: ReadonlyArray<WebSocketFrame>,
  since: number,
): Array<{ readonly at: number; readonly text: string }> {
  const events: Array<{ at: number; text: string }> = [];
  for (const { message, at } of decodeFrames(frames.filter((frame) => frame.at >= since))) {
    if (message._tag !== "Chunk" || !Array.isArray(message.values)) continue;
    for (const value of message.values as Array<{
      type?: string;
      data?: unknown;
      snapshot?: { history?: unknown };
    }>) {
      if (value.type === "output" && typeof value.data === "string") {
        events.push({ at, text: value.data });
      } else if (value.type === "snapshot" && typeof value.snapshot?.history === "string") {
        // An attach (or resync) snapshot carries the transcript the terminal shows.
        events.push({ at, text: value.snapshot.history });
      }
    }
  }
  return events;
}

const terminalOutputSince = (frames: ReadonlyArray<WebSocketFrame>, since: number) =>
  terminalOutputEvents(frames, since)
    .map((event) => event.text)
    .join("");

/** Readline may redraw or wrap the line; compare without escapes and whitespace. */
const CSI_SEQUENCE = new RegExp(`${String.fromCharCode(0x1b)}\\[[0-9;?]*[A-Za-z]`, "g");
const normalizeTerminalText = (text: string) => text.replace(CSI_SEQUENCE, "").replace(/\s+/g, "");

/** Types `text` one key every `intervalMs`; returns when each key was dispatched. */
async function typeText(page: CdpPage, text: string, intervalMs: number): Promise<Array<number>> {
  const times: Array<number> = [];
  for (const character of text) {
    const started = Date.now();
    await page.typeCharacter(character);
    times.push(Date.now());
    await sleep(Math.max(0, intervalMs - (Date.now() - started)));
  }
  return times;
}

async function waitUntil(check: () => boolean, timeoutMs = 60_000): Promise<number> {
  const deadline = Date.now() + timeoutMs;
  while (!check()) {
    if (Date.now() > deadline) throw new Error("Timed out waiting for a frame condition");
    await sleep(2);
  }
  return Date.now();
}

/** Runs every scenario on `harness` at `rttMs`; the harness was set up on an undelayed link. */
export async function runScenarios(
  harness: Harness,
  rttMs: number,
  options: {
    readonly jitterMs: number;
    readonly typingIntervalMs: number;
    readonly seedMessages: number;
  },
): Promise<Array<ActionMeasurement>> {
  const { page } = harness;
  const results: Array<ActionMeasurement> = [];
  const measure = async (
    action: string,
    run: () => Promise<number | { readonly visibleAt: number; readonly lags: Array<number> }>,
  ) => {
    await sleep(1_500);
    const startedAt = Date.now();
    const outcome = await run().catch(async (error: unknown) => {
      const text = await page.evaluate<string>("document.body.innerText").catch(() => "");
      throw new Error(`${action} failed: ${String(error)}\n--- page text ---\n${text}`, {
        cause: error,
      });
    });
    const visibleAt = typeof outcome === "number" ? outcome : outcome.visibleAt;
    await sleep(SETTLE_MS + 100);
    const window = summarizeWindow(
      action,
      rttMs,
      page.frames,
      startedAt,
      visibleAt,
      visibleAt + SETTLE_MS,
    );
    const measurement: ActionMeasurement =
      typeof outcome === "number"
        ? window
        : {
            ...window,
            echoLagP50Ms: percentile(outcome.lags, 50),
            echoLagP95Ms: percentile(outcome.lags, 95),
          };
    results.push(measurement);
    console.log(JSON.stringify(measurement));
    return measurement;
  };
  const project = "BiBCode UI Fixture";
  const other = "Second Fixture";

  // History for the thread-open scenarios, recorded before the link slows down.
  await page.click(threadButton(project));
  await page.waitFor(`document.querySelector('[data-testid="composer-editor"]')`);
  for (let index = 0; index < options.seedMessages; index++) {
    await page.evaluate(`document.querySelector('[data-testid="composer-editor"]').focus()`);
    await page.send("Input.insertText", { text: `seed message ${index}` });
    // The composer can ignore a submit while the previous turn settles; retry until it clears.
    for (let attempt = 0; ; attempt++) {
      await page.pressKey("Enter", "Enter", 13);
      try {
        await page.waitFor(
          `document.querySelector('[data-testid="composer-editor"]').innerText.trim() === ""`,
          2_000,
        );
        break;
      } catch (error) {
        if (attempt >= 10) throw error;
      }
    }
    await page.waitFor(answered(`seed message ${index}`));
    await page.waitFor(`!document.querySelector('[aria-label="Stop generation"]')`);
  }
  harness.setDelay(rttMs / 2, options.jitterMs);

  await measure("composer: type 40 characters", async () => {
    await page.evaluate(`document.querySelector('[data-testid="composer-editor"]').focus()`);
    await typeText(page, TYPED, options.typingIntervalMs);
    return page.waitFor(
      `document.querySelector('[data-testid="composer-editor"]').innerText.includes(${JSON.stringify(TYPED)})`,
    );
  });

  let sendStarted = 0;
  let responseVisible: Promise<number> = Promise.resolve(0);
  await measure("send message: user message visible", async () => {
    sendStarted = Date.now();
    await page.pressKey("Enter", "Enter", 13);
    // Observed from the submit on, so the settle time below never delays it.
    responseVisible = page.waitFor(answered(TYPED));
    return page.waitFor(sent(TYPED));
  });
  const responseAt = await responseVisible;
  // This row's own settling window, which can end after the previous row's.
  await sleep(Math.max(0, responseAt + SETTLE_MS + 100 - Date.now()));
  results.push({
    ...summarizeWindow(
      "send message: response visible",
      rttMs,
      page.frames,
      sendStarted,
      responseAt,
      responseAt + SETTLE_MS,
    ),
  });
  console.log(JSON.stringify(results.at(-1)));

  await measure("switch thread: to an empty thread", async () => {
    await page.click(threadButton(other));
    return page.waitFor(bodyIncludes("Send a message to start the conversation."));
  });
  await measure("switch thread: back to a thread with history", async () => {
    await page.click(threadButton(project));
    return page.waitFor(answered(TYPED));
  });

  await measure("source control: open panel", async () => {
    await clickButton(page, "Toggle right panel");
    await clickButton(page, "Source Control");
    return page.waitFor(bodyIncludes("module-0.ts"));
  });
  await clickButton(page, "Close Source Control");
  await measure("files: open panel", async () => {
    await clickButton(page, "Toggle right panel");
    await clickButton(page, "Files");
    // The tree is virtualised; its header counts the listed files once the listing arrives.
    return page.waitFor(`/\\b\\d+ files\\b/.test(document.body.innerText)`);
  });
  await clickButton(page, "Close Files");

  await measure("terminal: open", async () => {
    // From before the click: on a fast link the prompt can arrive while it is acknowledged.
    const since = Date.now();
    await clickButton(page, "Toggle right panel");
    await clickButton(page, "Terminal");
    // The first text, from the attach snapshot or output, is the shell's prompt, whatever
    // the shell draws it with.
    return waitUntil(() => terminalOutputSince(page.frames, since).trim().length > 0);
  });
  await sleep(1_000);
  await measure("terminal: type 40 characters", async () => {
    const since = Date.now();
    await page.evaluate(`document.querySelector(".xterm textarea").focus()`);
    const keyTimes = await typeText(page, TYPED, options.typingIntervalMs);
    const visibleAt = await waitUntil(() =>
      normalizeTerminalText(terminalOutputSince(page.frames, since)).includes(
        normalizeTerminalText(TYPED),
      ),
    ).catch((error: unknown) => {
      throw new Error(
        `${String(error)}; output was ${JSON.stringify(terminalOutputSince(page.frames, since))}`,
      );
    });
    const lags = echoLags(
      keyTimes,
      TYPED,
      terminalOutputEvents(page.frames, since),
      normalizeTerminalText,
    );
    return { visibleAt, lags };
  });
  await page.evaluate(`document.querySelector(".xterm textarea").focus()`);
  await page.send("Input.insertText", { text: "\u0015" });

  // Connect to first thread paint: a reload straight onto a thread's route, the way the
  // app reopens where it was, with nothing cached in the page.
  const threadPath = await page.evaluate<string>(
    `(${threadButton(project)}?.click(), new Promise((resolve) =>
      setTimeout(() => resolve(location.pathname), 500)))`,
  );
  await measure("reload onto a thread route until it paints, no local cache", async () => {
    await page.send("Storage.clearDataForOrigin", {
      origin: harness.origin,
      storageTypes: "local_storage,indexeddb,cache_storage,service_workers,websql,file_systems",
    });
    await page.evaluate("window.__beforeReload = true");
    await page.navigate(`${harness.origin}${threadPath}`);
    return page.waitFor(`!window.__beforeReload && ${answered(TYPED)}`);
  });

  await measure("open thread after reload, no local cache", async () => {
    // Without this the thread paints from the page's own cache before any RPC returns.
    await page.send("Storage.clearDataForOrigin", {
      origin: harness.origin,
      storageTypes: "local_storage,indexeddb,cache_storage,service_workers,websql,file_systems",
    });
    await page.evaluate("window.__beforeReload = true");
    await page.navigate(`${harness.origin}/`);
    await page.waitFor(`!window.__beforeReload && ${threadButton(project)}`);
    await page.click(threadButton(project));
    return page.waitFor(answered(TYPED));
  });

  await measure("reconnect: dropped socket until thread resynced", async () => {
    const socketsBefore = new Set(page.frames.map((frame) => frame.socket));
    harness.dropSockets();
    // A Chunk carries thread state; an Exit could be a failure.
    return waitUntil(() => {
      const messages = decodeFrames(
        page.frames.filter((frame) => !socketsBefore.has(frame.socket)),
      );
      const subscriptions = new Set(
        messages
          .filter(
            ({ direction, message }) =>
              direction === "sent" &&
              message._tag === "Request" &&
              message.tag === "orchestration.subscribeThread",
          )
          .map(({ socket, message }) => `${socket}:${String(message.id)}`),
      );
      return messages.some(
        ({ direction, socket, message }) =>
          direction === "received" &&
          message._tag === "Chunk" &&
          subscriptions.has(`${socket}:${String(message.requestId)}`),
      );
    });
  });
  return results;
}

void runMeasureRemoteLatencyMain(import.meta.main);
