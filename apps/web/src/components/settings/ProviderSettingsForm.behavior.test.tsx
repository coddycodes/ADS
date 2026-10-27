// @vitest-environment happy-dom

import { makeProviderSettingsSchema, ProviderDriverKind } from "@coddy/contracts";
import * as Schema from "effect/Schema";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { ProviderSettingsForm } from "./ProviderSettingsForm";
import { DRIVER_OPTION_BY_VALUE, type ProviderClientDefinition } from "./providerDriverMeta";

let root: Root | null = null;
let container: HTMLDivElement | null = null;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

afterEach(async () => {
  if (root) {
    await act(async () => root?.unmount());
  }
  container?.remove();
  root = null;
  container = null;
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

async function mount(element: React.ReactNode): Promise<HTMLDivElement> {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () => root?.render(element));
  return container;
}

function setInputValue(input: HTMLInputElement | HTMLTextAreaElement, value: string): void {
  const descriptor = Object.getOwnPropertyDescriptor(
    input instanceof HTMLTextAreaElement
      ? HTMLTextAreaElement.prototype
      : HTMLInputElement.prototype,
    "value",
  );
  descriptor?.set?.call(input, value);
  input.dispatchEvent(new Event("input", { bubbles: true }));
  input.dispatchEvent(new Event("change", { bubbles: true }));
}

const customDefinition = {
  value: ProviderDriverKind.make("custom"),
  label: "Custom",
  icon: () => null,
  settingsSchema: makeProviderSettingsSchema(
    {
      notes: Schema.String.pipe(
        Schema.annotateKey({
          title: "Notes",
          description: "Long-form notes.",
          providerSettingsForm: { control: "textarea", clearWhenEmpty: "persist" },
        }),
      ),
      enabled: Schema.Boolean.pipe(
        Schema.annotateKey({
          title: "Enabled",
          providerSettingsForm: { control: "switch", clearWhenEmpty: "persist" },
        }),
      ),
    },
    { order: ["notes", "enabled"] },
  ),
} satisfies ProviderClientDefinition;

describe("ProviderSettingsForm behavior", () => {
  it("edits text and password fields in dialog mode", async () => {
    const definition = DRIVER_OPTION_BY_VALUE[ProviderDriverKind.make("opencode")]!;
    const onChange = vi.fn();
    const mounted = await mount(
      <ProviderSettingsForm
        definition={definition}
        value={{ serverUrl: "http://old", serverPassword: "secret" }}
        idPrefix="provider"
        variant="dialog"
        onChange={onChange}
      />,
    );

    const serverUrl = mounted.querySelector<HTMLInputElement>("#provider-serverUrl")!;
    const password = mounted.querySelector<HTMLInputElement>("#provider-serverPassword")!;
    expect(password.type).toBe("password");
    expect(password.autocomplete).toBe("off");
    await act(async () => setInputValue(serverUrl, "http://new"));
    await act(async () => setInputValue(password, "new-secret"));

    expect(onChange).toHaveBeenCalledWith(
      expect.objectContaining({ serverUrl: "http://new", serverPassword: "secret" }),
    );
    expect(onChange).toHaveBeenCalledWith(
      expect.objectContaining({ serverUrl: "http://old", serverPassword: "new-secret" }),
    );
  });

  it("edits textarea and switch controls with descriptions", async () => {
    const onChange = vi.fn();
    const mounted = await mount(
      <ProviderSettingsForm
        definition={customDefinition}
        value={{ notes: "old", enabled: false }}
        idPrefix="custom"
        variant="card"
        onChange={onChange}
      />,
    );
    expect(mounted.textContent).toContain("Long-form notes.");
    const textarea = mounted.querySelector<HTMLTextAreaElement>("#custom-notes")!;
    await act(async () => setInputValue(textarea, "new notes"));
    await act(async () => mounted.querySelector<HTMLElement>('[role="switch"]')!.click());

    expect(onChange).toHaveBeenCalledWith({ notes: "new notes", enabled: false });
    expect(onChange).toHaveBeenCalledWith({ notes: "old", enabled: true });
  });

  it("renders card text inputs and returns null for an all-hidden schema", async () => {
    const definition = DRIVER_OPTION_BY_VALUE[ProviderDriverKind.make("codex")]!;
    const mounted = await mount(
      <ProviderSettingsForm
        definition={definition}
        value={null}
        idPrefix="codex"
        variant="card"
        onChange={() => {}}
      />,
    );
    expect(mounted.querySelector("#codex-binaryPath")).not.toBeNull();
    expect(mounted.querySelector(".border-t")).not.toBeNull();

    const hiddenDefinition = {
      ...customDefinition,
      settingsSchema: Schema.Struct({
        hidden: Schema.String.pipe(Schema.annotateKey({ providerSettingsForm: { hidden: true } })),
      }),
    } satisfies ProviderClientDefinition;
    await act(async () =>
      root?.render(
        <ProviderSettingsForm
          definition={hiddenDefinition}
          value={undefined}
          idPrefix="hidden"
          variant="dialog"
          onChange={() => {}}
        />,
      ),
    );
    expect(mounted.innerHTML).toBe("");
  });
});
