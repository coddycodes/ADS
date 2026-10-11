import * as NodeAssert from "node:assert/strict";
import * as NodeHttp from "node:http";
import * as NodeModule from "node:module";
import * as NodePath from "node:path";
import * as NodeTest from "node:test";
import * as NodeURL from "node:url";
import { normalizeWebDriverRequest } from "../../../desktop/e2e/support/webdriver-request.ts";

const desktopRequire = NodeModule.createRequire(
  NodePath.resolve(import.meta.dirname, "../../../desktop/package.json"),
);
const { remote } = await import(NodeURL.pathToFileURL(desktopRequire.resolve("webdriverio")).href);

NodeTest.test(
  "the real WebDriver SDK creates and closes a session through the owned HTTP endpoint",
  async () => {
    const calls = [];
    const server = NodeHttp.createServer(async (request, response) => {
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      const body = Buffer.concat(chunks).toString();
      calls.push({
        method: request.method,
        path: request.url,
        body: body ? JSON.parse(body) : null,
      });
      response.setHeader("Content-Type", "application/json");
      if (request.method === "POST" && request.url === "/session")
        response.end(
          JSON.stringify({
            value: {
              sessionId: "owned-typography-session",
              capabilities: { browserName: "MiniBrowser" },
            },
          }),
        );
      else if (
        request.method === "GET" &&
        request.url === "/session/owned-typography-session/window"
      )
        response.end(JSON.stringify({ value: "owned-window" }));
      else if (request.method === "DELETE" && request.url === "/session/owned-typography-session")
        response.end(JSON.stringify({ value: null }));
      else {
        response.writeHead(404);
        response.end(
          JSON.stringify({
            value: { error: "unknown command", message: "Owned fixture route refused." },
          }),
        );
      }
    });
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    const address = server.address();
    if (!address || typeof address === "string")
      throw new Error("Owned transport listener missing.");
    try {
      const base = {
        hostname: "127.0.0.1",
        port: address.port,
        connectionRetryTimeout: 5_000,
        connectionRetryCount: 0,
        logLevel: "silent",
        capabilities: { browserName: "MiniBrowser", "wdio:enforceWebDriverClassic": true },
      };
      await NodeAssert.rejects(remote(base), /UND_ERR_INVALID_ARG/);
      NodeAssert.equal(calls.length, 0);
      const browser = await remote({ ...base, transformRequest: normalizeWebDriverRequest });
      NodeAssert.equal(browser.sessionId, "owned-typography-session");
      await browser.deleteSession();
      NodeAssert.deepEqual(
        calls.map(({ method, path }) => ({ method, path })),
        [
          { method: "POST", path: "/session" },
          { method: "GET", path: "/session/owned-typography-session/window" },
          { method: "DELETE", path: "/session/owned-typography-session" },
        ],
      );
      NodeAssert.equal(calls[0].body.capabilities.alwaysMatch.browserName, "MiniBrowser");
    } finally {
      await new Promise((resolve) => server.close(resolve));
    }
  },
);
