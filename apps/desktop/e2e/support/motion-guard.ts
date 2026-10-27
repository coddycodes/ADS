export async function installDesktopUiMotionGuard(): Promise<void> {
  await browser.execute(() => {
    const selector = "style[data-coddy-studio-desktop-ui-automation]";
    if (!document.querySelector(selector)) {
      const style = document.createElement("style");
      style.dataset.coddyStudioDesktopUiAutomation = "true";
      style.textContent = [
        `
    html:not([data-coddy-studio-desktop-ui-motion="native"]) *,
    html:not([data-coddy-studio-desktop-ui-motion="native"]) *::before,
    html:not([data-coddy-studio-desktop-ui-motion="native"]) *::after {
      animation-delay: 0s !important;
      animation-duration: 0s !important;
      transition-delay: 0s !important;
      transition-duration: 0s !important;
    }`,
        `
    [data-open][data-starting-style] {
      opacity: 1 !important;
      scale: 1 !important;
      translate: none !important;
      transform: none !important;
    }`,
        `
    [data-closed] {
      display: none !important;
    }`,
        `
    [data-slot="sidebar-group"][data-testid="sidebar-projects-group"]
      ul[data-sidebar="menu"] > li {
      opacity: 1 !important;
      transform: none !important;
    }`,
      ].join("\n");
      document.head.append(style);
    }
    document.documentElement.dataset.coddyStudioDesktopUiMotion = "disabled";
  });
}

export async function setDesktopUiMotionMode(mode: "native" | "disabled"): Promise<void> {
  await browser.execute((value: string) => {
    document.documentElement.dataset.coddyStudioDesktopUiMotion = value;
  }, mode);
}
