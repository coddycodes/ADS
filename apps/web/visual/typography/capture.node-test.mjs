import * as NodeAssert from "node:assert/strict";
import * as NodeFSP from "node:fs/promises";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as NodeTest from "node:test";
import { captureFixtures, serveGallery } from "./capture.mjs";

NodeTest.test(
  "the complete capture caller serves its own build, writes both source-joined observations and cleans up",
  async () => {
    const root = await NodeFSP.mkdtemp(
      NodePath.join(NodeOS.tmpdir(), "bibcode-typography-caller-"),
    );
    let deleted = false;
    try {
      await NodeFSP.writeFile(NodePath.join(root, "index.html"), "<p>Owned gallery build</p>");
      const gallery = await serveGallery(root);
      const url = gallery.url;
      NodeAssert.equal(await (await fetch(url)).text(), "<p>Owned gallery build</p>");
      NodeAssert.equal((await fetch(`${url}/missing`)).status, 404);
      let theme = "light";
      const screenshots = [];
      const browser = {
        setWindowSize: async () => undefined,
        setTimeout: async () => undefined,
        url: async (next) => {
          theme = new URL(next).searchParams.get("theme");
        },
        waitUntil: async (predicate) => NodeAssert.equal(await predicate(), true),
        executeAsync: async () => true,
        execute: async (fn, argument) => {
          if (argument) return true;
          if (String(fn).includes("JSON.parse"))
            return [
              {
                id: "badge",
                modules: ["components/ui/badge.tsx"],
                owners: ["Badge"],
                anchors: ["Small unread badge"],
              },
            ];
          if (String(fn).includes("viewport"))
            return {
              fixture: "badge",
              status: "mounted",
              dark: theme === "dark",
              viewport: { width: 1440, height: 1200, devicePixelRatio: 1 },
              fonts: { dmSans: true, jetbrains: true },
            };
          return true;
        },
        saveScreenshot: async (path) => {
          screenshots.push(path);
          const bytes = Buffer.from("owned fake capture transport");
          await NodeFSP.writeFile(path, bytes);
          return bytes;
        },
        deleteSession: async () => {
          deleted = true;
        },
      };
      const output = NodePath.join(root, "output");
      const source = "a".repeat(40);
      const results = await captureFixtures({
        browser,
        gallery,
        output,
        source,
        engine: "fake-transport",
      });
      NodeAssert.deepEqual(
        results.map(({ id, theme, source }) => ({ id, theme, source })),
        [
          { id: "badge", theme: "light", source },
          { id: "badge", theme: "dark", source },
        ],
      );
      NodeAssert.equal(screenshots.length, 2);
      NodeAssert.equal(results[0].modules["components/ui/badge.tsx"].length, 64);
      NodeAssert.equal(
        JSON.parse(await NodeFSP.readFile(NodePath.join(output, "manifest.json"), "utf8"))
          .observations.length,
        2,
      );
      NodeAssert.equal(deleted, true);
      await NodeAssert.rejects(fetch(url));
    } finally {
      await NodeFSP.rm(root, { recursive: true, force: true });
    }
  },
);

NodeTest.test(
  "a failed browser preparation still closes its session and owned gallery",
  async () => {
    let closed = false;
    let deleted = false;
    const root = await NodeFSP.mkdtemp(
      NodePath.join(NodeOS.tmpdir(), "bibcode-typography-failure-"),
    );
    try {
      await NodeAssert.rejects(
        captureFixtures({
          browser: {
            setWindowSize: async () => {
              throw new Error("Owned preparation failure");
            },
            deleteSession: async () => {
              deleted = true;
            },
          },
          gallery: {
            url: "http://127.0.0.1:1",
            close: async () => {
              closed = true;
            },
          },
          output: root,
          source: "a".repeat(40),
          engine: "fake-transport",
        }),
        /Owned preparation failure/,
      );
      NodeAssert.equal(deleted, true);
      NodeAssert.equal(closed, true);
    } finally {
      await NodeFSP.rm(root, { recursive: true, force: true });
    }
  },
);

NodeTest.test(
  "a refused state preserves later accepted captures and remains incomplete",
  async () => {
    const root = await NodeFSP.mkdtemp(
      NodePath.join(NodeOS.tmpdir(), "bibcode-typography-partial-"),
    );
    let theme = "light";
    let closed = false;
    let deleted = false;
    const browser = {
      setWindowSize: async () => undefined,
      setTimeout: async () => undefined,
      url: async (url) => {
        theme = new URL(url).searchParams.get("theme");
      },
      waitUntil: async (predicate) => NodeAssert.equal(await predicate(), true),
      executeAsync: async () => true,
      execute: async (fn, argument) => {
        if (argument) return true;
        if (String(fn).includes("JSON.parse"))
          return [
            {
              id: "badge",
              modules: ["components/ui/badge.tsx"],
              owners: ["Badge"],
              anchors: ["Ready"],
            },
          ];
        if (String(fn).includes("viewport"))
          return {
            fixture: "badge",
            status: "mounted",
            dark: theme === "dark",
            viewport: { width: 1440, height: 1200, devicePixelRatio: 1 },
            fonts: { dmSans: true, jetbrains: true },
          };
        return true;
      },
      saveScreenshot: async (path) => {
        const data = Buffer.from("owned fake capture transport");
        await NodeFSP.writeFile(path, data);
        return data;
      },
      deleteSession: async () => {
        deleted = true;
      },
    };
    try {
      await NodeAssert.rejects(
        captureFixtures({
          browser,
          gallery: {
            url: "http://127.0.0.1:1",
            close: async () => {
              closed = true;
            },
          },
          output: root,
          source: "a".repeat(40),
          engine: "fake-transport",
          interact: async () => {
            if (theme === "light") throw new Error("Owned interaction refusal.");
          },
        }),
        /refused 1 states/,
      );
      const manifest = JSON.parse(
        await NodeFSP.readFile(NodePath.join(root, "manifest.json"), "utf8"),
      );
      NodeAssert.equal(manifest.complete, false);
      NodeAssert.deepEqual(manifest.failures, [
        {
          id: "badge",
          state: "initial",
          theme: "light",
          status: "capture-refused",
          stage: "interaction",
        },
      ]);
      NodeAssert.deepEqual(
        manifest.observations.map(({ theme, filename }) => ({ theme, filename })),
        [{ theme: "dark", filename: "badge-initial-dark.png" }],
      );
      NodeAssert.equal(deleted, true);
      NodeAssert.equal(closed, true);
    } finally {
      await NodeFSP.rm(root, { recursive: true, force: true });
    }
  },
);
