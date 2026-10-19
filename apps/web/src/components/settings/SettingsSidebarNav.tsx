import { useCallback, type ComponentType } from "react";
import {
  ActivityIcon,
  ArchiveIcon,
  ArrowLeftIcon,
  BotIcon,
  GitBranchIcon,
  InfoIcon,
  KeyboardIcon,
  MonitorIcon,
  PanelBottomIcon,
  ServerIcon,
  Settings2Icon,
  TerminalIcon,
} from "lucide-react";
import { useCanGoBack, useNavigate } from "@tanstack/react-router";

import {
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarSeparator,
  useSidebar,
} from "../ui/sidebar";
import {
  BiBCodeConnectSidebarAvatar,
  BiBCodeConnectSidebarSignIn,
} from "../clerk/BiBCodeConnectSidebarSignIn";
import { readCurrentEnvironmentPresentationPolicy } from "~/connection/currentEnvironmentPresentation";
import type { EnvironmentPresentationPolicy } from "~/connection/environmentPresentationPolicy";

export type SettingsSectionPath =
  | "/settings/general"
  | "/settings/remote-servers"
  | "/settings/local-environment"
  | "/settings/agents"
  | "/settings/status-bar"
  | "/settings/terminal"
  | "/settings/keybindings"
  | "/settings/providers"
  | "/settings/source-control"
  | "/settings/archived"
  | "/settings/diagnostics"
  | "/settings/about";

export interface SettingsNavItem {
  readonly label: string;
  readonly to: SettingsSectionPath;
  readonly icon: ComponentType<{ className?: string }>;
}

export const BASE_SETTINGS_NAV_ITEMS: ReadonlyArray<SettingsNavItem> = [
  { label: "General", to: "/settings/general", icon: Settings2Icon },
  { label: "Remote Servers", to: "/settings/remote-servers", icon: ServerIcon },
  { label: "Agents", to: "/settings/agents", icon: BotIcon },
  { label: "Status Bar", to: "/settings/status-bar", icon: PanelBottomIcon },
  { label: "Terminal", to: "/settings/terminal", icon: TerminalIcon },
  { label: "Keybindings", to: "/settings/keybindings", icon: KeyboardIcon },
  { label: "Providers", to: "/settings/providers", icon: BotIcon },
  { label: "Source Control", to: "/settings/source-control", icon: GitBranchIcon },
  { label: "Archive", to: "/settings/archived", icon: ArchiveIcon },
  { label: "Diagnostics", to: "/settings/diagnostics", icon: ActivityIcon },
  { label: "About", to: "/settings/about", icon: InfoIcon },
];

const LOCAL_ENVIRONMENT_NAV_ITEM = {
  label: "Local environment",
  to: "/settings/local-environment",
  icon: MonitorIcon,
} as const;

export function settingsNavItemsFor(
  policy: EnvironmentPresentationPolicy,
): ReadonlyArray<SettingsNavItem> {
  return policy.showLocalEnvironmentSettings
    ? [
        BASE_SETTINGS_NAV_ITEMS[0]!,
        BASE_SETTINGS_NAV_ITEMS[1]!,
        LOCAL_ENVIRONMENT_NAV_ITEM,
        ...BASE_SETTINGS_NAV_ITEMS.slice(2),
      ]
    : BASE_SETTINGS_NAV_ITEMS;
}

export function SettingsSidebarNav({ pathname }: { pathname: string }) {
  const policy = readCurrentEnvironmentPresentationPolicy();
  const navItems = settingsNavItemsFor(policy);
  const navigate = useNavigate();
  const canGoBack = useCanGoBack();
  const { isMobile, setOpenMobile } = useSidebar();
  const handleSectionClick = useCallback(
    (to: SettingsSectionPath) => {
      if (isMobile) {
        setOpenMobile(false);
      }
      void navigate({ to, replace: true });
    },
    [isMobile, navigate, setOpenMobile],
  );
  const handleBackClick = useCallback(() => {
    if (isMobile) {
      setOpenMobile(false);
    }
    if (canGoBack) {
      window.history.back();
      return;
    }
    void navigate({ to: "/" });
  }, [canGoBack, isMobile, navigate, setOpenMobile]);

  return (
    <>
      <SidebarContent className="overflow-x-hidden">
        <SidebarGroup className="px-2 py-3">
          <SidebarMenu>
            {navItems.map((item) => {
              const Icon = item.icon;
              const isActive = pathname === item.to;
              return (
                <SidebarMenuItem key={item.to}>
                  <SidebarMenuButton
                    size="sm"
                    isActive={isActive}
                    className={
                      isActive
                        ? "gap-2.5 px-2.5 py-2 text-left text-[13px] font-medium text-foreground"
                        : "gap-2.5 px-2.5 py-2 text-left text-[13px] text-muted-foreground/70 hover:text-foreground/80"
                    }
                    onClick={() => handleSectionClick(item.to)}
                  >
                    <Icon
                      className={
                        isActive
                          ? "size-4 shrink-0 text-foreground"
                          : "size-4 shrink-0 text-muted-foreground/60"
                      }
                    />
                    <span className="truncate">{item.label}</span>
                  </SidebarMenuButton>
                </SidebarMenuItem>
              );
            })}
          </SidebarMenu>
        </SidebarGroup>
      </SidebarContent>

      <SidebarSeparator />
      <SidebarFooter className="p-2">
        {policy.showRemoteDeviceControls ? <BiBCodeConnectSidebarSignIn /> : null}
        <div className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-1">
          <SidebarMenu className="min-w-0">
            <SidebarMenuItem>
              <SidebarMenuButton
                size="sm"
                className="gap-2 px-2 py-2 text-xs text-muted-foreground hover:bg-accent hover:text-foreground"
                onClick={handleBackClick}
              >
                <ArrowLeftIcon className="size-4" />
                <span>Back</span>
              </SidebarMenuButton>
            </SidebarMenuItem>
          </SidebarMenu>
          {policy.showRemoteDeviceControls ? <BiBCodeConnectSidebarAvatar /> : null}
        </div>
      </SidebarFooter>
    </>
  );
}
