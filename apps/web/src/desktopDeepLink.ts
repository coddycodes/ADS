import { useNavigate } from "@tanstack/react-router";
import { resolvePairingDeepLinkCode } from "@coddy/shared/pairingCode";
import { useEffect } from "react";

export function resolvePairingDeepLink(rawUrl: string): { readonly code: string } | null {
  const code = resolvePairingDeepLinkCode(rawUrl);
  return code === null ? null : { code };
}

/** Mounted once by the root route in desktop mode; renders nothing. */
export function DesktopDeepLinkRouter() {
  const navigate = useNavigate();

  useEffect(() => {
    const bridge = window.desktopBridge;
    if (!bridge?.onDeepLink) return;
    const handleUrls = (urls: ReadonlyArray<string>) => {
      for (const rawUrl of urls) {
        const pairing = resolvePairingDeepLink(rawUrl);
        if (pairing !== null) {
          void navigate({ to: "/pair", search: { code: pairing.code } });
          return;
        }
      }
    };
    void bridge
      .getPendingDeepLinks?.()
      .then(handleUrls)
      .catch(() => undefined);
    return bridge.onDeepLink(handleUrls);
  }, [navigate]);

  return null;
}
