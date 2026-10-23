// @effect-diagnostics nodeBuiltinImport:off - Evidence tests write temporary log files.
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";

import { describe, expect, it } from "vite-plus/test";

import { copyAppLogEvidence } from "./app-log-evidence.ts";

const temporaryDirectory = () =>
  NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "bibcode-app-log-evidence-"));

describe("packaged app log evidence", () => {
  it("keeps the end of each app log next to the run's screenshots", () => {
    const stateRoot = temporaryDirectory();
    const artifacts = temporaryDirectory();
    const logs = NodePath.join(stateRoot, "userdata", "logs");
    NodeFS.mkdirSync(logs, { recursive: true });
    NodeFS.writeFileSync(
      NodePath.join(logs, "server.log"),
      "early line\nlate desktop_e2e_page_load_started id=1\n",
    );
    NodeFS.writeFileSync(NodePath.join(logs, "notes.txt"), "not a log");

    const copied = copyAppLogEvidence(stateRoot, artifacts, 20);

    expect(copied).toEqual([NodePath.join(artifacts, "app-server.log")]);
    expect(NodeFS.readFileSync(copied[0]!, "utf8")).toBe("e_load_started id=1\n");
    expect(NodeFS.existsSync(NodePath.join(artifacts, "app-notes.txt"))).toBe(false);
  });

  it("collects nothing, without failing, when the app wrote no logs", () => {
    expect(copyAppLogEvidence(temporaryDirectory(), temporaryDirectory())).toEqual([]);
  });
});
