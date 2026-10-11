import { expect, it } from "vite-plus/test";
import { renderToStaticMarkup } from "react-dom/server";
import { badgeFixture } from "./badge";
import { requireFixture } from "./catalog";

it("admits the actual badge fixture and renders all named text states", () => {
  const fixture = requireFixture([badgeFixture], "badge");
  const markup = renderToStaticMarkup(fixture.render());
  for (const text of badgeFixture.anchors) expect(markup).toContain(text);
  expect(markup).toContain('data-slot="badge"');
  expect(markup).toContain("text-xs");
});
it("refuses unknown, duplicate and incomplete fixture definitions", () => {
  expect(() => requireFixture([badgeFixture], "unknown")).toThrow();
  expect(() => requireFixture([badgeFixture, badgeFixture], "badge")).toThrow();
  expect(() => requireFixture([{ ...badgeFixture, anchors: [] }], "badge")).toThrow();
});
