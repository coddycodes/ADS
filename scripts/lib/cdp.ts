// @effect-diagnostics nodeBuiltinImport:off - A development harness drives a local browser process.
// @effect-diagnostics globalTimers:off - Polling waits own short, bounded timers.
// @effect-diagnostics globalDate:off - Frame timestamps use wall-clock milliseconds.
// @effect-diagnostics globalFetch:off - DevTools lists its targets over loopback HTTP.
/**
 * Minimal Chrome DevTools Protocol driver for development measurement scripts.
 * It launches a headless Chromium, attaches to its first page and records every
 * WebSocket frame the page sends or receives. No production code uses it.
 */
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";

export interface WebSocketFrame {
  /** Wall-clock milliseconds when the driver received the event. */
  readonly at: number;
  readonly direction: "sent" | "received";
  readonly socket: string;
  readonly url: string;
  /** 1 text, 2 binary. */
  readonly opcode: number;
  /** Text payload, or base64 for binary frames. */
  readonly payload: string;
  /** Payload size in bytes. */
  readonly bytes: number;
}

type Pending = { resolve: (value: unknown) => void; reject: (error: Error) => void };

export class CdpPage {
  private nextId = 1;
  private readonly pending = new Map<number, Pending>();
  private readonly sockets = new Map<string, string>();
  readonly frames: Array<WebSocketFrame> = [];
  readonly closedSockets: Array<{ readonly at: number; readonly socket: string }> = [];

  private readonly ws: WebSocket;

  private constructor(ws: WebSocket) {
    this.ws = ws;
    ws.addEventListener("message", (event) => this.onMessage(String(event.data)));
    // A command still waiting when the debugger goes away would otherwise never settle.
    ws.addEventListener("close", () => {
      for (const pending of this.pending.values()) {
        pending.reject(new Error("CDP connection closed"));
      }
      this.pending.clear();
    });
  }

  static async connect(webSocketDebuggerUrl: string): Promise<CdpPage> {
    const ws = new WebSocket(webSocketDebuggerUrl);
    await new Promise<void>((resolve, reject) => {
      ws.addEventListener("open", () => resolve(), { once: true });
      ws.addEventListener("error", () => reject(new Error("CDP connection failed")), {
        once: true,
      });
    });
    const page = new CdpPage(ws);
    await page.send("Network.enable", { maxTotalBufferSize: 0 });
    await page.send("Page.enable");
    await page.send("Runtime.enable");
    // A headless page never has OS focus; without this, focus() and typing go nowhere.
    await page.send("Emulation.setFocusEmulationEnabled", { enabled: true });
    return page;
  }

  private onMessage(raw: string): void {
    const message = JSON.parse(raw) as {
      id?: number;
      result?: unknown;
      error?: { message: string };
      method?: string;
      params?: Record<string, unknown>;
    };
    if (message.id !== undefined) {
      const pending = this.pending.get(message.id);
      this.pending.delete(message.id);
      if (message.error) pending?.reject(new Error(message.error.message));
      else pending?.resolve(message.result);
      return;
    }
    const params = message.params ?? {};
    switch (message.method) {
      case "Network.webSocketCreated":
        this.sockets.set(String(params.requestId), String(params.url));
        return;
      case "Network.webSocketClosed":
        this.closedSockets.push({ at: Date.now(), socket: String(params.requestId) });
        return;
      case "Network.webSocketFrameSent":
      case "Network.webSocketFrameReceived": {
        const response = params.response as { opcode: number; payloadData: string };
        const socket = String(params.requestId);
        this.frames.push({
          at: Date.now(),
          direction: message.method === "Network.webSocketFrameSent" ? "sent" : "received",
          socket,
          url: this.sockets.get(socket) ?? "",
          opcode: response.opcode,
          payload: response.payloadData,
          bytes:
            response.opcode === 2
              ? Buffer.from(response.payloadData, "base64").length
              : Buffer.byteLength(response.payloadData),
        });
        return;
      }
    }
  }

  send<T = unknown>(method: string, params: Record<string, unknown> = {}): Promise<T> {
    if (this.ws.readyState !== WebSocket.OPEN) {
      return Promise.reject(new Error("CDP connection closed"));
    }
    const id = this.nextId++;
    this.ws.send(JSON.stringify({ id, method, params }));
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (value: unknown) => void, reject });
    });
  }

  /** Evaluates an expression in the page and returns its JSON value. */
  async evaluate<T = unknown>(expression: string): Promise<T> {
    const result = await this.send<{
      result: { value?: T };
      exceptionDetails?: { exception?: { description?: string }; text: string };
    }>("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (result.exceptionDetails) {
      throw new Error(
        result.exceptionDetails.exception?.description ?? result.exceptionDetails.text,
      );
    }
    return result.result.value as T;
  }

  /** Polls `expression` until it is truthy; returns the wall-clock time it first was. */
  async waitFor(expression: string, timeoutMs = 30_000, intervalMs = 5): Promise<number> {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      if (await this.evaluate<boolean>(`Boolean(${expression})`)) return Date.now();
      if (Date.now() > deadline) throw new Error(`Timed out waiting for: ${expression}`);
      await new Promise((resolve) => setTimeout(resolve, intervalMs));
    }
  }

  async navigate(url: string): Promise<void> {
    await this.send("Page.navigate", { url });
  }

  /** Types one character as a trusted key press. */
  async typeCharacter(character: string): Promise<void> {
    await this.send("Input.dispatchKeyEvent", { type: "keyDown", text: character, key: character });
    await this.send("Input.dispatchKeyEvent", { type: "keyUp", key: character });
  }

  async pressKey(key: string, code: string, keyCode: number): Promise<void> {
    const base = { key, code, windowsVirtualKeyCode: keyCode, nativeVirtualKeyCode: keyCode };
    await this.send("Input.dispatchKeyEvent", {
      type: "rawKeyDown",
      ...base,
      ...(key === "Enter" ? { text: "\r" } : {}),
    });
    if (key === "Enter") await this.send("Input.dispatchKeyEvent", { type: "char", text: "\r" });
    await this.send("Input.dispatchKeyEvent", { type: "keyUp", ...base });
  }

  /** A trusted left click at the centre of the element `elementExpression` evaluates to. */
  async click(elementExpression: string): Promise<void> {
    const point = await this.evaluate<{ x: number; y: number } | null>(`(() => {
      const element = ${elementExpression};
      if (!element) return null;
      element.scrollIntoView({ block: "center", inline: "center" });
      const rect = element.getBoundingClientRect();
      return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
    })()`);
    if (point === null) throw new Error(`Nothing to click: ${elementExpression}`);
    const base = { ...point, button: "left", clickCount: 1 };
    await this.send("Input.dispatchMouseEvent", { type: "mouseMoved", ...point });
    await this.send("Input.dispatchMouseEvent", { type: "mousePressed", ...base });
    await this.send("Input.dispatchMouseEvent", { type: "mouseReleased", ...base });
  }

  close(): void {
    this.ws.close();
  }
}

export interface Browser {
  readonly page: CdpPage;
  /** DevTools HTTP port, for attaching another inspector while exploring. */
  readonly debugPort: string;
  readonly close: () => Promise<void>;
}

/**
 * Launches headless Chromium with its own profile under `profileDirectory`. `onSpawn`
 * receives the process's release as soon as it exists, before startup can fail or wait.
 */
export async function launchChromium(
  executable: string,
  profileDirectory: string,
  onSpawn: (release: () => Promise<void>) => void = () => undefined,
  extraArgs: ReadonlyArray<string> = [],
): Promise<Browser> {
  NodeFS.mkdirSync(profileDirectory, { recursive: true });
  const child = NodeChildProcess.spawn(
    executable,
    [
      "--headless=new",
      "--remote-debugging-port=0",
      `--user-data-dir=${profileDirectory}`,
      "--no-first-run",
      "--no-default-browser-check",
      "--disable-background-timer-throttling",
      "--disable-renderer-backgrounding",
      "--window-size=1600,1000",
      ...extraArgs,
      "about:blank",
    ],
    { stdio: ["ignore", "ignore", "pipe"] },
  );
  const stopChild = async () => {
    if (child.exitCode !== null || child.signalCode !== null || child.pid === undefined) return;
    const exited = new Promise((resolve) => child.once("exit", resolve));
    child.kill();
    await exited;
  };
  onSpawn(stopChild);
  const endpoint = await new Promise<string>((resolve, reject) => {
    let output = "";
    child.stderr.on("data", (data: Buffer) => {
      output += data.toString();
      const match = output.match(/DevTools listening on (ws:\/\/[^\s]+)/);
      if (match) resolve(match[1]!);
    });
    child.once("exit", (code) => reject(new Error(`Chromium exited early (${code}): ${output}`)));
    child.once("error", reject);
  });
  const port = new URL(endpoint).port;
  let page: CdpPage;
  try {
    const targets = (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()) as Array<{
      type: string;
      webSocketDebuggerUrl: string;
    }>;
    const target = targets.find((candidate) => candidate.type === "page");
    if (!target) throw new Error("Chromium has no page target");
    page = await CdpPage.connect(target.webSocketDebuggerUrl);
  } catch (error) {
    await stopChild();
    throw error;
  }
  return {
    page,
    debugPort: port,
    close: async () => {
      page.close();
      await stopChild();
      // Chromium's helper processes may still be flushing the profile.
      NodeFS.rmSync(NodePath.resolve(profileDirectory), {
        recursive: true,
        force: true,
        maxRetries: 20,
        retryDelay: 100,
      });
    },
  };
}
