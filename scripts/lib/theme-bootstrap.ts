// @effect-diagnostics nodeBuiltinImport:off - Vite versions a static bootstrap before an Effect runtime exists.
import * as NodeCrypto from "node:crypto";
import * as NodeFS from "node:fs";
import type { Plugin } from "vite-plus";

const bootstrapPath = new URL("../../apps/web/public/theme-bootstrap.js", import.meta.url);

export function assertNoInlineScripts(html: string): void {
  const scripts = html.matchAll(
    /<script(?=[\s/>])((?:[^>"']|"[^"]*"|'[^']*')*)>([\s\S]*?)<\/script\s*>/gi,
  );
  for (const [tag, attributes = "", body = ""] of scripts) {
    const hasSrc = /\ssrc(?:\s|=|$)/i.test(attributes.replace(/"[^"]*"|'[^']*'/g, ""));
    if (!hasSrc || body.trim() !== "") {
      throw new Error(`Inline script is not allowed in built index HTML: ${tag.slice(0, 80)}`);
    }
  }
}

export function versionThemeBootstrap(html: string, source: string): string {
  const scriptSrc = /(<script\b[^>]*\ssrc\s*=\s*)(["'])\/theme-bootstrap\.js\2(?=[\s>])/gi;
  const count = Array.from(html.matchAll(scriptSrc)).length;
  if (count !== 1) {
    throw new Error(
      `Expected exactly one script with src="/theme-bootstrap.js" in index HTML; found ${count}`,
    );
  }

  const version = NodeCrypto.createHash("sha256").update(source).digest("hex").slice(0, 12);
  return html.replace(scriptSrc, `$1$2/theme-bootstrap.js?v=${version}$2`);
}

export function themeBootstrapVersionPlugin() {
  const transformIndexHtml = (html: string) =>
    versionThemeBootstrap(html, NodeFS.readFileSync(bootstrapPath, "utf8"));

  return {
    name: "coddy-studio-theme-bootstrap-version",
    transformIndexHtml: {
      order: "pre",
      handler: transformIndexHtml,
    },
  } satisfies Plugin;
}

export function themeBootstrapPlugins() {
  return [
    themeBootstrapVersionPlugin(),
    {
      name: "coddy-studio-no-inline-scripts",
      apply: "build",
      transformIndexHtml: {
        order: "post",
        handler: assertNoInlineScripts,
      },
    },
  ] satisfies Plugin[];
}
