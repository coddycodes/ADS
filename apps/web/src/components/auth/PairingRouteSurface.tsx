import type { AuthSessionState } from "@coddy/contracts";
import { squashAtomCommandFailure } from "@coddy/client-runtime/state/runtime";
import React, { startTransition, useEffect, useRef, useState, useCallback } from "react";

import { APP_DISPLAY_NAME } from "../../branding";
import { connectPairing } from "../../connection/onboarding";
import {
  peekPairingTokenFromUrl,
  stripPairingTokenFromUrl,
  submitServerAuthCredential,
} from "../../environments/primary";
import { readHostedPairingRequest } from "../../hostedPairing";
import { Button } from "../ui/button";
import { Input } from "../ui/input";
import { useAtomCommand } from "../../state/use-atom-command";

export function PairingPendingSurface() {
  return (
    <div className="relative flex min-h-screen items-center justify-center overflow-hidden bg-background px-4 py-10 text-foreground sm:px-6">
      <div className="pointer-events-none absolute inset-0 opacity-80">
        <div className="absolute inset-x-0 top-0 h-44 bg-[radial-gradient(44rem_16rem_at_top,color-mix(in_srgb,var(--color-emerald-500)_14%,transparent),transparent)]" />
        <div className="absolute inset-y-0 left-0 w-72 bg-[radial-gradient(28rem_18rem_at_left,color-mix(in_srgb,var(--color-sky-500)_10%,transparent),transparent)]" />
        <div className="absolute inset-0 bg-[linear-gradient(145deg,color-mix(in_srgb,var(--background)_90%,var(--color-black))_0%,var(--background)_55%)]" />
      </div>

      <section className="relative w-full max-w-xl rounded-2xl border border-border/80 bg-card/90 p-6 shadow-2xl shadow-black/20 backdrop-blur-md sm:p-8">
        <p className="text-xs font-semibold tracking-[0.18em] text-muted-foreground uppercase">
          {APP_DISPLAY_NAME}
        </p>
        <h1 className="mt-3 text-2xl font-semibold tracking-tight sm:text-3xl">
          Pairing with this environment
        </h1>
        <p className="mt-2 text-sm leading-relaxed text-muted-foreground">
          Validating the pairing link and preparing your session.
        </p>
      </section>
    </div>
  );
}

export function PairingRouteSurface({
  auth,
  initialCredential,
  initialErrorMessage,
  onInitialCredentialConsumed,
  onAuthenticated,
}: {
  auth: AuthSessionState["auth"];
  initialCredential?: string;
  initialErrorMessage?: string;
  onInitialCredentialConsumed?: () => void;
  onAuthenticated: () => void;
}) {
  const [autoPairToken] = useState(() => peekPairingTokenFromUrl() ?? initialCredential ?? null);
  const [credential, setCredential] = useState(autoPairToken ?? "");
  const [errorMessage, setErrorMessage] = useState(initialErrorMessage ?? "");
  const [isSubmitting, setIsSubmitting] = useState(false);
  const autoSubmitAttemptedRef = useRef(false);

  const submitCredential = useCallback(
    async (nextCredential: string) => {
      setIsSubmitting(true);
      setErrorMessage("");

      const submitError = await submitServerAuthCredential(nextCredential).then(
        () => null,
        (error) => errorMessageFromUnknown(error),
      );

      setIsSubmitting(false);

      if (submitError) {
        setErrorMessage(submitError);
        return;
      }

      startTransition(() => {
        onAuthenticated();
      });
    },
    [onAuthenticated],
  );

  const handleSubmit = useCallback(
    async (event?: React.SubmitEvent<HTMLFormElement>) => {
      event?.preventDefault();
      await submitCredential(credential);
    },
    [submitCredential, credential],
  );

  useEffect(() => {
    const token = autoPairToken;
    if (!token || autoSubmitAttemptedRef.current) {
      return;
    }

    autoSubmitAttemptedRef.current = true;
    stripPairingTokenFromUrl();
    onInitialCredentialConsumed?.();
    void submitCredential(token);
  }, [autoPairToken, onInitialCredentialConsumed, submitCredential]);

  return (
    <div className="relative flex min-h-screen items-center justify-center overflow-hidden bg-background px-4 py-10 text-foreground sm:px-6">
      <div className="pointer-events-none absolute inset-0 opacity-80">
        <div className="absolute inset-x-0 top-0 h-44 bg-[radial-gradient(44rem_16rem_at_top,color-mix(in_srgb,var(--color-emerald-500)_14%,transparent),transparent)]" />
        <div className="absolute inset-y-0 left-0 w-72 bg-[radial-gradient(28rem_18rem_at_left,color-mix(in_srgb,var(--color-sky-500)_10%,transparent),transparent)]" />
        <div className="absolute inset-0 bg-[linear-gradient(145deg,color-mix(in_srgb,var(--background)_90%,var(--color-black))_0%,var(--background)_55%)]" />
      </div>

      <section className="relative w-full max-w-xl rounded-2xl border border-border/80 bg-card/90 p-6 shadow-2xl shadow-black/20 backdrop-blur-md sm:p-8">
        <p className="text-xs font-semibold tracking-[0.18em] text-muted-foreground uppercase">
          {APP_DISPLAY_NAME}
        </p>
        <h1 className="mt-3 text-2xl font-semibold tracking-tight sm:text-3xl">
          Pair with this environment
        </h1>
        <p className="mt-2 text-sm leading-relaxed text-muted-foreground">
          {describeAuthGate(auth.bootstrapMethods)}
        </p>

        <form className="mt-6 space-y-4" onSubmit={(event) => void handleSubmit(event)}>
          <div className="space-y-2">
            <label className="text-sm font-medium" htmlFor="pairing-token">
              Pairing token
            </label>
            <Input
              id="pairing-token"
              autoCapitalize="none"
              autoComplete="off"
              autoCorrect="off"
              disabled={isSubmitting}
              nativeInput
              onChange={(event) => setCredential(event.currentTarget.value)}
              placeholder="Paste a one-time token or pairing secret"
              spellCheck={false}
              value={credential}
            />
          </div>

          {errorMessage ? (
            <div className="rounded-lg border border-destructive/30 bg-destructive/6 px-3 py-2 text-sm text-destructive">
              {errorMessage}
            </div>
          ) : null}

          <div className="flex flex-wrap gap-2">
            <Button disabled={isSubmitting} size="sm" type="submit">
              {isSubmitting ? "Pairing..." : "Continue"}
            </Button>
            <Button
              disabled={isSubmitting}
              onClick={() => window.location.reload()}
              size="sm"
              variant="outline"
            >
              Reload app
            </Button>
          </div>
        </form>

        <div className="mt-6 rounded-lg border border-border/70 bg-background/55 px-3 py-3 text-xs leading-relaxed text-muted-foreground">
          {describeSupportedMethods(auth.bootstrapMethods)}
        </div>
      </section>
    </div>
  );
}

const MISSING_HOSTED_PAIRING_MESSAGE =
  "This pairing link is missing its backend host or token. Open the complete link again, or create a new pairing link on the backend.";

export function HostedPairingRouteSurface() {
  const connectPairingEnvironment = useAtomCommand(connectPairing, {
    reportFailure: false,
  });
  const [request] = useState(readHostedPairingRequest);
  const [status, setStatus] = useState<"confirm" | "pairing" | "paired" | "error">(() =>
    request ? "confirm" : "error",
  );
  const [errorKind, setErrorKind] = useState<"missing" | "resubmitted" | "connection" | null>(() =>
    request ? null : "missing",
  );
  const [message, setMessage] = useState(() =>
    request
      ? "Review the backend address before submitting this one-time pairing token."
      : MISSING_HOSTED_PAIRING_MESSAGE,
  );
  const tokenStrippedRef = useRef(false);
  const tokenSubmittedRef = useRef(false);

  const submitHostedPairingRequest = useCallback(async () => {
    if (!request) {
      setStatus("error");
      setErrorKind("missing");
      setMessage(MISSING_HOSTED_PAIRING_MESSAGE);
      return;
    }

    if (tokenSubmittedRef.current) {
      setStatus("error");
      setErrorKind("resubmitted");
      setMessage("This one-time pairing token was already submitted. Request a new pairing link.");
      return;
    }

    setStatus("pairing");
    setErrorKind(null);
    setMessage("Connecting to this backend.");
    tokenSubmittedRef.current = true;

    const result = await connectPairingEnvironment({
      host: request.httpBaseUrl,
      pairingCode: request.token,
    });
    if (result._tag === "Success") {
      setStatus("paired");
      setMessage(`${request.label || "The environment"} is saved in this browser.`);
      return;
    }

    setStatus("error");
    setErrorKind("connection");
    setMessage(
      `${errorMessageFromUnknown(squashAtomCommandFailure(result))} This one-time token may already have been accepted; request a new pairing link before trying again.`,
    );
  }, [connectPairingEnvironment, request]);

  useEffect(() => {
    if (tokenStrippedRef.current) {
      return;
    }
    tokenStrippedRef.current = true;

    stripPairingTokenFromUrl();
  }, []);

  return (
    <div className="relative flex min-h-screen items-center justify-center overflow-hidden bg-background px-4 py-10 text-foreground sm:px-6">
      <div className="pointer-events-none absolute inset-0 opacity-80">
        <div className="absolute inset-x-0 top-0 h-44 bg-[radial-gradient(44rem_16rem_at_top,color-mix(in_srgb,var(--color-emerald-500)_14%,transparent),transparent)]" />
        <div className="absolute inset-y-0 left-0 w-72 bg-[radial-gradient(28rem_18rem_at_left,color-mix(in_srgb,var(--color-sky-500)_10%,transparent),transparent)]" />
        <div className="absolute inset-0 bg-[linear-gradient(145deg,color-mix(in_srgb,var(--background)_90%,var(--color-black))_0%,var(--background)_55%)]" />
      </div>

      <section className="relative w-full max-w-xl rounded-2xl border border-border/80 bg-card/90 p-6 shadow-2xl shadow-black/20 backdrop-blur-md sm:p-8">
        <p className="text-xs font-semibold tracking-[0.18em] text-muted-foreground uppercase">
          {APP_DISPLAY_NAME}
        </p>
        <h1 className="mt-3 text-2xl font-semibold tracking-tight sm:text-3xl">
          {status === "paired"
            ? "Backend paired"
            : status === "error"
              ? "Pairing failed"
              : status === "pairing"
                ? "Pairing backend"
                : "Pair this backend"}
        </h1>
        <p className="mt-2 text-sm leading-relaxed text-muted-foreground">{message}</p>

        {request ? (
          <div className="mt-5 rounded-lg border border-border/70 bg-background/55 px-3 py-3 text-xs leading-relaxed text-muted-foreground">
            Host: <span className="font-mono text-foreground/80">{request.displayHost}</span>
          </div>
        ) : null}

        {status === "error" && errorKind === "connection" ? (
          <div className="mt-5 rounded-lg border border-destructive/30 bg-destructive/6 px-3 py-2 text-sm text-destructive">
            Verify the backend is reachable from this browser, supports CORS for hosted clients, and
            is served over HTTPS when opening this page from HTTPS.
          </div>
        ) : null}

        {status !== "error" ? (
          <div className="mt-6 flex flex-wrap gap-2">
            {status === "confirm" ? (
              <Button size="sm" onClick={() => void submitHostedPairingRequest()}>
                Pair this backend
              </Button>
            ) : status === "pairing" ? (
              <Button disabled size="sm">
                Pairing...
              </Button>
            ) : null}
            {status === "paired" ? (
              <Button size="sm" variant="outline" onClick={() => (window.location.href = "/")}>
                Open app
              </Button>
            ) : null}
          </div>
        ) : null}
      </section>
    </div>
  );
}

function errorMessageFromUnknown(error: unknown): string {
  if (error instanceof Error && error.message.trim().length > 0) {
    return error.message;
  }

  if (typeof error === "string" && error.trim().length > 0) {
    return error;
  }

  return "Authentication failed.";
}

function describeAuthGate(bootstrapMethods: ReadonlyArray<string>): string {
  if (bootstrapMethods.includes("desktop-bootstrap")) {
    return "This environment expects a trusted pairing credential before the app can connect.";
  }

  return "Enter a pairing token to start a session with this environment.";
}

function describeSupportedMethods(bootstrapMethods: ReadonlyArray<string>): string {
  if (
    bootstrapMethods.includes("desktop-bootstrap") &&
    bootstrapMethods.includes("one-time-token")
  ) {
    return "Desktop-managed pairing and one-time pairing tokens are both accepted for this environment.";
  }

  if (bootstrapMethods.includes("desktop-bootstrap")) {
    return "This environment is desktop-managed. Open it from the desktop app or paste a bootstrap credential if one was issued explicitly.";
  }

  return "This environment accepts one-time pairing tokens. Pairing links can open this page directly, or you can paste the token here.";
}
