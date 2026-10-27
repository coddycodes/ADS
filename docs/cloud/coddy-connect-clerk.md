# Coddy Connect Clerk setup

Coddy Connect uses one Clerk application for browser and desktop sign-in. The
relay accepts Clerk JWTs only from the configured issuer and audience. Clients
then exchange a Clerk token plus a DPoP proof for relay-scoped access; Clerk
tokens are not environment credentials.

## Client configuration

Cloud UI is disabled in a fresh clone. To enable it for source builds, add these
public values to the repository-root `.env.local` or `.env`:

```dotenv
CODDY_STUDIO_CLERK_PUBLISHABLE_KEY=<publishable key>
CODDY_STUDIO_CLERK_JWT_TEMPLATE=coddy-studio-relay
CODDY_STUDIO_RELAY_URL=https://relay.example.com
```

The shared loader projects canonical `CODDY_STUDIO_*` values into the `VITE_*`
aliases consumed by the web build. Precedence is:

1. process or CI environment variables;
2. repository-root `.env.local`;
3. repository-root `.env`.

These three values are public build-time configuration, not secrets. Web and
desktop builds omit Connect UI unless all three are valid. A built desktop
artifact does not need an environment file at runtime.

The publishable-key parser accepts only canonical lowercase ASCII DNS names
for Clerk's Frontend API. It rejects Unicode input, URL-normalized spellings,
and every `xn--` A-label before constructing a URL so browser and operating-
system WebView IDNA differences cannot widen the trusted host. This conservative
rule means IDN custom Clerk Frontend API domains, including valid Punycode
spellings, are unsupported until Coddy Studio has one deterministic cross-runtime
IDNA validator.

Never expose `CLERK_SECRET_KEY` in a client environment, desktop artifact, or
repository file.

## Clerk JWT template

Create a template under **Clerk Dashboard > JWT templates**:

| Setting | Value                        |
| ------- | ---------------------------- |
| Name    | `coddy-studio-relay`              |
| Claims  | `{ "aud": "coddy-studio-relay" }` |

Set `CODDY_STUDIO_CLERK_JWT_TEMPLATE=coddy-studio-relay` for the client and
`CLERK_JWT_AUDIENCE=coddy-studio-relay` for the relay. The stable audience is shared
across deployment stages; `CODDY_STUDIO_RELAY_URL` chooses the concrete relay.

## Relay deployment

Copy `infra/relay/.env.example` to `infra/relay/.env` for a local deployment.
The relay reads `RELAY_DOMAIN`, `RELAY_API_ZONE_NAME`,
`RELAY_TUNNEL_ZONE_NAME`, `CLERK_PUBLISHABLE_KEY`, and
`CLERK_JWT_AUDIENCE` through Effect `Config`. `CLERK_SECRET_KEY` is supplied as
an Alchemy secret.

`vp run --filter coddy-studio-relay deploy` runs Alchemy from `infra/relay`, so that
directory's environment file is loaded. After deployment, the wrapper writes
the deployed HTTPS relay URL to the repository-root environment configuration.

The `prod` Alchemy stage owns the retained PlanetScale database.
Non-production stages reference that database and provision isolated branches;
deploy `prod` before a personal developer stage.

## Client-driven link flow

Linking is initiated from the Connections settings in the running React client:

1. Clerk supplies a JWT from the configured template.
2. The client ensures the environment's managed relay binary is available.
3. The client requests a relay link challenge.
4. The local environment signs the challenge and endpoint descriptor.
5. The client submits the proof to the relay.
6. The relay provisions a managed endpoint and returns signed runtime
   configuration.
7. The client stores that configuration in the environment through its
   authenticated Connect API.

Unlinking clears the environment configuration first, then best-effort revokes
the cloud link. Environment link state is durable server state and is
reconciled when the server runs. The implementation entry points are
[`apps/web/src/cloud/linkEnvironment.ts`](../../apps/web/src/cloud/linkEnvironment.ts)
and
[`apps/server/src/production/connect_mcp.rs`](../../apps/server/src/production/connect_mcp.rs).

See [Coddy Connect auth flow](./coddy-connect-auth-flow.md) for the relay and
DPoP connection sequence.

## Desktop authentication

The Tauri desktop loads the same React app and `@clerk/react` provider as the
browser build. It does not use an Electron adapter or Electron token storage.
The production application identifier is `com.coddy.studio.desktop`.

Current Clerk flows must work inside the operating-system WebView. External-
browser native callbacks, custom `coddy-studio://` redirects, and native desktop
passkeys are not implemented. Do not claim them until a Tauri-specific secure
token transport, platform entitlements, and end-to-end tests exist.

## Private beta access

For request-and-approval access, enable **Clerk Dashboard > Waitlist** and
invite or deny requests there. Approved signed-in users manage Connect in the
Connections settings; signed-out users reach Clerk sign-in or waitlist UI from
those controls.

For a closed known-user beta, use **Restrictions > Allowlist** or Clerk's
restricted mode. An empty allowlist blocks all new sign-ups. Allowlisting
controls account creation; ban an existing user to terminate their Clerk
sessions and prevent future cloud sign-in.
