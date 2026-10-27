import {
  DEFAULT_SERVER_SETTINGS,
  ProviderDriverKind,
  ProviderInstanceId,
  type ProviderInstanceConfig,
  type ProviderSessionDefault,
  type ServerSettings,
} from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";
import {
  buildProviderInstanceUpdatePatch,
  createProviderSessionDefaultsDraft,
} from "./SettingsPanels.logic";

const CODEX = ProviderDriverKind.make("codex");
const CLAUDE = ProviderDriverKind.make("claudeAgent");

function defaultValue(model: string): ProviderSessionDefault {
  return { model };
}

function defaults(
  codex = "codex-initial",
  claude = "claude-initial",
): ServerSettings["providerSessionDefaults"] {
  return {
    [CODEX]: defaultValue(codex),
    [CLAUDE]: defaultValue(claude),
  };
}

describe("createProviderSessionDefaultsDraft", () => {
  it("keeps S2 over an acknowledged S1 snapshot and clears at the exact S2 acknowledgment", () => {
    const initial = defaults();
    const draft = createProviderSessionDefaultsDraft(initial);
    const submittedS1 = draft.submit(CODEX, defaultValue("codex-s1"));
    const submittedS2 = draft.submit(CODEX, defaultValue("codex-s2"));

    expect(draft.reconcile(submittedS1.defaults)).toEqual(submittedS2.defaults);
    expect(draft.reconcile(submittedS1.defaults)).toEqual(submittedS2.defaults);
    expect(draft.reconcile(submittedS2.defaults)).toEqual(submittedS2.defaults);
  });

  it("accepts a rollback or divergent same-driver authority instead of masking it", () => {
    const initial = defaults();
    const rollbackDraft = createProviderSessionDefaultsDraft(initial);
    rollbackDraft.submit(CODEX, defaultValue("codex-pending"));
    expect(rollbackDraft.reconcile(initial)).toEqual(initial);

    const externalDraft = createProviderSessionDefaultsDraft(initial);
    externalDraft.submit(CODEX, defaultValue("codex-pending"));
    const external = defaults("codex-external");
    expect(externalDraft.reconcile(external)).toEqual(external);
  });

  it("accepts a later external update after the latest local edit is acknowledged", () => {
    const draft = createProviderSessionDefaultsDraft(defaults());
    const submitted = draft.submit(CLAUDE, defaultValue("claude-submitted"));

    expect(draft.reconcile(submitted.defaults)).toEqual(submitted.defaults);
    const external = defaults("codex-external", "claude-external");
    expect(draft.reconcile(external)).toEqual(external);
  });

  it("rolls failed S2 back to the acknowledged S1 authority", () => {
    const draft = createProviderSessionDefaultsDraft(defaults());
    const submittedS1 = draft.submit(CODEX, defaultValue("codex-s1"));
    const submittedS2 = draft.submit(CODEX, defaultValue("codex-s2"));
    expect(submittedS1).toEqual(
      expect.objectContaining({ revision: expect.anything(), defaults: expect.anything() }),
    );

    expect(draft.reconcile(submittedS1.defaults)).toEqual(submittedS2.defaults);
    expect(draft.reject(submittedS2.revision)).toEqual(submittedS1.defaults);
    expect(draft.reconcile(submittedS1.defaults)).toEqual(submittedS1.defaults);
  });

  it("keeps S2 pending when the older S1 mutation fails", () => {
    const draft = createProviderSessionDefaultsDraft(defaults());
    const submittedS1 = draft.submit(CODEX, defaultValue("codex-s1"));
    const submittedS2 = draft.submit(CODEX, defaultValue("codex-s2"));
    expect(submittedS2).toEqual(
      expect.objectContaining({ revision: expect.anything(), defaults: expect.anything() }),
    );

    expect(draft.reject(submittedS1.revision)).toEqual(submittedS2.defaults);
    expect(draft.reconcile(submittedS2.defaults)).toEqual(submittedS2.defaults);
  });
});

describe("buildProviderInstanceUpdatePatch", () => {
  it("promotes an edited default provider into providerInstances and resets the legacy provider", () => {
    const instanceId = ProviderInstanceId.make("codex");
    const nextInstance = {
      driver: ProviderDriverKind.make("codex"),
      enabled: true,
      config: {
        binaryPath: "/opt/coddy-studio/codex",
      },
    } satisfies ProviderInstanceConfig;

    const patch = buildProviderInstanceUpdatePatch({
      settings: {
        ...DEFAULT_SERVER_SETTINGS,
        providers: {
          ...DEFAULT_SERVER_SETTINGS.providers,
          codex: {
            ...DEFAULT_SERVER_SETTINGS.providers.codex,
            binaryPath: "/legacy/codex",
          },
        },
      },
      instanceId,
      instance: nextInstance,
      driver: ProviderDriverKind.make("codex"),
      isDefault: true,
    });

    expect(patch.providerInstances?.[instanceId]).toEqual(nextInstance);
    expect(patch.providers?.codex).toEqual(DEFAULT_SERVER_SETTINGS.providers.codex);
  });

  it("updates custom instances without touching legacy provider settings", () => {
    const instanceId = ProviderInstanceId.make("codex_personal");
    const nextInstance = {
      driver: ProviderDriverKind.make("codex"),
      enabled: true,
      config: {
        homePath: "/Users/example/.codex-personal",
      },
    } satisfies ProviderInstanceConfig;

    const patch = buildProviderInstanceUpdatePatch({
      settings: DEFAULT_SERVER_SETTINGS,
      instanceId,
      instance: nextInstance,
      driver: ProviderDriverKind.make("codex"),
      isDefault: false,
    });

    expect(patch.providerInstances?.[instanceId]).toEqual(nextInstance);
    expect(patch.providers).toBeUndefined();
  });
});
