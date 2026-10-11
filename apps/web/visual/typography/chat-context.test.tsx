// @vitest-environment happy-dom
import { RegistryContext } from "@effect/atom-react";
import { AtomRegistry } from "effect/unstable/reactivity";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vite-plus/test";
import { TooltipProvider } from "../../src/components/ui/tooltip";
import { chatFixtures } from "./chat-context";
import type { Fixture } from "./types";

const mounted: Array<{ dispose: () => Promise<void> }> = [];
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  Object.defineProperty(Element.prototype, "getAnimations", {
    configurable: true,
    value: () => [],
  });
});
afterEach(async () => {
  for (const tree of mounted.splice(0)) await tree.dispose();
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 25));
  });
  document.body.replaceChildren();
  vi.unstubAllGlobals();
});
async function mount(fixture: Fixture) {
  const registry = AtomRegistry.make();
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  mounted.push({
    dispose: async () => {
      await act(async () => root.unmount());
      registry.dispose();
      container.remove();
    },
  });
  await act(async () => {
    root.render(
      <RegistryContext.Provider value={registry}>
        <TooltipProvider delay={0}>{fixture.render()}</TooltipProvider>
      </RegistryContext.Provider>,
    );
  });
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 25));
  });
  return container;
}
function fixture(id: string): Fixture {
  const match = chatFixtures.find((item) => item.id === id);
  if (!match) throw new Error(`Missing fixture ${id}.`);
  return match;
}
for (const entry of chatFixtures) {
  it(`renders actual anchors for ${entry.id}`, async () => {
    const container = await mount(entry);
    if (entry.id === "chat-markdown-file-tooltip") {
      const link = container.querySelector<HTMLAnchorElement>(".chat-markdown-file-link");
      if (!link) throw new Error("Missing real Markdown file link.");
      await act(async () => {
        document.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab", bubbles: true }));
        link.focus();
      });
      await act(async () => {
        await new Promise((resolve) => setTimeout(resolve, 25));
      });
      expect(document.querySelector(".markdown-file-link-tooltip-scroll")).not.toBeNull();
    }
    for (const anchor of entry.anchors) expect(document.body.textContent).toContain(anchor);
  });
}
it("exposes real private Markdown file chips, footnotes and code title", async () => {
  const container = await mount(fixture("chat-markdown"));
  expect(container.querySelectorAll(".chat-markdown-file-link")).toHaveLength(2);
  expect(container.querySelector("a[data-footnote-ref]")?.textContent).toBe("1");
  expect(container.querySelector("a[data-footnote-backref]")?.textContent).toBe("↩");
  expect(container.querySelector(".chat-markdown-codeblock-title")?.textContent).toBe(
    "typography-example.txt",
  );
});
it("expands the plan through its real public button and real ChatMarkdown child", async () => {
  const container = await mount(fixture("plan-sidebar"));
  const trigger = Array.from(container.querySelectorAll("button")).find((button) =>
    button.textContent?.includes("Typography implementation plan"),
  );
  if (!trigger) throw new Error("Missing real plan toggle.");
  expect(container.querySelector(".chat-markdown")).toBeNull();
  await act(async () => trigger.click());
  expect(container.querySelector(".chat-markdown")?.textContent).toContain(
    "The expanded plan uses the real ChatMarkdown child.",
  );
});
it("opens the real model picker popup from its trigger", async () => {
  const container = await mount(fixture("chat-provider-picker"));
  const trigger = container.querySelector<HTMLButtonElement>("[data-chat-provider-model-picker]");
  if (!trigger) throw new Error("Missing real provider picker trigger.");
  expect(trigger.getAttribute("aria-expanded")).toBe("false");
  await act(async () => trigger.click());
  expect(trigger.getAttribute("aria-expanded")).toBe("true");
  expect(document.querySelector("[data-model-picker-content]")).not.toBeNull();
  expect(document.querySelector('[role="combobox"]')).not.toBeNull();
});
