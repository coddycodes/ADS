import { describe, expect, it } from "vite-plus/test";

import {
  isNetworkLinkHref,
  resolveMarkdownFileLinkMeta,
  resolveMarkdownFileLinkTarget,
  rewriteMarkdownFileUriHref,
} from "./markdown-links";

describe("rewriteMarkdownFileUriHref", () => {
  it("rewrites file uri hrefs into direct path hrefs", () => {
    expect(rewriteMarkdownFileUriHref("file:///Users/julius/project/src/main.ts#L42")).toBe(
      "/Users/julius/project/src/main.ts#L42",
    );
  });

  it("preserves encoded octets so file paths are decoded only once later", () => {
    expect(rewriteMarkdownFileUriHref("file:///Users/julius/project/file%2520name.md")).toBe(
      "/Users/julius/project/file%2520name.md",
    );
  });

  it("normalizes file uri hrefs for windows drive paths", () => {
    expect(
      rewriteMarkdownFileUriHref(
        "file:///D:/Programme/coddy-studio/apps/web/src/components/chat/OpenInPicker.tsx#L69",
      ),
    ).toBe("D:/Programme/coddy-studio/apps/web/src/components/chat/OpenInPicker.tsx#L69");
  });

  it("unwraps angle-bracketed file uri hrefs", () => {
    expect(
      rewriteMarkdownFileUriHref(" <file:///D:/Programme/coddy-studio/apps/web/src/markdown-links.ts> "),
    ).toBe("D:/Programme/coddy-studio/apps/web/src/markdown-links.ts");
  });

  it("rejects absent, malformed, non-file, and pathless destinations", () => {
    expect(rewriteMarkdownFileUriHref(undefined)).toBeNull();
    expect(rewriteMarkdownFileUriHref("not a valid URL")).toBeNull();
    expect(rewriteMarkdownFileUriHref("https://example.com/a.ts")).toBeNull();
    expect(rewriteMarkdownFileUriHref("file:")).toBe("/");
  });
});

describe("resolveMarkdownFileLinkTarget", () => {
  it("resolves absolute posix file paths", () => {
    expect(resolveMarkdownFileLinkTarget("/Users/julius/project/AGENTS.md")).toBe(
      "/Users/julius/project/AGENTS.md",
    );
  });

  it("resolves relative file paths against cwd", () => {
    expect(resolveMarkdownFileLinkTarget("src/processRunner.ts:71", "/Users/julius/project")).toBe(
      "/Users/julius/project/src/processRunner.ts:71",
    );
  });

  it("does not treat filename line references as external schemes", () => {
    expect(resolveMarkdownFileLinkTarget("script.ts:10", "/Users/julius/project")).toBe(
      "/Users/julius/project/script.ts:10",
    );
  });

  it("resolves bare file names against cwd", () => {
    expect(resolveMarkdownFileLinkTarget("AGENTS.md", "/Users/julius/project")).toBe(
      "/Users/julius/project/AGENTS.md",
    );
  });

  it("maps #L line anchors to editor line suffixes", () => {
    expect(resolveMarkdownFileLinkTarget("/Users/julius/project/src/main.ts#L42C7")).toBe(
      "/Users/julius/project/src/main.ts:42:7",
    );
  });

  it("ignores external urls", () => {
    expect(resolveMarkdownFileLinkTarget("https://example.com/docs")).toBeNull();
  });

  it("does not double-decode file URLs", () => {
    expect(resolveMarkdownFileLinkTarget("file:///Users/julius/project/file%2520name.md")).toBe(
      "/Users/julius/project/file%20name.md",
    );
  });

  it("formats tooltip display paths relative to the cwd when possible", () => {
    expect(
      resolveMarkdownFileLinkMeta(
        "file:///C:/Users/mike/dev-stuff/coddy-studio/apps/web/src/session-logic.ts#L501",
        "C:/Users/mike/dev-stuff/coddy-studio",
      ),
    ).toMatchObject({
      displayPath: "coddy-studio/apps/web/src/session-logic.ts:501",
      workspaceRelativePath: "apps/web/src/session-logic.ts",
    });
  });

  it("formats tooltip display paths relative to the cwd for slash-prefixed windows paths", () => {
    expect(
      resolveMarkdownFileLinkMeta(
        "/C:/Users/mike/dev-stuff/coddy-studio/apps/web/src/components/chat/MessagesTimeline.virtualization.browser.tsx",
        "C:/Users/mike/dev-stuff/coddy-studio",
      ),
    ).toMatchObject({
      displayPath:
        "coddy-studio/apps/web/src/components/chat/MessagesTimeline.virtualization.browser.tsx",
      workspaceRelativePath:
        "apps/web/src/components/chat/MessagesTimeline.virtualization.browser.tsx",
    });
  });

  it("does not create a preview path for files outside the workspace", () => {
    expect(resolveMarkdownFileLinkMeta("/tmp/report.ts", "/repo/project")).toMatchObject({
      workspaceRelativePath: null,
    });
  });

  it("normalizes slash-prefixed windows drive paths before resolving", () => {
    expect(
      resolveMarkdownFileLinkTarget(
        "/D:/Programme/coddy-studio/apps/web/src/components/chat/OpenInPicker.tsx#L69",
      ),
    ).toBe("D:/Programme/coddy-studio/apps/web/src/components/chat/OpenInPicker.tsx:69");
  });

  it("resolves angle-bracketed windows drive paths", () => {
    expect(
      resolveMarkdownFileLinkTarget(
        "</D:/Programme/coddy-studio/apps/web/src/components/ChatMarkdown.tsx:1>",
      ),
    ).toBe("D:/Programme/coddy-studio/apps/web/src/components/ChatMarkdown.tsx:1");
  });

  it("does not treat app routes as file links", () => {
    expect(resolveMarkdownFileLinkTarget("/chat/settings")).toBeNull();
  });

  it("handles query strings, hash variants, relative prefixes, and missing cwd", () => {
    expect(resolveMarkdownFileLinkTarget("/tmp/a.ts?raw=1")).toBe("/tmp/a.ts");
    expect(resolveMarkdownFileLinkTarget("/custom/a.ts#section")).toBe("/custom/a.ts");
    expect(resolveMarkdownFileLinkTarget("/chat/settings:12")).toBe("/chat/settings:12");
    expect(resolveMarkdownFileLinkTarget("./src/a.ts", "/repo")).toBe("/repo/./src/a.ts");
    expect(resolveMarkdownFileLinkTarget("src/a.ts")).toBeNull();
    expect(resolveMarkdownFileLinkTarget(undefined)).toBeNull();
    expect(resolveMarkdownFileLinkTarget("   ")).toBeNull();
    expect(resolveMarkdownFileLinkTarget("#L12")).toBeNull();
    expect(resolveMarkdownFileLinkTarget("?raw=1")).toBeNull();
  });

  it("parses optional positions and metadata fallbacks", () => {
    expect(resolveMarkdownFileLinkTarget("/tmp/a.ts#L12")).toBe("/tmp/a.ts:12");
    expect(resolveMarkdownFileLinkMeta("/tmp/a.ts:12:4")).toMatchObject({
      basename: "a.ts",
      workspaceRelativePath: null,
      line: 12,
      column: 4,
    });
    expect(resolveMarkdownFileLinkMeta("/tmp/a.ts")).toMatchObject({
      basename: "a.ts",
      workspaceRelativePath: null,
    });
    expect(resolveMarkdownFileLinkMeta("https://example.com/a.ts")).toBeNull();
  });

  it("preserves malformed percent escapes instead of throwing", () => {
    expect(resolveMarkdownFileLinkTarget("file:///tmp/bad%E0%A4%A.md")).toBe("/tmp/bad%E0%A4%A.md");
  });
});

describe("network link hrefs", () => {
  const networkHrefs = [
    "file:////server/share/x.html",
    "file://server/share/x.html",
    "file://SERVER/share/x.pdf",
    "file:///%5C%5Cserver/share/x.html",
    "file://localhost//server/share/x.html",
    "//server/share/x.html",
    "\\\\server\\share\\x.html",
    "\\\\?\\UNC\\server\\share\\x.html",
    "\\\\.\\C:\\..\\UNC\\server\\share\\x.html",
    "%5C%5Cserver%5Cshare%5Cx.html",
    "<//server/share/x.html>",
  ];

  it.each(networkHrefs)("flags %s as a network link", (href) => {
    expect(isNetworkLinkHref(href)).toBe(true);
  });

  it.each(networkHrefs)("never resolves %s as a file link", (href) => {
    expect(resolveMarkdownFileLinkTarget(href, "/repo")).toBeNull();
    expect(resolveMarkdownFileLinkMeta(href, "/repo")).toBeNull();
    expect(rewriteMarkdownFileUriHref(href)).toBeNull();
  });

  it.each([
    "file:///Users/julius/project/src/main.ts#L42",
    "file://localhost/tmp/a.html",
    "file:///C:/repo/a.html",
    "/Users/julius/project/src/main.ts",
    "src/main.ts:12",
    "https://example.com/docs",
    "#section",
  ])("does not flag %s", (href) => {
    expect(isNetworkLinkHref(href)).toBe(false);
  });

  it("keeps file://localhost links resolving to the local path", () => {
    expect(resolveMarkdownFileLinkTarget("file://localhost/tmp/a.html", "/repo")).toBe(
      "/tmp/a.html",
    );
  });
});
