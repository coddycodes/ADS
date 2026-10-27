// @vitest-environment happy-dom

import { EnvironmentId } from "@coddy/contracts";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";

const captured = vi.hoisted(() => ({ props: null as Record<string, unknown> | null }));

vi.mock("../components/settings/DiagnosticsSettings", () => ({
  DiagnosticsSettingsPanel: (props: Record<string, unknown>) => {
    captured.props = props;
    return null;
  },
}));

import { Route, validateDiagnosticsSearch } from "./settings.diagnostics";

afterEach(() => {
  captured.props = null;
  vi.restoreAllMocks();
});

describe("/settings/diagnostics", () => {
  it("keeps only a non-empty environment search param", () => {
    expect(validateDiagnosticsSearch({ environment: "luna", junk: "x" })).toEqual({
      environment: "luna",
    });
    expect(validateDiagnosticsSearch({ environment: "" })).toEqual({});
    expect(validateDiagnosticsSearch({ environment: 3 })).toEqual({});
    expect(validateDiagnosticsSearch({})).toEqual({});
  });

  it("passes the search param to the panel and writes a pick back to it", () => {
    const Component = Route.options.component;
    if (typeof Component !== "function") throw new Error("Route component is not registered.");
    const navigate = vi.fn();
    vi.spyOn(Route, "useSearch").mockReturnValue({ environment: "luna" } as never);
    vi.spyOn(Route, "useNavigate").mockReturnValue(navigate as never);

    renderToStaticMarkup(<Component />);

    expect(captured.props).toMatchObject({ profilerEnvironmentId: EnvironmentId.make("luna") });
    const change = captured.props!.onProfilerEnvironmentChange as (
      environmentId: EnvironmentId | null,
    ) => void;
    change(EnvironmentId.make("mars"));
    expect(navigate).toHaveBeenLastCalledWith({ search: { environment: "mars" }, replace: true });
    change(null);
    expect(navigate).toHaveBeenLastCalledWith({ search: {}, replace: true });
  });

  it("leaves the selection to the panel without a search param", () => {
    const Component = Route.options.component;
    if (typeof Component !== "function") throw new Error("Route component is not registered.");
    vi.spyOn(Route, "useSearch").mockReturnValue({} as never);
    vi.spyOn(Route, "useNavigate").mockReturnValue(vi.fn() as never);

    renderToStaticMarkup(<Component />);

    expect(captured.props).toMatchObject({ profilerEnvironmentId: null });
  });
});
