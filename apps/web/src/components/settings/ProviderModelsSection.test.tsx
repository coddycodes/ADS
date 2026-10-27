/**
 * Behavior tests for ProviderModelsSection.
 *
 * Uses the repo's instrumented-hooks SSR pattern (see FilePreviewPanel.test.tsx):
 * a partial `vi.mock("react")` replaces useState/useEffect/useRef so state can
 * be seeded per scenario and setter calls recorded, while useMemo stays real
 * (the dispatcher is live during renderToStaticMarkup). Leaf UI children are
 * capture-mocked so their handler props (onClick / onChange / onKeyDown) can be
 * invoked directly without a DOM.
 */
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { ReactNode } from "react";
import {
  ProviderDriverKind,
  ProviderInstanceId,
  type ServerProviderModel,
} from "@coddy/contracts";

const harness = vi.hoisted(() => {
  type Matcher = (initial: unknown) => boolean;
  const state = {
    stateSeeds: [] as Array<{ match: Matcher; value: unknown }>,
    setStateCalls: [] as Array<{ initial: unknown; next: unknown; applied: unknown }>,
    refs: [] as Array<{ current: unknown }>,
    reset() {
      state.stateSeeds.length = 0;
      state.setStateCalls.length = 0;
      state.refs.length = 0;
    },
    seedState(match: Matcher, value: unknown) {
      state.stateSeeds.push({ match, value });
    },
  };
  return state;
});

const ui = vi.hoisted(() => {
  const registry = {
    entries: [] as Array<{ kind: string; props: Record<string, unknown> }>,
    reset() {
      registry.entries.length = 0;
    },
    record(kind: string, props: unknown) {
      if (props && typeof props === "object") {
        registry.entries.push({ kind, props: props as Record<string, unknown> });
      }
    },
    filter(kind: string, predicate?: (props: Record<string, unknown>) => boolean) {
      return registry.entries
        .filter((entry) => entry.kind === kind && (predicate?.(entry.props) ?? true))
        .map((entry) => entry.props);
    },
    find(kind: string, predicate?: (props: Record<string, unknown>) => boolean) {
      const found = registry.entries.find(
        (entry) => entry.kind === kind && (predicate?.(entry.props) ?? true),
      )?.props;
      if (!found) throw new Error(`No recorded "${kind}" element matched`);
      return found;
    },
    byLabel(kind: string, label: string) {
      return registry.find(kind, (props) => props["aria-label"] === label);
    },
  };
  return registry;
});

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  const resolveInitial = (initial: unknown): unknown =>
    typeof initial === "function" ? (initial as () => unknown)() : initial;

  const useState = (initial?: unknown) => {
    const resolved = resolveInitial(initial);
    const seedIndex = harness.stateSeeds.findIndex((seed) => seed.match(resolved));
    const value = seedIndex >= 0 ? harness.stateSeeds.splice(seedIndex, 1)[0]!.value : resolved;
    const setValue = (next: unknown) => {
      const applied =
        typeof next === "function" ? (next as (value: unknown) => unknown)(value) : next;
      harness.setStateCalls.push({ initial: resolved, next, applied });
    };
    return [value, setValue];
  };
  const useEffect = () => {};
  const useRef = (initial?: unknown) => {
    const ref = { current: initial ?? null };
    harness.refs.push(ref);
    return ref;
  };
  return {
    ...actual,
    useState: useState as typeof actual.useState,
    useEffect: useEffect as typeof actual.useEffect,
    useRef: useRef as typeof actual.useRef,
  };
});

vi.mock("../../modelSelection", () => ({
  MAX_CUSTOM_MODEL_LENGTH: 256,
}));

vi.mock("@coddy/shared/model", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@coddy/shared/model")>();
  return {
    ...actual,
    normalizeModelSlug: (value: string | null | undefined) => {
      const trimmed = typeof value === "string" ? value.trim() : "";
      return trimmed.length > 0 ? trimmed : null;
    },
  };
});

vi.mock("../ui/button", () => ({
  Button: (props: Record<string, unknown>) => {
    ui.record("Button", props);
    return (
      <button type="button" aria-label={props["aria-label"] as string | undefined}>
        {props.children as ReactNode}
      </button>
    );
  },
}));

vi.mock("../ui/input", () => ({
  Input: (props: Record<string, unknown>) => {
    ui.record("Input", props);
    return <input aria-label={props["aria-label"] as string | undefined} />;
  },
}));

vi.mock("../ui/tooltip", () => ({
  Tooltip: ({ children }: { children?: ReactNode }) => <>{children}</>,
  TooltipTrigger: ({ render, children }: { render?: ReactNode; children?: ReactNode }) => (
    <span>
      {render}
      {children}
    </span>
  ),
  TooltipPopup: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
}));

import { ProviderModelsSection } from "./ProviderModelsSection";

const INSTANCE_ID = ProviderInstanceId.make("codex");
const CODEX = ProviderDriverKind.make("codex");

type Props = Parameters<typeof ProviderModelsSection>[0];

function model(slug: string, overrides: Partial<ServerProviderModel> = {}): ServerProviderModel {
  return {
    slug,
    name: slug,
    isCustom: false,
    capabilities: null,
    ...overrides,
  };
}

function baseProps(overrides: Partial<Props> = {}): Props {
  return {
    instanceId: INSTANCE_ID,
    driverKind: CODEX,
    models: [],
    customModels: [],
    hiddenModels: [],
    favoriteModels: [],
    modelOrder: [],
    onChange: vi.fn(),
    onHiddenModelsChange: vi.fn(),
    onFavoriteModelsChange: vi.fn(),
    onModelOrderChange: vi.fn(),
    ...overrides,
  };
}

function render(props: Props): string {
  ui.reset();
  harness.setStateCalls.length = 0;
  harness.refs.length = 0;
  return renderToStaticMarkup(<ProviderModelsSection {...props} />);
}

beforeEach(() => {
  harness.reset();
  ui.reset();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("rendering", () => {
  it("shows singular / plural model counts", () => {
    expect(render(baseProps({ models: [model("a")] }))).toContain("1 model");
    const markup = render(baseProps({ models: [model("a"), model("b")] }));
    expect(markup).toContain("2 model");
    expect(markup).toContain("models available");
  });

  it("renders model names, custom + hidden markers, and detail tooltips", () => {
    const markup = render(
      baseProps({
        models: [
          model("gpt-x", {
            name: "GPT X",
            capabilities: {
              optionDescriptors: [
                { id: "fastMode", label: "Fast", type: "boolean" },
                { id: "thinking", label: "Think", type: "boolean" },
                {
                  id: "reasoningEffort",
                  label: "Effort",
                  type: "select",
                  options: [{ id: "high", label: "High" }],
                },
              ],
            },
          }),
          model("custom-1", { name: "custom-1", isCustom: true }),
        ],
        customModels: ["custom-1"],
        hiddenModels: ["gpt-x"],
      }),
    );
    expect(markup).toContain("GPT X");
    // capability labels surface inside the details tooltip
    expect(markup).toContain("Fast mode");
    expect(markup).toContain("Thinking");
    expect(markup).toContain("Reasoning");
    // the slug is shown in the tooltip code block
    expect(markup).toContain("gpt-x");
    // hidden non-custom model gets a "hidden" marker + strike-through
    expect(markup).toContain("hidden");
    expect(markup).toContain("line-through");
    // custom model gets a "custom" marker
    expect(markup).toContain("custom");
  });

  it("omits the details tooltip when there is nothing to show", () => {
    const markup = render(baseProps({ models: [model("plain")] }));
    expect(markup).not.toContain('aria-label="Details for plain"');
  });

  it("passes the driver placeholder to the input, falling back to model-slug", () => {
    render(baseProps({ driverKind: CODEX }));
    expect(ui.find("Input").placeholder).toBe("gpt-6.7-codex-ultra-preview");
    render(baseProps({ driverKind: null }));
    expect(ui.find("Input").placeholder).toBe("model-slug");
  });
});

describe("adding a custom model", () => {
  it("rejects empty input", () => {
    render(baseProps());
    (ui.find("Button", (p) => p.variant === "outline").onClick as () => void)();
    expect(harness.setStateCalls.some((c) => c.next === "Enter a model slug.")).toBe(true);
  });

  it("rejects empty input when no driver kind is set", () => {
    render(baseProps({ driverKind: null }));
    (ui.find("Button", (p) => p.variant === "outline").onClick as () => void)();
    expect(harness.setStateCalls.some((c) => c.next === "Enter a model slug.")).toBe(true);
  });

  it("rejects a slug that duplicates a built-in model", () => {
    harness.seedState((initial) => initial === "", "builtin");
    const onChange = vi.fn();
    render(baseProps({ models: [model("builtin")], onChange }));
    (ui.find("Button", (p) => p.variant === "outline").onClick as () => void)();
    expect(harness.setStateCalls.some((c) => c.next === "That model is already built in.")).toBe(
      true,
    );
    expect(onChange).not.toHaveBeenCalled();
  });

  it("rejects a slug that exceeds the maximum length", () => {
    harness.seedState((initial) => initial === "", "a".repeat(257));
    render(baseProps());
    (ui.find("Button", (p) => p.variant === "outline").onClick as () => void)();
    expect(
      harness.setStateCalls.some(
        (c) => typeof c.next === "string" && c.next.includes("characters or less"),
      ),
    ).toBe(true);
  });

  it("rejects a slug already saved as custom", () => {
    harness.seedState((initial) => initial === "", "dup");
    render(baseProps({ customModels: ["dup"] }));
    (ui.find("Button", (p) => p.variant === "outline").onClick as () => void)();
    expect(
      harness.setStateCalls.some((c) => c.next === "That custom model is already saved."),
    ).toBe(true);
  });

  it("commits a valid slug and scrolls the new row into view", () => {
    const scrollTo = vi.fn();
    const observe = vi.fn();
    const disconnect = vi.fn();
    const mutationCallbacks: Array<() => void> = [];
    const raf = vi.fn((cb: () => void) => {
      cb();
      return 1;
    });
    vi.stubGlobal("requestAnimationFrame", raf);
    vi.stubGlobal(
      "MutationObserver",
      class {
        constructor(cb: () => void) {
          mutationCallbacks.push(cb);
        }
        observe = observe;
        disconnect = disconnect;
      },
    );
    vi.stubGlobal("setTimeout", vi.fn());

    harness.seedState((initial) => initial === "", "new-model");
    const onChange = vi.fn();
    render(baseProps({ customModels: ["existing"], onChange }));
    // Seed the list ref so the scroll-into-view path runs.
    harness.refs[0]!.current = { scrollTo, scrollHeight: 500 };

    (ui.find("Button", (p) => p.variant === "outline").onClick as () => void)();
    expect(onChange).toHaveBeenCalledWith(["existing", "new-model"]);
    expect(harness.setStateCalls.some((c) => c.next === "")).toBe(true);
    expect(harness.setStateCalls.some((c) => c.next === null)).toBe(true);
    expect(raf).toHaveBeenCalled();
    expect(scrollTo).toHaveBeenCalledWith({ top: 500, behavior: "smooth" });
    expect(observe).toHaveBeenCalled();
    // The observer callback re-scrolls then disconnects.
    mutationCallbacks[0]?.();
    expect(disconnect).toHaveBeenCalled();
  });

  it("commits without scrolling when the list ref is unset", () => {
    harness.seedState((initial) => initial === "", "new-model");
    const onChange = vi.fn();
    render(baseProps({ onChange }));
    // Leave harness.refs[0].current === null.
    (ui.find("Button", (p) => p.variant === "outline").onClick as () => void)();
    expect(onChange).toHaveBeenCalledWith(["new-model"]);
  });
});

describe("input handlers", () => {
  it("updates the input value and clears an existing error on change", () => {
    harness.seedState((initial) => initial === null, "prior error");
    render(baseProps());
    const input = ui.find("Input");
    (input.onChange as (event: unknown) => void)({ target: { value: "typed" } });
    expect(harness.setStateCalls.some((c) => c.next === "typed")).toBe(true);
    expect(harness.setStateCalls.some((c) => c.next === null)).toBe(true);
  });

  it("ignores non-Enter keys and submits on Enter", () => {
    harness.seedState((initial) => initial === "", "kb-model");
    const onChange = vi.fn();
    render(baseProps({ onChange }));
    const input = ui.find("Input");

    const noop = { key: "a", preventDefault: vi.fn() };
    (input.onKeyDown as (event: unknown) => void)(noop);
    expect(noop.preventDefault).not.toHaveBeenCalled();
    expect(onChange).not.toHaveBeenCalled();

    const enter = { key: "Enter", preventDefault: vi.fn() };
    (input.onKeyDown as (event: unknown) => void)(enter);
    expect(enter.preventDefault).toHaveBeenCalled();
    expect(onChange).toHaveBeenCalledWith(["kb-model"]);
  });
});

describe("per-model actions", () => {
  const threeModels = [model("m1"), model("m2"), model("m3")];

  it("toggles favorites on and off", () => {
    const onFavoriteModelsChange = vi.fn();
    render(baseProps({ models: [model("m1")], onFavoriteModelsChange }));
    (ui.byLabel("Button", "Add m1 to favorites").onClick as () => void)();
    expect(onFavoriteModelsChange).toHaveBeenCalledWith(["m1"]);

    onFavoriteModelsChange.mockReset();
    render(baseProps({ models: [model("m1")], favoriteModels: ["m1"], onFavoriteModelsChange }));
    (ui.byLabel("Button", "Remove m1 from favorites").onClick as () => void)();
    expect(onFavoriteModelsChange).toHaveBeenCalledWith([]);
  });

  it("toggles hidden state for built-in models", () => {
    const onHiddenModelsChange = vi.fn();
    render(baseProps({ models: [model("m1")], onHiddenModelsChange }));
    (ui.byLabel("Button", "Hide m1").onClick as () => void)();
    expect(onHiddenModelsChange).toHaveBeenCalledWith(["m1"]);

    onHiddenModelsChange.mockReset();
    render(baseProps({ models: [model("m1")], hiddenModels: ["m1"], onHiddenModelsChange }));
    (ui.byLabel("Button", "Show m1").onClick as () => void)();
    expect(onHiddenModelsChange).toHaveBeenCalledWith([]);
  });

  it("removes custom models and prunes order + favorites", () => {
    const onChange = vi.fn();
    const onModelOrderChange = vi.fn();
    const onFavoriteModelsChange = vi.fn();
    render(
      baseProps({
        models: [model("c1", { isCustom: true })],
        customModels: ["c1", "c2"],
        modelOrder: ["c1", "c2"],
        favoriteModels: ["c1"],
        onChange,
        onModelOrderChange,
        onFavoriteModelsChange,
      }),
    );
    (ui.byLabel("Button", "Remove c1").onClick as () => void)();
    expect(onChange).toHaveBeenCalledWith(["c2"]);
    expect(onModelOrderChange).toHaveBeenCalledWith(["c2"]);
    expect(onFavoriteModelsChange).toHaveBeenCalledWith([]);
  });

  it("moves a model down and up within its group", () => {
    const onModelOrderChange = vi.fn();
    render(baseProps({ models: threeModels, onModelOrderChange }));

    (ui.byLabel("Button", "Move m2 up").onClick as () => void)();
    expect(onModelOrderChange).toHaveBeenLastCalledWith(["m2", "m1", "m3"]);

    (ui.byLabel("Button", "Move m2 down").onClick as () => void)();
    expect(onModelOrderChange).toHaveBeenLastCalledWith(["m1", "m3", "m2"]);
  });

  it("ignores moves that fall off either end", () => {
    const onModelOrderChange = vi.fn();
    render(baseProps({ models: threeModels, onModelOrderChange }));
    (ui.byLabel("Button", "Move m1 up").onClick as () => void)();
    (ui.byLabel("Button", "Move m3 down").onClick as () => void)();
    expect(onModelOrderChange).not.toHaveBeenCalled();
  });
});
