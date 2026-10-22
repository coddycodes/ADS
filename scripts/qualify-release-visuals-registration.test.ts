// @effect-diagnostics nodeBuiltinImport:off - Static registration policy never dispatches or starts the qualification runtime.
import * as NodeFS from "node:fs";
import * as YAML from "yaml";
import { describe, expect, it } from "vite-plus/test";

const workflow = YAML.parse(
  NodeFS.readFileSync(
    new URL("../.github/workflows/qualify-release-visuals.yml", import.meta.url),
    "utf8",
  ),
);

describe("visual qualification dispatch registration", () => {
  it("registers only manual dispatch with the existing three fixed scene choices", () => {
    expect(workflow.name).toBe("First release visual batch (manual preparation lane)");
    expect(Object.keys(workflow.on)).toEqual(["workflow_dispatch"]);
    expect(workflow.on.workflow_dispatch.inputs).toEqual({
      scene_selection: {
        description: "Fixed visual preparation selection",
        type: "choice",
        required: true,
        default: "release-visual-core",
        options: ["release-visual-core", "release-visual-settings", "release-visual-git-project"],
      },
    });
  });

  it("keeps both workflow versions read-only and requires one explicit mode", () => {
    expect(workflow.permissions).toEqual({ contents: "read" });
    const registration = workflow.jobs.registration_only;
    expect(Object.keys(workflow.jobs)).toEqual([
      registration ? "registration_only" : "visual_core",
    ]);
    const job = registration ?? workflow.jobs.visual_core;
    expect(job.permissions).toBeUndefined();
    expect(job.strategy).toBeUndefined();
    if (registration) {
      expect(job.env).toBeUndefined();
      expect(job.steps).toHaveLength(1);
      expect(job.steps[0].uses).toBeUndefined();
      expect(job.steps[0].env).toBeUndefined();
    } else {
      expect(job["runs-on"]).toBe("ubuntu-22.04");
      expect(job["timeout-minutes"]).toBe(120);
      expect(job.env).toEqual({ CARGO_BUILD_JOBS: "2", CARGO_PROFILE_DEV_DEBUG: "0" });
    }
  });

  it("refuses the registration stub or preserves the nominated ref's full owner gates", () => {
    const registration = workflow.jobs.registration_only;
    const job = registration ?? workflow.jobs.visual_core;
    expect(job.if).toBeUndefined();
    expect(job["continue-on-error"]).toBeUndefined();
    if (registration) {
      const refusal = job.steps[0];
      expect(refusal.if).toBeUndefined();
      expect(refusal["continue-on-error"]).toBeUndefined();
      expect(refusal.shell).toBe("bash");
      expect(refusal.run.trim()).toBe(
        "printf '%s\\n' 'QUALIFICATION_REQUIRES_NOMINATED_QA_REF'\nexit 1",
      );
    } else {
      const steps = job.steps as Array<{
        name?: string;
        uses?: string;
        run?: string;
        if?: string;
        with?: { ref?: string; "persist-credentials"?: boolean; "if-no-files-found"?: string };
      }>;
      const checkout = steps.find((step) => step.uses?.startsWith("actions/checkout@"))!;
      expect(checkout.uses).toBe("actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1");
      expect(checkout.with?.ref).toBeUndefined();
      expect(checkout.with?.["persist-credentials"]).toBe(false);
      const admission = steps.find(
        (step) => step.name === "Check owned helpers and namespace admission",
      )!;
      expect(admission.run).toContain("python3 -B scripts/qualify-chat-uploads.py preflight");
      expect(admission.run).toContain("scripts/qualify-release-visuals-workflow.test.ts");
      expect(steps.find((step) => step.name === "Build guarded production CLI")?.run).toBe(
        "cargo build -p bibcode-server --bin bibcode --features hermetic-test-guard -j 2",
      );
      const selections = [
        ["Run contained first visual batch", "release-visual-core"],
        ["Run contained settings visual batch", "release-visual-settings"],
        ["Run contained Git/project visual batch", "release-visual-git-project"],
      ];
      for (const [name, selection] of selections) {
        const run = steps.find((step) => step.name === name)!;
        expect(run.run).toBe(`python3 -B scripts/qualify-chat-uploads.py --scenario ${selection}`);
        expect(run.if).toBe(
          selection === "release-visual-core"
            ? "${{ inputs.scene_selection == 'release-visual-core' || inputs.scene_selection == '' }}"
            : `\${{ inputs.scene_selection == '${selection}' }}`,
        );
      }
      const evidence = steps.filter((step) => step.name?.startsWith("Retain explicit"));
      expect(evidence).toHaveLength(3);
      for (const step of evidence) {
        expect(step.uses).toBe("actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9");
        expect(step.with?.["if-no-files-found"]).toBe("error");
      }
    }
  });
});
