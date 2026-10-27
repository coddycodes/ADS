import { afterEach, describe, expect, it, vi } from "vite-plus/test";

import { openPendingTab } from "./browserTab";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("openPendingTab", () => {
  it("opens a blank tab detached from this window and drives it later", () => {
    const replace = vi.fn();
    const close = vi.fn();
    const tab = { opener: {} as unknown, location: { replace }, close };
    const open = vi.fn(() => tab);
    vi.stubGlobal("window", { open });

    const pending = openPendingTab();

    expect(open).toHaveBeenCalledWith("about:blank", "_blank");
    expect(tab.opener).toBeNull();
    pending?.navigate("http://10.0.0.2:41000/__coddy-studio/bootstrap?cap=C&to=%2F");
    expect(replace).toHaveBeenCalledWith("http://10.0.0.2:41000/__coddy-studio/bootstrap?cap=C&to=%2F");
    pending?.close();
    expect(close).toHaveBeenCalled();
  });

  it("closes the tab instead of navigating it to anything but http(s)", () => {
    const replace = vi.fn();
    const close = vi.fn();
    vi.stubGlobal("window", { open: () => ({ opener: null, location: { replace }, close }) });

    for (const url of ["javascript:alert(1)", "file:///etc/passwd", "not a url"]) {
      openPendingTab()?.navigate(url);
    }

    expect(replace).not.toHaveBeenCalled();
    expect(close).toHaveBeenCalledTimes(3);
  });

  it("returns null when the popup is blocked", () => {
    vi.stubGlobal("window", { open: vi.fn(() => null) });
    expect(openPendingTab()).toBeNull();
  });
});
