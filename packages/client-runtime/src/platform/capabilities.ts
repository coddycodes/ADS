import {
  type AuthClientPresentationMetadata,
  type AuthEnvironmentScope,
  type DesktopSshEnvironmentBootstrap,
  type DesktopSshEnvironmentTarget,
  EnvironmentId,
} from "@coddy/contracts";
import * as Context from "effect/Context";
import type * as Effect from "effect/Effect";
import type * as Option from "effect/Option";

import type { ConnectionAttemptError } from "../connection/model.ts";

export interface PreparedSshEnvironment {
  readonly bootstrap: DesktopSshEnvironmentBootstrap;
  readonly bearerToken: string;
}

export interface ProvisionedSshEnvironment extends PreparedSshEnvironment {
  readonly environmentId: EnvironmentId;
  readonly label: string;
}

export class CloudSession extends Context.Service<
  CloudSession,
  {
    readonly clerkToken: Effect.Effect<string, ConnectionAttemptError>;
  }
>()("@coddy/client-runtime/platform/capabilities/CloudSession") {}

export class ClientPresentation extends Context.Service<
  ClientPresentation,
  {
    readonly metadata: AuthClientPresentationMetadata;
    readonly scopes: ReadonlyArray<AuthEnvironmentScope>;
  }
>()("@coddy/client-runtime/platform/capabilities/ClientPresentation") {}

export class PrimaryEnvironmentAuth extends Context.Service<
  PrimaryEnvironmentAuth,
  {
    readonly bearerToken: Effect.Effect<Option.Option<string>, ConnectionAttemptError>;
  }
>()("@coddy/client-runtime/platform/capabilities/PrimaryEnvironmentAuth") {}

export interface SshEnvironmentConnectionInput {
  readonly connectionId: string;
  readonly expectedEnvironmentId: EnvironmentId;
  readonly target: DesktopSshEnvironmentTarget;
}

export class SshEnvironmentGateway extends Context.Service<
  SshEnvironmentGateway,
  {
    /** Add: launch, tunnel, mint, fetch the descriptor, then exchange. */
    readonly provision: (
      target: DesktopSshEnvironmentTarget,
    ) => Effect.Effect<ProvisionedSshEnvironment, ConnectionAttemptError>;
    /**
     * Reconnect: reuses a live tunnel (no SSH command) or launches and
     * tunnels again. The bootstrap never carries a pairing token.
     */
    readonly ensureTunnel: (
      input: SshEnvironmentConnectionInput,
    ) => Effect.Effect<DesktopSshEnvironmentBootstrap, ConnectionAttemptError>;
    /** Mints a fresh one-time credential over SSH and exchanges it for a bearer. */
    readonly mintBearer: (
      input: SshEnvironmentConnectionInput,
    ) => Effect.Effect<PreparedSshEnvironment, ConnectionAttemptError>;
    readonly disconnect: (
      target: DesktopSshEnvironmentTarget,
    ) => Effect.Effect<void, ConnectionAttemptError>;
  }
>()("@coddy/client-runtime/platform/capabilities/SshEnvironmentGateway") {}
