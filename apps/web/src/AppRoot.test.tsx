import { Children, isValidElement, type ReactElement, type ReactNode } from "react";
import { RouterProvider } from "@tanstack/react-router";
import type { DesktopPreviewBridge } from "@bibcode/contracts";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({ previewBridge: null as DesktopPreviewBridge | null }));

vi.mock("./components/preview/previewBridge", () => ({
  get previewBridge() {
    return h.previewBridge;
  },
}));

import { PreviewAutomationHosts } from "./components/preview/PreviewAutomationHosts";
import { PreviewNewWindowRouter } from "./components/preview/PreviewNewWindowRouter";
import { OpenRequestRouter } from "./components/preview/OpenRequestRouter";
import { OpenPromptBanner } from "./components/preview/OpenPromptBanner";
import { ConnectionDatabaseRecoveryDialog } from "./components/ConnectionDatabaseRecoveryDialog";
import { AppAtomRegistryProvider } from "./rpc/atomRegistry";
import type { AppRouter } from "./router";
import { ThreadLifecycleReconciler } from "./ThreadLifecycleReconciler";
import { registerPreviewRuntimeCapabilities } from "./previewRuntimeCapabilities";
import { AppRoot, ProjectDataRecoveryCoordinator, ShareExposureReconciler } from "./AppRoot";
import { RemoteUpdateConfirmationCoordinator } from "./components/settings/UpdateServerDialog";
import { ServerReloadPrompt } from "./components/ServerReloadPrompt";

/** The reload prompt and the open prompt share one stacked top container, so neither hides the other. */
function expectTopBannerStack(stack: ReactNode, router?: AppRouter) {
  expect(isValidElement(stack) && stack.type).toBe("div");
  const props = (stack as ReactElement<{ className: string; children: ReactNode }>).props;
  expect(props.className).toContain("fixed");
  expect(props.className).toContain("flex-col");
  const banners = Children.toArray(props.children);
  expect(banners.map((banner) => isValidElement(banner) && banner.type)).toEqual([
    ServerReloadPrompt,
    OpenPromptBanner,
  ]);
  // The open prompt sits outside the router, so it gets the router to show an off-screen thread.
  if (router !== undefined) {
    expect((banners[1] as ReactElement<{ router: AppRouter }>).props.router).toBe(router);
  }
}

describe("AppRoot", () => {
  beforeEach(() => {
    h.previewBridge = {} as DesktopPreviewBridge;
  });

  it("shares the application atom registry with routed UI and preview automation", () => {
    const router = {} as AppRouter;
    const root = AppRoot({ router });

    expect(root.type).toBe(AppAtomRegistryProvider);
    const children = Children.toArray(
      (root as ReactElement<{ readonly children: ReactNode }>).props.children,
    );
    expect(children).toHaveLength(10);
    expect(isValidElement(children[0]) && children[0].type).toBe(ConnectionDatabaseRecoveryDialog);
    expect(isValidElement(children[1]) && children[1].type).toBe(ShareExposureReconciler);
    expect(isValidElement(children[2]) && children[2].type).toBe(ThreadLifecycleReconciler);
    expect(isValidElement(children[3]) && children[3].type).toBe(ProjectDataRecoveryCoordinator);
    expect(isValidElement(children[4]) && children[4].type).toBe(RouterProvider);
    expect(isValidElement(children[5]) && children[5].type).toBe(
      RemoteUpdateConfirmationCoordinator,
    );
    expectTopBannerStack(children[6], router);
    expect(isValidElement(children[7]) && children[7].type).toBe(PreviewAutomationHosts);
    expect(isValidElement(children[8]) && children[8].type).toBe(PreviewNewWindowRouter);
    expect(isValidElement(children[9]) && children[9].type).toBe(OpenRequestRouter);
  });

  it("mounts preview automation hosts for a preview bridge without full automation", () => {
    const bridge = {} as DesktopPreviewBridge;
    registerPreviewRuntimeCapabilities(bridge, {
      picker: false,
      recording: false,
      automation: false,
      imageClipboard: false,
      screenshot: true,
      pageTools: true,
    });
    h.previewBridge = bridge;

    const root = AppRoot({ router: {} as AppRouter });
    const children = Children.toArray(
      (root as ReactElement<{ readonly children: ReactNode }>).props.children,
    );

    expect(children).toHaveLength(10);
    expect(isValidElement(children[7]) && children[7].type).toBe(PreviewAutomationHosts);
  });

  it("mounts browser-mode automation hosts and open routing without a preview bridge", () => {
    h.previewBridge = null;

    const root = AppRoot({ router: {} as AppRouter });
    const children = Children.toArray(
      (root as ReactElement<{ readonly children: ReactNode }>).props.children,
    );

    expect(children).toHaveLength(10);
    expect(isValidElement(children[0]) && children[0].type).toBe(ConnectionDatabaseRecoveryDialog);
    expect(isValidElement(children[4]) && children[4].type).toBe(RouterProvider);
    expect(isValidElement(children[7]) && children[7].type).toBe(PreviewAutomationHosts);
    expect(isValidElement(children[9]) && children[9].type).toBe(OpenRequestRouter);
  });
});
