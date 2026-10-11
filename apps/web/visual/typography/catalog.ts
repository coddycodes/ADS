import type { TypographyFixture } from "./types";

export function requireFixture(
  fixtures: readonly TypographyFixture[],
  id: string,
): TypographyFixture {
  const ids = new Set<string>();
  for (const fixture of fixtures) {
    if (
      !/^[a-z0-9-]{1,80}$/.test(fixture.id) ||
      ids.has(fixture.id) ||
      !fixture.title.trim() ||
      [fixture.modules, fixture.owners, fixture.anchors].some(
        (items) => items.length === 0 || items.some((item) => !item.trim()),
      )
    ) {
      throw new Error("Typography fixture definition refused.");
    }
    ids.add(fixture.id);
  }
  const selected = fixtures.find((fixture) => fixture.id === id);
  if (!selected) throw new Error("Typography fixture selection refused.");
  return selected;
}
