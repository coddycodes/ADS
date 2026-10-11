// @vitest-environment happy-dom
import { expect, it } from "vite-plus/test";
import { typographyFixtures } from "./fixtures";
import required from "./required-coverage.json";
import { requireFixture } from "./catalog";

it("maps every required typography module and render owner to actual component fixtures", () => {
  for (const { module, owners } of required) {
    const fixtures = typographyFixtures.filter((fixture) => fixture.modules.includes(module));
    expect(fixtures.length, module).toBeGreaterThan(0);
    for (const owner of owners) {
      expect(
        fixtures.some((fixture) => fixture.owners.includes(owner)),
        `${module}: ${owner}`,
      ).toBe(true);
    }
  }
  for (const fixture of typographyFixtures)
    expect(requireFixture(typographyFixtures, fixture.id)).toBe(fixture);
});
