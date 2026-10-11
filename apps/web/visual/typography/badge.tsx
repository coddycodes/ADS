import { Badge } from "../../src/components/ui/badge";
import type { TypographyFixture } from "./types";

export const badgeFixture: TypographyFixture = {
  id: "badge",
  title: "Badge sizes and states",
  modules: ["components/ui/badge.tsx"],
  owners: ["Badge"],
  anchors: [
    "Small unread badge",
    "Default unread badge",
    "Large unread badge",
    "Ready",
    "Unavailable",
    "Retry needed",
  ],
  render: () => (
    <div className="flex flex-wrap items-center gap-4">
      <Badge size="sm">Small unread badge</Badge>
      <Badge>Default unread badge</Badge>
      <Badge size="lg">Large unread badge</Badge>
      <Badge variant="success">Ready</Badge>
      <Badge variant="outline">Unavailable</Badge>
      <Badge variant="error">Retry needed</Badge>
    </div>
  ),
};
