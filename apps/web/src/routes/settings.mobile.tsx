import { createFileRoute } from "@tanstack/react-router";

import { MobileSettingsPanel } from "../components/settings/DevicePreviewsSection";

export const Route = createFileRoute("/settings/mobile")({
  component: MobileSettingsPanel,
});
