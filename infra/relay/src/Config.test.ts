import { expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Redacted from "effect/Redacted";

import * as RelayConfiguration from "./Config.ts";

const configuration = {
  relayIssuer: "https://relay.example.test",
  clerkSecretKey: Redacted.make("clerk-secret"),
  clerkPublishableKey: "pk_test_test",
  clerkJwtAudience: "coddy-studio-relay",
  cloudMintPrivateKey: Redacted.make("private-key"),
  cloudMintPublicKey: "public-key",
  managedEndpointBaseDomain: undefined,
  managedEndpointNamespace: undefined,
} satisfies RelayConfiguration.RelayConfiguration["Service"];

it.effect("provides relay configuration through its Context service layer", () =>
  Effect.gen(function* () {
    const fromMake = RelayConfiguration.make(configuration);
    expect(fromMake).toBe(configuration);

    const provided = yield* RelayConfiguration.RelayConfiguration;
    expect(provided).toBe(configuration);
    expect(Redacted.value(provided.clerkSecretKey)).toBe("clerk-secret");
    expect(Redacted.value(provided.cloudMintPrivateKey)).toBe("private-key");
  }).pipe(Effect.provide(RelayConfiguration.layer(configuration))),
);
