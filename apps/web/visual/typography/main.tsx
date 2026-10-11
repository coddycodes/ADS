import "@fontsource-variable/dm-sans/index.css";
import "@fontsource/jetbrains-mono/400.css";
import "@fontsource/jetbrains-mono/500.css";
import "./gallery.css";
import { RegistryContext } from "@effect/atom-react";
import { AtomRegistry } from "effect/unstable/reactivity";
import { Component, type ErrorInfo, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { applyStoredTheme, writeThemePreference } from "../../src/hooks/useTheme";
import { requireFixture } from "./catalog";
import { typographyFixtures } from "./fixtures";
import { prepareSettingsFixture } from "./settings-ports";
import { prepareRemainingContextFixture } from "./remaining-ports";
import settingsStates from "./settings-capture-states.json";
import gitActions from "./git-capture-actions.json";
import chatStates from "./chat-capture-states.json";
import remainingStates from "./remaining-capture-states.json";

class FixtureBoundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  override state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  override componentDidCatch(_error: Error, _info: ErrorInfo) {
    document.documentElement.dataset.fixtureStatus = "failed";
  }

  override render() {
    return this.state.failed ? <p role="alert">Fixture rendering failed.</p> : this.props.children;
  }
}

const params = new URLSearchParams(window.location.search);
const id = params.get("fixture") ?? "badge";
const theme = params.get("theme") ?? "light";
if (theme !== "light" && theme !== "dark") throw new Error("Typography theme refused.");
if (params.get("paint") === "linux-webkit") {
  if (
    !/\bLinux\b/.test(navigator.userAgent) ||
    !/AppleWebKit/.test(navigator.userAgent) ||
    /Chrome|Chromium/.test(navigator.userAgent)
  ) {
    throw new Error("Native Linux WebKit fixture painting refused.");
  }
  // The standalone fixture has no native desktop bridge. Enable the product CSS
  // gate explicitly after verifying the engine; this is component paint evidence.
  document.documentElement.dataset.linuxWebkit = "";
}
const fixture = requireFixture(typographyFixtures, id);
writeThemePreference(theme);
applyStoredTheme();
const rootElement = document.getElementById("root");
if (!rootElement) throw new Error("Typography fixture root missing.");
document.documentElement.dataset.fixtureId = fixture.id;
document.documentElement.dataset.fixtureStatus = "mounted";
const registry = AtomRegistry.make();
const cleanup = prepareSettingsFixture(fixture.id);
const cleanupRemaining = prepareRemainingContextFixture(fixture.id);
window.addEventListener(
  "pagehide",
  () => {
    cleanupRemaining();
    cleanup();
    registry.dispose();
  },
  { once: true },
);
const metadata = document.createElement("script");
metadata.type = "application/json";
metadata.id = "typography-fixture-metadata";
const stateById = new Map(Object.entries({ ...settingsStates, ...chatStates, ...remainingStates }));
const actionsById = new Map(Object.entries(gitActions));
metadata.textContent = JSON.stringify(
  typographyFixtures.map(({ render: _render, ...entry }) => ({
    ...entry,
    captureStates: stateById.get(entry.id) ?? [
      { state: "initial", actions: actionsById.get(entry.id) ?? [], visible_text: entry.anchors },
    ],
  })),
);
document.head.append(metadata);

createRoot(rootElement).render(
  <RegistryContext.Provider value={registry}>
    <FixtureBoundary>
      <main className="mx-auto max-w-6xl p-8">
        <header className="mb-6 border-b pb-4">
          <h1 className="text-lg font-semibold">{fixture.title}</h1>
          <p className="mt-1 text-xs text-muted-foreground">
            Actual component fixture · {theme} theme
          </p>
        </header>
        <section data-typography-fixture={fixture.id}>{fixture.render()}</section>
      </main>
    </FixtureBoundary>
  </RegistryContext.Provider>,
);
