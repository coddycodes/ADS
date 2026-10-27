// @effect-diagnostics anyUnknownInErrorContext:off layerMergeAllWithDependencies:off - Alchemy provider helpers expose framework-owned any requirements.
import * as Alchemy from "alchemy";
import * as Cloudflare from "alchemy/Cloudflare";
import * as Drizzle from "alchemy/Drizzle/Providers";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Planetscale from "alchemy/Planetscale";

import * as RelayDb from "./src/db.ts";
import { ManagedEndpointZone, RelayApiZone } from "./src/zone.ts";
import ApiLive, { Api } from "./src/worker.ts";

export default Alchemy.Stack(
  "CoddyStudioRelay",
  {
    providers: Layer.mergeAll(Cloudflare.providers(), Drizzle.providers(), Planetscale.providers()),
    state: Cloudflare.state(),
  },
  Effect.gen(function* () {
    const db = yield* RelayDb.PlanetscaleDatabase;
    const hyperdrive = yield* RelayDb.RelayHyperdrive;
    const managedEndpointZone = yield* ManagedEndpointZone;
    const relayApiZone = yield* RelayApiZone;
    const api = yield* Api;

    return {
      databaseName: db.database.name,
      databaseBranchName: db.branch?.name ?? "main",
      hyperdriveName: hyperdrive.name,
      workerName: api.workerName,
      url: api.url,
      relayApiZoneId: relayApiZone.zoneId,
      managedEndpointZoneId: managedEndpointZone.zoneId,
    };
  }).pipe(Effect.provide(ApiLive)),
);
