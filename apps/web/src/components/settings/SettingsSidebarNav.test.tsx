import type { ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vite-plus/test";

import { createEnvironmentPresentationPolicy } from "~/connection/environmentPresentationPolicy";

const h = vi.hoisted(() => ({
  policy: null as ReturnType<
    typeof import("~/connection/environmentPresentationPolicy").createEnvironmentPresentationPolicy
  > | null,
}));

vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => vi.fn(),
  useCanGoBack: () => false,
}));

vi.mock("~/connection/currentEnvironmentPresentation", () => ({
  readCurrentEnvironmentPresentationPolicy: () => h.policy,
}));

vi.mock("../ui/sidebar", () => ({
  SidebarContent: (props: { children?: ReactNode }) => <div>{props.children}</div>,
  SidebarFooter: (props: { children?: ReactNode }) => <footer>{props.children}</footer>,
  SidebarGroup: (props: { children?: ReactNode }) => <div>{props.children}</div>,
  SidebarMenu: (props: { children?: ReactNode }) => <div>{props.children}</div>,
  SidebarMenuButton: (props: { children?: ReactNode }) => <button>{props.children}</button>,
  SidebarMenuItem: (props: { children?: ReactNode }) => <div>{props.children}</div>,
  SidebarSeparator: () => <hr />,
  useSidebar: () => ({ isMobile: false, setOpenMobile: vi.fn() }),
}));

vi.mock("../clerk/CoddyConnectSidebarSignIn", () => ({
  CoddyConnectSidebarSignIn: () => <span>Coddy Connect sign in</span>,
  CoddyConnectSidebarAvatar: () => <span>Coddy Connect avatar</span>,
}));

import {
  BASE_SETTINGS_NAV_ITEMS,
  SettingsSidebarNav,
  settingsNavItemsFor,
} from "./SettingsSidebarNav";

describe("settings navigation", () => {
  it("lists the settings sections in the approved order", () => {
    expect(BASE_SETTINGS_NAV_ITEMS.map(({ label, to }) => [label, to])).toEqual([
      ["General", "/settings/general"],
      ["Remote Servers", "/settings/remote-servers"],
      ["Agents", "/settings/agents"],
      ["Status Bar", "/settings/status-bar"],
      ["Terminal", "/settings/terminal"],
      ["Keybindings", "/settings/keybindings"],
      ["Providers", "/settings/providers"],
      ["Source Control", "/settings/source-control"],
      ["Mobile emulators", "/settings/mobile"],
      ["Archive", "/settings/archived"],
      ["Diagnostics", "/settings/diagnostics"],
      ["About", "/settings/about"],
    ]);
  });

  it("adds Local environment only for Windows desktop", () => {
    const windowsPolicy = createEnvironmentPresentationPolicy({
      surface: "desktop",
      platform: "windows",
    });
    const macPolicy = createEnvironmentPresentationPolicy({
      surface: "desktop",
      platform: "macos",
    });
    const browserPolicy = createEnvironmentPresentationPolicy({
      surface: "browser",
      platform: "unknown",
    });

    expect(settingsNavItemsFor(windowsPolicy).map((item) => item.label)).toContain(
      "Local environment",
    );
    expect(
      settingsNavItemsFor(windowsPolicy)
        .map(({ label, to }) => [label, to])
        .slice(0, 3),
    ).toEqual([
      ["General", "/settings/general"],
      ["Remote Servers", "/settings/remote-servers"],
      ["Local environment", "/settings/local-environment"],
    ]);
    expect(settingsNavItemsFor(macPolicy).map((item) => item.label)).not.toContain(
      "Local environment",
    );
    expect(settingsNavItemsFor(browserPolicy).map((item) => item.label)).not.toContain(
      "Local environment",
    );
  });

  it("omits Connect footer controls on desktop and retains them in browser mode", () => {
    h.policy = createEnvironmentPresentationPolicy({ surface: "desktop", platform: "windows" });
    const desktopMarkup = renderToStaticMarkup(<SettingsSidebarNav pathname="/settings/general" />);
    expect(desktopMarkup).not.toContain("Coddy Connect sign in");
    expect(desktopMarkup).not.toContain("Coddy Connect avatar");

    h.policy = createEnvironmentPresentationPolicy({ surface: "browser", platform: "unknown" });
    const browserMarkup = renderToStaticMarkup(<SettingsSidebarNav pathname="/settings/general" />);
    expect(browserMarkup).toContain("Coddy Connect sign in");
    expect(browserMarkup).toContain("Coddy Connect avatar");
  });
});
