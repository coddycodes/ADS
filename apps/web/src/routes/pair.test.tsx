// @vitest-environment happy-dom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { encodePairingCode } from "@coddy/shared/pairingCode";

const captured = vi.hoisted(() => ({
  pairingProps: null as Record<string, unknown> | null,
  navigate: vi.fn(),
}));

vi.mock("../components/auth/PairingRouteSurface", () => ({
  HostedPairingRouteSurface: () => null,
  PairingPendingSurface: () => null,
  PairingRouteSurface: (props: Record<string, unknown>) => {
    captured.pairingProps = props;
    return null;
  },
}));

import { Route } from "./pair";

const PAIRING_CODE = encodePairingCode({
  v: 1,
  endpoint: "https://backend.example.test",
  name: "Office",
  token: "BCDFGHJKMNPQ",
  hostKey: "HcMLXPPBHFNvcbHrCVMH-DMh49rd5AGCzSCqAVJ49hM",
  reach: "another-device",
  storageInstanceId: "3f2f6a52-6f5f-4f4e-9d38-0a1e2ac21d11",
});

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  captured.pairingProps = null;
  captured.navigate.mockReset();
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.restoreAllMocks();
});

describe("/pair with a pairing code", () => {
  it("validates the code search param", () => {
    const validate = Route.options.validateSearch;
    if (typeof validate !== "function") throw new Error("validateSearch is not registered.");
    expect(validate({ code: "abc" })).toEqual({ code: "abc" });
    expect(validate({ code: "abc", host: "devbox", label: "Office", junk: "x" })).toEqual({
      code: "abc",
      host: "devbox",
      label: "Office",
    });
    expect(validate({ code: "" })).toEqual({});
    expect(validate({})).toEqual({});
  });

  it("replaces a legacy query code immediately while retaining it for pairing", async () => {
    const Component = Route.options.component;
    if (typeof Component !== "function") throw new Error("Route component is not registered.");
    vi.spyOn(Route, "useRouteContext").mockReturnValue({
      authGateState: { status: "pairing", auth: { bootstrapMethods: [] } },
    } as never);
    vi.spyOn(Route, "useSearch").mockReturnValue({
      code: PAIRING_CODE,
      host: "devbox",
      label: "Office",
    } as never);
    vi.spyOn(Route, "useNavigate").mockReturnValue(captured.navigate as never);

    await act(async () => root.render(<Component />));

    expect(captured.navigate).toHaveBeenCalledTimes(1);
    expect(captured.navigate).toHaveBeenCalledWith({
      search: { host: "devbox", label: "Office" },
      replace: true,
    });
    expect(captured.pairingProps).toMatchObject({ initialCredential: "BCDFGHJKMNPQ" });

    const onInitialCredentialConsumed = captured.pairingProps?.onInitialCredentialConsumed as
      | (() => void)
      | undefined;
    expect(onInitialCredentialConsumed).toBeDefined();
    await act(async () => {
      onInitialCredentialConsumed?.();
      root.render(<Component />);
    });
    expect(captured.pairingProps).not.toHaveProperty("initialCredential");
  });

  it("captures and scrubs a code that arrives on an already-mounted pair route", async () => {
    const Component = Route.options.component;
    if (typeof Component !== "function") throw new Error("Route component is not registered.");
    vi.spyOn(Route, "useRouteContext").mockReturnValue({
      authGateState: { status: "pairing", auth: { bootstrapMethods: [] } },
    } as never);
    const useSearch = vi.spyOn(Route, "useSearch").mockReturnValue({} as never);
    vi.spyOn(Route, "useNavigate").mockReturnValue(captured.navigate as never);

    await act(async () => root.render(<Component />));
    expect(captured.navigate).not.toHaveBeenCalled();

    useSearch.mockReturnValue({ code: PAIRING_CODE, host: "devbox", label: "Office" } as never);
    await act(async () => root.render(<Component />));

    expect(captured.navigate).toHaveBeenCalledWith({
      search: { host: "devbox", label: "Office" },
      replace: true,
    });
    expect(captured.pairingProps).toMatchObject({ initialCredential: "BCDFGHJKMNPQ" });
  });

  it("forwards an authenticated client to Remote Servers with the code", async () => {
    const beforeLoad = Route.options.beforeLoad;
    if (typeof beforeLoad !== "function") throw new Error("beforeLoad is not registered.");
    await expect(
      Promise.resolve().then(() =>
        beforeLoad({
          context: { authGateState: { status: "authenticated" } },
          search: { code: "abc" },
        } as never),
      ),
    ).rejects.toMatchObject({
      options: { to: "/settings/remote-servers", search: { code: "abc" }, replace: true },
    });
  });

  it("still sends an authenticated client without a code to the root", async () => {
    const beforeLoad = Route.options.beforeLoad;
    if (typeof beforeLoad !== "function") throw new Error("beforeLoad is not registered.");
    await expect(
      Promise.resolve().then(() =>
        beforeLoad({
          context: { authGateState: { status: "authenticated" } },
          search: {},
        } as never),
      ),
    ).rejects.toMatchObject({ options: { to: "/", replace: true } });
  });

  it("never gates a fresh unauthenticated device carrying a code", async () => {
    const beforeLoad = Route.options.beforeLoad;
    if (typeof beforeLoad !== "function") throw new Error("beforeLoad is not registered.");
    await expect(
      Promise.resolve().then(() =>
        beforeLoad({
          context: { authGateState: { status: "pairing", auth: {} } },
          search: { code: "abc" },
        } as never),
      ),
    ).resolves.toMatchObject({ authGateState: { status: "pairing" } });
  });
});
