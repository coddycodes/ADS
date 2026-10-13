# BiBCode Connect Relay

> [!WARNING]
> BiBCode Connect is currently in private beta. Join the waitlist in the app under Settings > BiBCode Connect.

The relay is the hosted control plane for BiBCode Connect. It helps clients discover and connect to
remote environments and manages the cloud-side records needed for those connections.

The relay is intentionally not in the hot path for normal BiBCode traffic. After a client connects,
regular API and WebSocket traffic goes directly between that client and the selected environment.
See the [BiBCode Connect setup](../../docs/cloud/bibcode-connect-clerk.md) for the larger
authentication and deployment context.

## Responsibilities

The relay currently owns:

- Linking BiBCode environments to a cloud account.
- Provisioning and tracking managed environment endpoints.
- Issuing short-lived credentials used to connect clients to linked environments.
- Listing linked environments for an account.
- Persisting relay state.

Each managed endpoint is one Cloudflare tunnel with one hostname,
`<stage>-<hash>.<tunnel zone>`, plus 4 preview hostnames (`<stage>-<hash>-p1` to `-p4`) that
serve the preview gateway over BiBCode Connect. Provisioning creates a proxied CNAME and an
ingress rule for each, so an environment uses 5 DNS records in the tunnel zone. Preview records
are found by name rather than stored, so re-linking an environment adds any that are missing and
deprovisioning removes them before the tunnel. See
[Preview gateway](../../docs/architecture/remote.md#preview-gateway).

The environment server and relay have separate credentials and trust boundaries. Read
[Environment Authentication Profile](../../docs/cloud/environment-auth.md) before changing token,
credential, or authorization behavior.

## Code Map

- [`alchemy.run.ts`](./alchemy.run.ts) defines the deployed Alchemy stack.
- [`src/worker.ts`](./src/worker.ts) wires Cloudflare bindings, runtime layers, queues, and HTTP APIs.
- [`src/http/Api.ts`](./src/http/Api.ts) contains the relay HTTP handlers and authentication
  boundaries.
- [`src/environments`](./src/environments) contains environment linking, credentials, endpoint
  provisioning, and connection flows.
- [`src/auth`](./src/auth) contains relay token and DPoP proof handling.
- [`src/persistence/schema.ts`](./src/persistence/schema.ts) defines persisted relay state. Keep
  schema and migration changes together.

Shared request and response schemas live in
[`packages/contracts/src/relay.ts`](../../packages/contracts/src/relay.ts). Shared client-side relay
calls live in
[`packages/client-runtime/src/relay/managedRelay.ts`](../../packages/client-runtime/src/relay/managedRelay.ts).

## Working Locally

Install dependencies from the repository root, then run relay-focused checks from this directory:

```sh
vp install
cd infra/relay
vp test run
vp run typecheck
```

To run a smaller test set while iterating:

```sh
vp test run src/environments/EnvironmentLinker.test.ts
```

Before considering a change complete, run the repository-wide checks from the root:

```sh
vp check
vp run typecheck
```

Backend changes should include tests. Prefer testing the real business logic with external
dependencies represented at their boundary rather than mocking internal behavior.

## Deployment

The relay deploys through Alchemy:

```sh
vp run --filter bibcode-relay deploy
```

The stack provisions the Cloudflare Worker and queues, managed endpoint resources, and database
connectivity. Copy [`infra/relay/.env.example`](./.env.example) to
`infra/relay/.env` and fill in the deployment-specific values before deploying. Alchemy loads that
file from the relay directory. Runtime secrets include Clerk credentials. Production adopts the
configured API and tunnel DNS zones as retained Cloudflare resources. Personal stages reference the
production-owned zones.

The `prod` Alchemy stage owns the retained PlanetScale database and is the shared hosted relay for
stable and nightly clients. Every other stage references that database and provisions an isolated
PlanetScale branch and runtime role for local development, so deploy `prod` before creating
developer stages:

```sh
vp run --filter bibcode-relay deploy -- --stage prod
vp run --filter bibcode-relay deploy -- --env-file .env.local
```

Alchemy defaults personal deployments to the `dev_$USER` stage. Relay custom domains apply the same
DNS-safe sanitization as Alchemy physical resource names, so `prod` uses
`relay.<RELAY_API_ZONE_NAME>` and `dev_julius` uses
`relay-dev-julius.<RELAY_API_ZONE_NAME>`. Managed environment endpoints are provisioned below
`RELAY_TUNNEL_ZONE_NAME`, which may be a different Cloudflare zone. Production tunnel hostnames use
`prod-<digest>.<RELAY_TUNNEL_ZONE_NAME>`; personal stages use
`<stage>-<digest>.<RELAY_TUNNEL_ZONE_NAME>`. `RELAY_DOMAIN` remains available as an explicit API
domain override.

After a successful deploy, the wrapper updates the repository-root `.env` file with the derived relay
URL. That makes subsequent source builds point at the relay that was just deployed without copying
the URL manually.

### Deployment CI

The relay is versioned separately from client releases. `.github/workflows/deploy-relay.yml` deploys
the shared Alchemy `prod` stage on every push to `main`. Stable and nightly release builds both
resolve their static public config from the same
`production` GitHub environment. Pull requests do not deploy relay stages. Developers can
deploy personal non-production stages locally with any stage name other than `prod`.

The repository must define these Actions variables shared by relay deployments:

- `CLOUDFLARE_ACCOUNT_ID`
- `PLANETSCALE_ORGANIZATION`

The repository must define these Actions secrets shared by relay deployments:

- `CLOUDFLARE_API_TOKEN`
- `PLANETSCALE_API_TOKEN_ID`
- `PLANETSCALE_API_TOKEN`

The `production` GitHub environment must define these Actions variables:

- `RELAY_API_ZONE_NAME`
- `RELAY_TUNNEL_ZONE_NAME`
- `RELAY_DOMAIN` if overriding the derived production relay domain
- `CLERK_PUBLISHABLE_KEY`
- `CLERK_JWT_AUDIENCE`
- `CLERK_JWT_TEMPLATE`

The `production` GitHub environment must define these Actions secrets:

- `CLERK_SECRET_KEY`

The account-scoped repository credentials are consumed by Alchemy while provisioning relay stages; they
are not bound into the relay Worker. The release workflow reads the production relay's derived
public URL and Clerk publishable key from the same environment for downstream desktop, CLI, and
hosted web builds.

See:

- [BiBCode Connect Clerk Setup](../../docs/cloud/bibcode-connect-clerk.md) for Clerk keys, JWT templates, and waitlist
  setup.
- [Remote Architecture](../../docs/architecture/remote.md) for environment, access, and endpoint flows.
