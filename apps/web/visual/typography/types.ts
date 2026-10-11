import type { ReactNode } from "react";

export interface TypographyFixture {
  readonly id: string;
  readonly title: string;
  readonly modules: readonly string[];
  readonly owners: readonly string[];
  readonly anchors: readonly string[];
  readonly render: () => ReactNode;
}
export type Fixture = TypographyFixture;
