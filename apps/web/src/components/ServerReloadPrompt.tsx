import { useState } from "react";
import { usePrimaryEnvironment } from "../state/environments";
import { isDesktopHost } from "../env";
import { APP_VERSION } from "../branding";
import { serverReloadVersion, type ServerBoot } from "../serverReload.logic";
import { Button } from "./ui/button";

export function ServerReloadPrompt() {
  const primary = usePrimaryEnvironment();
  const descriptor = primary?.serverConfig?.environment ?? null;
  const current: ServerBoot | null =
    descriptor === null
      ? null
      : { bootId: descriptor.bootId, serverVersion: descriptor.serverVersion };
  const [firstBoot, setFirstBoot] = useState<ServerBoot | null>(() => current);
  // Bootstrap may arrive after mount; capture it once without writing refs during render.
  if (firstBoot === null && current !== null) setFirstBoot(current);
  const version = serverReloadVersion({
    desktop: isDesktopHost,
    bundleVersion: APP_VERSION,
    firstBoot,
    current,
  });
  if (version === null) return null;
  return (
    <div
      role="status"
      className="flex flex-wrap items-center justify-center gap-3 border-b border-border bg-background px-4 py-2 text-sm"
    >
      <span>Coddy Studio on this server was updated to v{version}. Reload to use it.</span>
      <Button size="xs" onClick={() => window.location.reload()}>
        Reload
      </Button>
    </div>
  );
}
