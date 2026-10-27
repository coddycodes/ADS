// @effect-diagnostics nodeBuiltinImport:off - This contract test reads the real packaging configuration.
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";

import { expect, it } from "vite-plus/test";

it("packages only the executable, web client, guide, and license", () => {
  const source = NodeFS.readFileSync(
    NodePath.resolve(import.meta.dirname, "../apps/server/package/nfpm.yaml"),
    "utf8",
  );

  expect(source).toContain("name: coddy-studio-server");
  expect(source).toContain("dst: /usr/bin/coddy-studio");
  expect(source).toContain("dst: /usr/share/coddy-studio/web");
  expect(source).toContain("dst: /usr/share/doc/coddy-studio-server/README.md");
  expect(source).toContain("dst: /usr/share/doc/coddy-studio-server/LICENSE");
  expect(source).not.toMatch(/scripts:|systemd|firewall|useradd|groupadd/);
});
