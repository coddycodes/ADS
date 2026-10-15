import { createFileRoute, redirect } from "@tanstack/react-router";
import { useEffect, useRef, useState } from "react";

import {
  HostedPairingRouteSurface,
  PairingPendingSurface,
  PairingRouteSurface,
} from "../components/auth/PairingRouteSurface";
import { extractEmbeddedPairingToken } from "../components/auth/pairingCodeCredential";

export const Route = createFileRoute("/pair")({
  validateSearch: (search: Record<string, unknown>) => ({
    ...(typeof search.code === "string" && search.code.length > 0 ? { code: search.code } : {}),
    ...(typeof search.host === "string" && search.host.length > 0 ? { host: search.host } : {}),
    ...(typeof search.label === "string" && search.label.length > 0 ? { label: search.label } : {}),
  }),
  beforeLoad: async ({ context, search }) => {
    const { authGateState } = context;
    if (authGateState.status === "hosted-pairing") {
      return {
        authGateState,
      };
    }

    if (authGateState.status === "authenticated" || authGateState.status === "hosted-static") {
      if (search?.code !== undefined) {
        throw redirect({
          to: "/settings/remote-servers",
          search: { code: search.code },
          replace: true,
        });
      }
      throw redirect({ to: "/", replace: true });
    }
    return {
      authGateState,
    };
  },
  component: PairRouteView,
  pendingComponent: PairRoutePendingView,
});

function PairRouteView() {
  const { authGateState } = Route.useRouteContext();
  const search = Route.useSearch();
  const [retainedCode, setRetainedCode] = useState(search.code);
  const scrubbedQueryCodeRef = useRef<string | null>(null);
  const [consumedCodes, setConsumedCodes] = useState<ReadonlySet<string>>(() => new Set());
  const navigate = Route.useNavigate();
  if (search.code !== undefined && search.code !== retainedCode) {
    setRetainedCode(search.code);
  }
  const code = search.code ?? retainedCode;

  useEffect(() => {
    if (search.code === undefined) {
      scrubbedQueryCodeRef.current = null;
      return;
    }
    if (scrubbedQueryCodeRef.current === search.code) return;
    scrubbedQueryCodeRef.current = search.code;
    void navigate({
      search: {
        ...(search.host === undefined ? {} : { host: search.host }),
        ...(search.label === undefined ? {} : { label: search.label }),
      },
      replace: true,
    });
  }, [navigate, search.code, search.host, search.label]);

  if (!authGateState) {
    return null;
  }

  if (authGateState.status === "hosted-pairing") {
    return <HostedPairingRouteSurface />;
  }

  const initialCredential =
    code === undefined || consumedCodes.has(code) ? null : extractEmbeddedPairingToken(code);

  return (
    <PairingRouteSurface
      key={code ?? "manual"}
      auth={authGateState.auth}
      {...(initialCredential === null ? {} : { initialCredential })}
      {...(code === undefined
        ? {}
        : {
            onInitialCredentialConsumed: () => {
              setConsumedCodes((current) =>
                current.has(code) ? current : new Set(current).add(code),
              );
            },
          })}
      onAuthenticated={() => {
        void navigate({ to: "/", replace: true });
      }}
      {...(authGateState.errorMessage ? { initialErrorMessage: authGateState.errorMessage } : {})}
    />
  );
}

function PairRoutePendingView() {
  return <PairingPendingSurface />;
}
