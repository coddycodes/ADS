// @vitest-environment happy-dom
import { RegistryContext } from "@effect/atom-react";
import { AtomRegistry } from "effect/unstable/reactivity";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it } from "vite-plus/test";
import { typographyFixtures } from "./fixtures";
import { prepareSettingsFixture } from "./settings-ports";
import { prepareRemainingContextFixture } from "./remaining-ports";
import gitActions from "./git-capture-actions.json";
const actionsById = new Map(Object.entries(gitActions));

it.each(typographyFixtures)(
  "mounts the actual $id parent with owned fixture services",
  async (fixture) => {
    const registry = AtomRegistry.make();
    const cleanup = prepareSettingsFixture(fixture.id);
    const cleanupRemaining = prepareRemainingContextFixture(fixture.id);
    const element = document.createElement("div");
    document.body.append(element);
    const root = createRoot(element);
    try {
      await act(async () => {
        root.render(
          <RegistryContext.Provider value={registry}>{fixture.render()}</RegistryContext.Provider>,
        );
      });
      if (fixture.id === "settings-redacted-email") {
        const reveal = element.querySelector<HTMLButtonElement>("button");
        if (!reveal) throw new Error("Fixture reveal control missing.");
        await act(async () => reveal.click());
      }
      for (const action of actionsById.get(fixture.id) ?? []) {
        const selector = "selector" in action ? action.selector : "button, input, textarea, [role]";
        const target = [...document.querySelectorAll<HTMLElement>(selector)].find((element) => {
          if (!("name" in action)) return true;
          const labelIds = element.getAttribute("aria-labelledby");
          const label =
            element.getAttribute("aria-label") ??
            (labelIds
              ? labelIds
                  .split(" ")
                  .map((id) => document.getElementById(id)?.textContent ?? "")
                  .join(" ")
              : null) ??
            (element.id
              ? document.querySelector(`label[for="${CSS.escape(element.id)}"]`)?.textContent
              : null) ??
            element.textContent;
          return label?.trim() === action.name;
        });
        if (!target) throw new Error(`Owned fixture action target missing: ${fixture.id}.`);
        await act(async () => {
          target.click();
          if ("key" in action)
            target.dispatchEvent(new KeyboardEvent("keydown", { key: action.key, bubbles: true }));
        });
      }
      if (fixture.id === "chat-markdown-file-tooltip") {
        const link = element.querySelector<HTMLAnchorElement>(".chat-markdown-file-link");
        if (!link) throw new Error("Fixture file link missing.");
        await act(async () => {
          document.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab", bubbles: true }));
          link.focus();
        });
      }
      if (fixture.id.startsWith("git-stash-list")) {
        expect(element.querySelector('[aria-label="Stashes"]')).not.toBeNull();
        return;
      }
      if (fixture.id.startsWith("git-stash-diff")) {
        await expect.poll(() => document.body.textContent ?? "").toContain(fixture.anchors.at(-1));
        return;
      }
      await expect.poll(() => document.body.textContent ?? "").toContain(fixture.anchors[0]);
    } finally {
      await act(async () => root.unmount());
      cleanup();
      cleanupRemaining();
      registry.dispose();
      element.remove();
    }
  },
);
