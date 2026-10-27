// @vitest-environment happy-dom

import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import {
  ProviderDriverKind,
  type ProviderSessionDefault,
  type ServerProviderModel,
} from "@coddy/contracts";

const controls = vi.hoisted(() => {
  const state = {
    entries: [] as Array<{
      readonly kind: "Select" | "Switch";
      readonly props: Record<string, unknown>;
    }>,
    reset() {
      state.entries.length = 0;
    },
  };
  return state;
});

vi.mock("../ui/select", async () => {
  const React = await import("react");
  const SelectContext = React.createContext<Record<string, unknown>>({});
  const Wrapper = ({ children }: { readonly children?: ReactNode }) => <>{children}</>;
  return {
    Select: ({
      children,
      ...props
    }: { readonly children?: ReactNode } & Record<string, unknown>) => {
      controls.entries.push({ kind: "Select", props });
      return <SelectContext.Provider value={props}>{children}</SelectContext.Provider>;
    },
    SelectItem: Wrapper,
    SelectPopup: Wrapper,
    SelectTrigger: ({
      children,
      ...props
    }: { readonly children?: ReactNode } & Record<string, unknown>) => {
      const selectProps = React.useContext(SelectContext);
      return (
        <button
          aria-label={props["aria-label"] as string | undefined}
          disabled={selectProps.disabled === true}
          id={props.id as string | undefined}
          type="button"
        >
          {children}
        </button>
      );
    },
    SelectValue: ({ children }: { readonly children?: ReactNode }) => {
      const selectProps = React.useContext(SelectContext);
      return <span>{children ?? (selectProps.value as ReactNode)}</span>;
    },
  };
});

vi.mock("../ui/switch", () => ({
  Switch: (props: Record<string, unknown>) => {
    controls.entries.push({ kind: "Switch", props });
    return (
      <button
        aria-label={props["aria-label"] as string | undefined}
        aria-checked={props.checked as boolean | undefined}
        disabled={props.disabled as boolean | undefined}
        id={props.id as string | undefined}
        type="button"
      />
    );
  },
}));

import { ProviderSessionDefaultsControls } from "./ProviderSessionDefaultsControls";

const CODEX = ProviderDriverKind.make("codex");
const OPENCODE = ProviderDriverKind.make("opencode");

let root: Root | null = null;
let container: HTMLDivElement | null = null;

function model(
  slug: string,
  optionDescriptors: NonNullable<ServerProviderModel["capabilities"]>["optionDescriptors"] = [],
): ServerProviderModel {
  return {
    slug,
    name: slug === "gpt-rich" ? "GPT Rich" : slug,
    isCustom: false,
    capabilities: { optionDescriptors },
  };
}

function namedModel(slug: string, name: string): ServerProviderModel {
  return { ...model(slug), name };
}

const richModels = [
  model("gpt-rich", [
    {
      id: "variant",
      label: "Variant",
      type: "select",
      options: [{ id: "max", label: "Max", isDefault: true }],
    },
    {
      id: "agent",
      label: "Agent",
      type: "select",
      options: [{ id: "build", label: "Build", isDefault: true }],
    },
    {
      id: "contextWindow",
      label: "Context window",
      type: "select",
      options: [{ id: "1m", label: "1M", isDefault: true }],
    },
    {
      id: "reasoning",
      label: "Reasoning",
      type: "select",
      options: [
        { id: "low", label: "Low" },
        { id: "high", label: "High", isDefault: true },
      ],
    },
    { id: "fastMode", label: "Fast", type: "boolean", currentValue: false },
  ]),
  model("gpt-next", [
    {
      id: "reasoning",
      label: "Reasoning",
      type: "select",
      options: [
        { id: "low", label: "Low" },
        { id: "high", label: "High", isDefault: true },
      ],
    },
    { id: "fastMode", label: "Fast", type: "boolean", currentValue: false },
  ]),
] satisfies ReadonlyArray<ServerProviderModel>;

const richValue: ProviderSessionDefault = {
  model: "gpt-rich",
  options: [
    { id: "reasoning", value: "high" },
    { id: "fastMode", value: false },
  ],
};

const stableCodexModels = [
  model("gpt-rich", [
    {
      id: "reasoningEffort",
      label: "Reasoning",
      type: "select",
      options: [
        { id: "medium", label: "Medium", isDefault: true },
        { id: "high", label: "High" },
      ],
      currentValue: "medium",
    },
    {
      id: "serviceTier",
      label: "Service tier",
      type: "select",
      options: [
        { id: "default", label: "Standard", isDefault: true },
        { id: "fast", label: "Fast" },
      ],
      currentValue: "default",
    },
  ]),
] satisfies ReadonlyArray<ServerProviderModel>;

const stableCodexValue: ProviderSessionDefault = {
  model: "gpt-rich",
  options: [
    { id: "reasoningEffort", value: "high" },
    { id: "serviceTier", value: "fast" },
  ],
};

type Props = Parameters<typeof ProviderSessionDefaultsControls>[0];

function baseProps(overrides: Partial<Props> = {}): Props {
  return {
    driver: CODEX,
    models: richModels,
    value: richValue,
    disabled: false,
    onChange: vi.fn(),
    ...overrides,
  };
}

function render(props: Props): string {
  controls.reset();
  return renderToStaticMarkup(<ProviderSessionDefaultsControls {...props} />);
}

async function mount(props: Props): Promise<HTMLDivElement> {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () => root?.render(<ProviderSessionDefaultsControls {...props} />));
  return container;
}

async function rerender(props: Props): Promise<void> {
  await act(async () => root?.render(<ProviderSessionDefaultsControls {...props} />));
}

function controlElement(mounted: HTMLDivElement, label: string): HTMLButtonElement {
  const element = mounted.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`);
  if (element === null) throw new Error(`Expected ${label} control.`);
  return element;
}

function entries(kind: "Select" | "Switch"): ReadonlyArray<Record<string, unknown>> {
  return controls.entries.filter((entry) => entry.kind === kind).map((entry) => entry.props);
}

function attributeValues(markup: string, attribute: string): ReadonlyArray<string> {
  return [...markup.matchAll(new RegExp(`\\s${attribute}="([^"]+)"`, "g"))].map(
    (match) => match[1]!,
  );
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  controls.reset();
});

afterEach(async () => {
  if (root) await act(async () => root?.unmount());
  container?.remove();
  root = null;
  container = null;
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("ProviderSessionDefaultsControls", () => {
  it("renders accessible model, effort, and fast controls in order", () => {
    const markup = render(baseProps());

    expect(markup).toContain("Default model");
    expect(markup).toContain("Default effort");
    expect(markup).toContain("Fast by default");
    expect(controls.entries.map((entry) => entry.kind)).toEqual(["Select", "Select", "Switch"]);
    expect(markup).toContain('aria-label="Default model"');
    expect(markup).toContain('aria-label="Default effort"');
    expect(attributeValues(markup, "for")).toEqual(attributeValues(markup, "id"));
    expect(entries("Switch")[0]?.["aria-label"]).toBe("Fast by default");
  });

  it("disables Codex Fast when rich metadata omits serviceTier", () => {
    const markup = render(
      baseProps({
        models: [
          model("gpt-rich", [
            {
              id: "reasoningEffort",
              label: "Reasoning",
              type: "select",
              options: [
                { id: "medium", label: "Medium", isDefault: true },
                { id: "high", label: "High" },
              ],
              currentValue: "medium",
            },
          ]),
        ],
        value: {
          model: "gpt-rich",
          options: [
            { id: "reasoningEffort", value: "high" },
            { id: "serviceTier", value: "fast" },
          ],
        },
      }),
    );

    expect(markup).toContain("Fast by default");
    expect(entries("Switch")[0]).toMatchObject({
      "aria-label": "Fast by default",
      checked: false,
      disabled: true,
    });
  });

  it("renders Codex Fast by default when live serviceTier only contains Standard", () => {
    const markup = render(
      baseProps({
        models: [
          model("gpt-rich", [
            {
              id: "reasoningEffort",
              label: "Reasoning",
              type: "select",
              options: [{ id: "medium", label: "Medium", isDefault: true }],
              currentValue: "medium",
            },
            {
              id: "serviceTier",
              label: "Live service tier",
              type: "select",
              options: [{ id: "default", label: "Live Standard", isDefault: true }],
              currentValue: "default",
            },
          ]),
        ],
        value: {
          model: "gpt-rich",
          options: [{ id: "serviceTier", value: "default" }],
        },
      }),
    );

    expect(markup).toContain("Fast by default");
    expect(entries("Switch")[0]).toMatchObject({
      "aria-label": "Fast by default",
      checked: false,
    });
  });

  it("gives each rendered defaults row unique control ids and local label targets", () => {
    const markup = renderToStaticMarkup(
      <>
        <section data-row="first">
          <ProviderSessionDefaultsControls {...baseProps()} />
        </section>
        <section data-row="second">
          <ProviderSessionDefaultsControls {...baseProps()} />
        </section>
      </>,
    );
    const firstRow = markup.match(/<section data-row="first">([\s\S]*?)<\/section>/)?.[1];
    const secondRow = markup.match(/<section data-row="second">([\s\S]*?)<\/section>/)?.[1];

    expect(firstRow).toBeDefined();
    expect(secondRow).toBeDefined();

    const firstIds = attributeValues(firstRow!, "id");
    const secondIds = attributeValues(secondRow!, "id");
    const allIds = [...firstIds, ...secondIds];

    expect(new Set(allIds)).toHaveLength(allIds.length);
    expect(attributeValues(firstRow!, "for").every((id) => firstIds.includes(id))).toBe(true);
    expect(attributeValues(secondRow!, "for").every((id) => secondIds.includes(id))).toBe(true);
  });

  it("disables every available control when the provider is disabled", () => {
    render(baseProps({ disabled: true }));

    expect(entries("Select").every((entry) => entry.disabled === true)).toBe(true);
    expect(entries("Switch")[0]?.disabled).toBe(true);
  });

  it("keeps effort and fast controls visible but disabled when unsupported", () => {
    const markup = render(
      baseProps({
        driver: OPENCODE,
        models: [
          model("plain", [
            {
              id: "variant",
              label: "Variant",
              type: "select",
              options: [{ id: "max", label: "Max", isDefault: true }],
            },
            {
              id: "agent",
              label: "Agent",
              type: "select",
              options: [{ id: "build", label: "Build", isDefault: true }],
            },
            {
              id: "contextWindow",
              label: "Context window",
              type: "select",
              options: [{ id: "1m", label: "1M", isDefault: true }],
            },
          ]),
        ],
        value: { model: "plain" },
      }),
    );

    expect(markup).toContain("Default model");
    expect(markup).toContain("Default effort");
    expect(markup).toContain("Fast by default");
    expect(markup).toContain("Not supported");
    expect(controls.entries.map((entry) => entry.kind)).toEqual(["Select", "Select", "Switch"]);
    expect(entries("Select")[0]?.disabled).toBe(false);
    expect(entries("Select")[1]?.disabled).toBe(true);
    expect(entries("Switch")[0]).toMatchObject({ checked: false, disabled: true });
  });

  it("disables the model control when no provider models are available", () => {
    render(baseProps({ driver: OPENCODE, models: [], value: { model: "auto" } }));

    expect(entries("Select")[0]?.disabled).toBe(true);
    expect(entries("Select")[1]?.disabled).toBe(true);
    expect(entries("Switch")[0]?.disabled).toBe(true);
  });

  it("keeps the full control row mounted when model capabilities change", async () => {
    const mounted = await mount(
      baseProps({ driver: OPENCODE, models: richModels, value: richValue }),
    );
    const modelControl = controlElement(mounted, "Default model");
    const effortControl = controlElement(mounted, "Default effort");
    const fastControl = controlElement(mounted, "Fast by default");

    expect(effortControl.disabled).toBe(false);
    expect(fastControl.disabled).toBe(false);

    await rerender(
      baseProps({ driver: OPENCODE, models: [model("plain")], value: { model: "plain" } }),
    );

    expect(controlElement(mounted, "Default model")).toBe(modelControl);
    expect(controlElement(mounted, "Default effort")).toBe(effortControl);
    expect(controlElement(mounted, "Fast by default")).toBe(fastControl);
    expect(effortControl.disabled).toBe(true);
    expect(effortControl.textContent).toBe("Not supported");
    expect(fastControl.disabled).toBe(true);
  });

  it("keeps Codex controls mounted and disables unverified Fast during empty discovery", async () => {
    const mounted = await mount(baseProps({ models: stableCodexModels, value: stableCodexValue }));
    const modelControl = controlElement(mounted, "Default model");
    const effortControl = controlElement(mounted, "Default effort");
    const fastControl = controlElement(mounted, "Fast by default");

    await rerender(baseProps({ models: [], value: stableCodexValue }));

    expect(controlElement(mounted, "Default model")).toBe(modelControl);
    expect(controlElement(mounted, "Default effort")).toBe(effortControl);
    expect(controlElement(mounted, "Fast by default")).toBe(fastControl);
    expect(modelControl.isConnected).toBe(true);
    expect(effortControl.isConnected).toBe(true);
    expect(fastControl.isConnected).toBe(true);
    expect(modelControl.textContent).toBe("GPT Rich");
    expect(effortControl.textContent).toBe("high");
    expect(fastControl.getAttribute("aria-checked")).toBe("false");
    expect(fastControl.disabled).toBe(true);
  });

  it("keeps Codex controls mounted while provider interactivity changes", async () => {
    const mounted = await mount(
      baseProps({ models: stableCodexModels, value: stableCodexValue, disabled: true }),
    );
    const modelControl = controlElement(mounted, "Default model");
    const effortControl = controlElement(mounted, "Default effort");
    const fastControl = controlElement(mounted, "Fast by default");

    expect([modelControl, effortControl, fastControl].every((control) => control.disabled)).toBe(
      true,
    );

    await rerender(
      baseProps({ models: stableCodexModels, value: stableCodexValue, disabled: false }),
    );

    expect(controlElement(mounted, "Default model")).toBe(modelControl);
    expect(controlElement(mounted, "Default effort")).toBe(effortControl);
    expect(controlElement(mounted, "Fast by default")).toBe(fastControl);
    expect([modelControl, effortControl, fastControl].every((control) => control.isConnected)).toBe(
      true,
    );
    expect(modelControl.textContent).toBe("GPT Rich");
    expect(effortControl.textContent).toBe("high");
    expect(fastControl.getAttribute("aria-checked")).toBe("true");
    expect([modelControl, effortControl, fastControl].every((control) => !control.disabled)).toBe(
      true,
    );
  });

  it("keeps committed model labels isolated by driver and slug", async () => {
    const mounted = await mount(
      baseProps({ models: [namedModel("first", "First Codex")], value: { model: "first" } }),
    );
    const modelControl = controlElement(mounted, "Default model");

    expect(modelControl.textContent).toBe("First Codex");

    await rerender(
      baseProps({ models: [namedModel("second", "Second Codex")], value: { model: "second" } }),
    );
    expect(modelControl.textContent).toBe("Second Codex");

    await rerender(baseProps({ models: [], value: { model: "second" } }));
    expect(modelControl.textContent).toBe("Second Codex");

    await rerender(baseProps({ driver: OPENCODE, models: [], value: { model: "second" } }));
    expect(modelControl.textContent).toBe("second");
  });

  it("keeps an unavailable saved model selectable so the user can recover", () => {
    const onChange = vi.fn();
    const markup = render(
      baseProps({
        models: [richModels[0]!],
        value: { model: "private-model" },
        onChange,
      }),
    );

    expect(markup).toContain("private-model");
    expect(markup).toContain("Unavailable here; new sessions will use gpt-rich.");
    expect(entries("Select")[0]).toMatchObject({ disabled: false, value: "private-model" });

    (entries("Select")[0]!.onValueChange as (value: string | null) => void)("gpt-rich");

    expect(onChange).toHaveBeenCalledWith({
      model: "gpt-rich",
      options: [
        { id: "reasoning", value: "high" },
        { id: "fastMode", value: false },
      ],
    });
  });

  it("selects the resolved live model for a saved model alias", () => {
    render(
      baseProps({
        models: [model("gpt-5.4")],
        value: { model: "gpt-5-codex" },
      }),
    );

    expect(entries("Select")[0]).toMatchObject({ disabled: false, value: "gpt-5.4" });
  });

  it("composes rapid model, effort, and fast changes without waiting for new props", () => {
    const onChange = vi.fn();
    render(baseProps({ onChange }));
    const [modelSelect, effortSelect] = entries("Select");
    const [fastSwitch] = entries("Switch");

    (modelSelect!.onValueChange as (value: string | null) => void)("gpt-next");
    (effortSelect!.onValueChange as (value: string | null) => void)("low");
    (fastSwitch!.onCheckedChange as (checked: boolean) => void)(true);

    expect(onChange).toHaveBeenNthCalledWith(1, {
      model: "gpt-next",
      options: [
        { id: "reasoning", value: "high" },
        { id: "fastMode", value: false },
      ],
    });
    expect(onChange).toHaveBeenNthCalledWith(2, {
      model: "gpt-next",
      options: [
        { id: "reasoning", value: "low" },
        { id: "fastMode", value: false },
      ],
    });
    expect(onChange).toHaveBeenNthCalledWith(3, {
      model: "gpt-next",
      options: [
        { id: "reasoning", value: "low" },
        { id: "fastMode", value: true },
      ],
    });
  });
});
