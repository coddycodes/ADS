# Exposing BiBCode diagnostics to AI coding harnesses: embedded MCP server (rmcp + Axum) and CLI alternatives

Research date: 2026-10-09. Local context: BiBCode workspace pins `axum = "0.8.9"` and `tokio = "1.53.1"` (`/work/workspaces/orca/BibCode/makara/Cargo.toml` lines 10, 75).

**Most important finding:** MCP changed a lot in the current spec revision, **2026-07-28**. It removed sessions, the `initialize` handshake, the HTTP GET stream, `resources/subscribe` and SSE resumability. Live push is now done with `subscriptions/listen`. Any design based on 2025-era guidance (`Mcp-Session-Id`, GET SSE stream, `Last-Event-ID`) is out of date. rmcp 3.x supports both eras.

## 1. rmcp (official Rust MCP SDK): version, Streamable HTTP in Axum, stdio, macros, resources, notifications, maturity

### Takeaway

`rmcp` **3.5.1** (released 2026-10-05) is the current official SDK. It implements MCP 2026-07-28 and stays compatible with 2025-11-25. Its `StreamableHttpService` is a Tower service that you mount with `Router::nest_service("/mcp", svc)`. You can put your own axum auth middleware in front of it with `.layer(...)`. The SDK has built-in Host allow-listing for DNS-rebinding protection (loopback only by default) and optional Origin allow-listing.

### Cited Findings

- **Version history:** latest stable is `3.5.1` (2026-10-05). Earlier 3.x releases were 3.5.0 (09-28), 3.4.x, 3.3.0 (09-10), 3.2.0, 3.1.x, and 3.0.1 (2026-07-29). The 3.x line started right after the 2026-07-28 spec — [crates.io API](https://crates.io/api/v1/crates/rmcp)
- The README says the SDK implements MCP spec `2026-07-28` while staying compatible with `2025-11-25`. It links a 3.x migration guide. Install with `cargo add rmcp --features server` — [rust-sdk README](https://github.com/modelcontextprotocol/rust-sdk)
- **Feature flags** (from `crates/rmcp/Cargo.toml`):

  | Flag                                       | What it does                                                                    |
  | ------------------------------------------ | ------------------------------------------------------------------------------- |
  | (default)                                  | `["base64", "macros", "server"]`                                                |
  | `transport-io`                             | stdio                                                                           |
  | `transport-streamable-http-server`         | pulls in `server-side-http` and `transport-worker`. Provides the Tower service. |
  | `transport-child-process`                  | client spawning a stdio server                                                  |
  | `transport-streamable-http-client-reqwest` | HTTP client                                                                     |
  | `local`                                    | a `!Send` variant. The streamable HTTP server is only available without it.     |

  Sources: [rmcp Cargo.toml](https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/Cargo.toml); [docs.rs streamable_http_server](https://docs.rs/rmcp/3.5.1/rmcp/transport/streamable_http_server/index.html)

- Codex itself depends on rmcp `=3.3.0` (a git rev, `default-features = false`) for its MCP client (`codex-rmcp-client`). This is a strong signal of maturity and interop with Codex — [openai/codex codex-rs/Cargo.toml](https://github.com/openai/codex/blob/main/codex-rs/Cargo.toml)
- **Axum mount (README):**
  ```rust
  let config = StreamableHttpServerConfig::default()
      .with_legacy_session_mode(false)
      .with_json_response(true);
  let service = StreamableHttpService::new(|| Ok(Counter::new()), LocalSessionManager::default().into(), config);
  let router = axum::Router::new().nest_service("/mcp", service);
  ```
  The service factory runs per request in stateless mode, so shared state must live in a `Clone` handle such as `Arc<...>` — [README](https://github.com/modelcontextprotocol/rust-sdk)
- **Auth middleware in front of the service:** the official example `examples/servers/src/simple_auth_streamhttp.rs` wraps the MCP service like this:
  ```rust
  Router::new().nest_service("/mcp", mcp_service)
      .layer(middleware::from_fn_with_state(token_store.clone(), auth_middleware))
  ```
  The `auth_middleware` checks `Authorization: Bearer <token>` and returns `StatusCode::UNAUTHORIZED` otherwise. The result is then merged into the main router, and public routes stay unauthenticated — [simple_auth_streamhttp.rs](https://github.com/modelcontextprotocol/rust-sdk/blob/main/examples/servers/src/simple_auth_streamhttp.rs)
- **Graceful shutdown:** use `StreamableHttpServerConfig::default().with_cancellation_token(ct.child_token())` and call `ct.cancel()` in `axum::serve(...).with_graceful_shutdown(...)`. Cancelling "terminates all active sessions and the server stops accepting new requests" — [counter_streamhttp.rs](https://github.com/modelcontextprotocol/rust-sdk/blob/main/examples/servers/src/counter_streamhttp.rs); [tower.rs docs](https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/src/transport/streamable_http_server/tower.rs)
- **`StreamableHttpServerConfig` fields** (non_exhaustive), from [tower.rs](https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/src/transport/streamable_http_server/tower.rs):

  | Field                                  | Default and behavior                                                                                                                                                                                                                                                                                                             |
  | -------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
  | `sse_keep_alive`                       | 15s                                                                                                                                                                                                                                                                                                                              |
  | `sse_retry`                            | 3s                                                                                                                                                                                                                                                                                                                               |
  | `legacy_session_mode`                  | **true**. Applies only to protocol versions before 2026-07-28. Requests that negotiate 2026-07-28 "are always served statelessly regardless of this setting."                                                                                                                                                                    |
  | `json_response`                        | Prefers `application/json` for simple tools. Falls back to SSE if the handler emits a notification first.                                                                                                                                                                                                                        |
  | `cancellation_token`                   | Shutdown signal (see above).                                                                                                                                                                                                                                                                                                     |
  | `allowed_hosts`                        | **Default `["localhost", "127.0.0.1", "::1"]`**. Host-header validation "to prevent DNS rebinding attacks against locally running servers". Public deployments override it. `disable_allowed_hosts()` allows any host and is "NOT recommended".                                                                                  |
  | `allowed_origins`                      | Default empty, which **disables Origin validation** for backward compatibility. A non-empty list enables RFC 6454 tuple matching. Requests with no `Origin` header still pass. `enforce_origin_validation()` with an empty list rejects every present Origin. Ports: use `:*` or an explicit port. A missing port is deprecated. |
  | `session_store`                        | Only relevant to legacy sessions.                                                                                                                                                                                                                                                                                                |
  | `max_request_body_bytes`               | Oversized bodies get 413.                                                                                                                                                                                                                                                                                                        |
  | `stateless_protocol_metadata_required` | Default false. Requires the 2026-07-28 per-request `MCP-Protocol-Version` header and `_meta`.                                                                                                                                                                                                                                    |

  Builders include `with_allowed_hosts`, `with_allowed_origins`, `enforce_origin_validation`, `with_sse_keep_alive`, `with_legacy_session_mode`, `with_json_response`, `with_cancellation_token` and `with_max_request_body_bytes`.

- **Checked against the 3.5.1 release, not only `main`:** docs.rs for 3.5.1 lists all 10 fields above and these 12 methods: `disable_allowed_hosts`, `disable_allowed_origins`, `enforce_origin_validation`, `with_allowed_hosts`, `with_allowed_origins`, `with_cancellation_token`, `with_json_response`, `with_legacy_session_mode`, `with_max_request_body_bytes`, `with_sse_keep_alive`, `with_sse_retry`, `with_stateless_protocol_metadata_required` — [docs.rs rmcp 3.5.1 StreamableHttpServerConfig](https://docs.rs/rmcp/3.5.1/rmcp/transport/streamable_http_server/tower/struct.StreamableHttpServerConfig.html). At tag `rmcp-v3.5.1`, `ServerHandler` (`crates/rmcp/src/handler/server.rs`) defines `supported_protocol_versions`, `accepted_subscription_filter` and `listen` — [rust-sdk at rmcp-v3.5.1](https://github.com/modelcontextprotocol/rust-sdk/blob/rmcp-v3.5.1/crates/rmcp/src/handler/server.rs)
- **Tool macros:**
  - `#[tool(description = "...")]` marks a method as a tool.
  - `#[tool_router]` generates a router for the impl block. `#[tool_router(server_handler)]` also implements `ServerHandler`, which suits tools-only servers.
  - `#[tool_handler]` wires the router into a hand-written `ServerHandler` impl, where you can set the name, version and instructions.
  - Input and output schemas come from the parameter type's fields and doc comments, via `schemars`. Arguments are wrapped in `Parameters<T>`.
  - A tool-level error is `Ok(CallToolResult::error(...))`, which the model sees. A protocol error is `Err(McpError)`.

  Sources: [README](https://github.com/modelcontextprotocol/rust-sdk); [examples/servers/src/common/counter.rs](https://github.com/modelcontextprotocol/rust-sdk/blob/main/examples/servers/src/common/counter.rs)

- **Resources:** implement `list_resources()`, `read_resource()` and optionally `list_resource_templates()` (for example `users://{user_id}/profile`), and enable them with `.enable_resources()`. Unknown URIs return `McpError::resource_not_found` — [README](https://github.com/modelcontextprotocol/rust-sdk)
- **Notifications:** call `notify_resource_list_changed()` and `notify_resource_updated(...)` through `context.peer`. Progress, cancellation and list-changed notifications are supported — [README](https://github.com/modelcontextprotocol/rust-sdk)
- **2026-07-28 live push in rmcp:** the `subscriptions_streamhttp.rs` example does the following:
  - Overrides `ServerHandler::supported_protocol_versions` to return `[ProtocolVersion::V_2026_07_28]`.
  - Implements `accepted_subscription_filter(&self, requested: &SubscriptionFilter) -> Option<SubscriptionFilter>`.
  - Implements `async fn listen(&self, context: SubscriptionContext)`, which loops on `tokio::select!` over `context.cancelled()` and pushes through `context.sink().notify_tool_list_changed()`.
  - Mounts with `.with_legacy_session_mode(false).with_sse_keep_alive(Some(Duration::from_secs(10)))`.

  Source: [subscriptions_streamhttp.rs](https://github.com/modelcontextprotocol/rust-sdk/blob/main/examples/servers/src/subscriptions_streamhttp.rs)

- **Other official server examples** in the repo: `counter_stdio.rs`, `counter_streamhttp.rs`, `counter_hyper_streamable_http.rs`, `simple_auth_streamhttp.rs`, `complex_auth_streamhttp.rs`, `cimd_auth_streamhttp.rs`, `subscriptions_streamhttp.rs`, `progress_demo.rs`, `structured_output.rs`, `mrtr.rs` — [examples/servers/src](https://github.com/modelcontextprotocol/rust-sdk/tree/main/examples/servers/src)

### Inferences

- **For BiBCode:**
  - Add `rmcp = { version = "3.5", default-features = false, features = ["server", "macros", "transport-streamable-http-server"] }`, plus `transport-io` if you also want a stdio subcommand.
  - Nest it under the existing Axum router, for example `/api/diagnostics/mcp`, behind the existing token middleware.
  - The service is a plain Tower service, so the existing auth layer needs no custom MCP auth code.
- Set these explicitly rather than relying on defaults:
  - `.with_legacy_session_mode(false)`
  - `.enforce_origin_validation()`, or an exact allow-list. Harness clients (Claude Code, Codex) are not browsers and send no `Origin`, so this only blocks browser-originated requests.
  - Keep the loopback `allowed_hosts` default for the local bind. Override it only for an explicit remote hostname.
- **Rough edge:** `legacy_session_mode` defaults to `true` and Origin validation defaults to off. These defaults favor backward compatibility, not secure-by-default.
- **Keep the default protocol versions.** Do not copy the subscriptions example's `supported_protocol_versions` override, which accepts only 2026-07-28. rmcp's default serves both the session-era and the stateless protocol. Restricting it would break any client that still negotiates 2025-11-25, such as a Claude Code install that is not on the v2 runtime.
- **"Live event streaming"** fits MCP 2026-07-28 in two ways:
  - A `subscriptions/listen` stream that sends `notifications/resources/updated` for a resource such as `diag://events/recent`. The client then re-reads the resource.
  - Request-scoped `notifications/progress` while a bounded "watch for N seconds" tool runs.

  Neither one injects the events themselves into the model's context. The agent still has to call `read`.

### Gaps

- I did not verify a published `rmcp` 3.x migration guide in detail, or the exact `ServerHandler` method set for resources under 3.5.1. Confirm both against docs.rs for the pinned version.
- I found no third-party production Rust app (outside the SDK examples and Codex's client use) that embeds the rmcp server inside an existing Axum app with its own auth. The SDK's `simple_auth_streamhttp.rs` is the canonical pattern.

## 2. Authorization: OAuth 2.1 vs static bearer; what Claude Code and Codex support (static headers, env tokens, stdio over ssh)

### Takeaway

MCP authorization is **OPTIONAL**. The OAuth 2.1 profile is a SHOULD for HTTP transports that choose to support authorization. A pre-shared bearer token checked by your own middleware is therefore spec-permissible for a private, single-user diagnostics endpoint. Both Claude Code and Codex support static bearer tokens taken from environment variables.

### Cited Findings

- "Authorization is **OPTIONAL** for MCP implementations." When it is supported, HTTP transports "**SHOULD** conform to this specification", and STDIO "**SHOULD NOT** follow this specification, and instead retrieve credentials from the environment." — [MCP 2025-11-25 Authorization](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization). This quote comes from the 2025-11-25 page. It should still hold for 2026-07-28, because that revision's changelog lists every authorization change (`iss` validation, DCR deprecation, credentials keyed by issuer, `application_type`) and none of them makes authorization mandatory — [2026-07-28 changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog)
- **The full OAuth profile, if adopted, requires all of the following:**
  - The server MUST implement RFC 9728 Protected Resource Metadata.
  - It MUST return 401 with `WWW-Authenticate: Bearer resource_metadata=...` or serve the well-known URI.
  - It MUST validate the token audience (RFC 8707).
  - "MCP servers **MUST NOT** accept or transit any other tokens." Token passthrough is forbidden.
  - Tokens MUST NOT be sent in the query string.

  Source: [MCP Authorization](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization)

- **2026-07-28 changes to authorization:**
  - Authorization servers SHOULD send `iss` (RFC 9207), and clients MUST validate it.
  - Dynamic Client Registration is **deprecated** in favor of Client ID Metadata Documents.
  - Clients MUST key credentials by issuer.

  Source: [2026-07-28 changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog)

- **Current protocol revision:** "The **current** protocol version is **2026-07-28**." Each request declares its version in `_meta` `io.modelcontextprotocol/protocolVersion`. Servers MUST implement `server/discover` — [Versioning](https://modelcontextprotocol.io/specification/versioning)
- **2026-07-28 major changes:**
  - Removed `Mcp-Session-Id` and protocol-level sessions.
  - Removed the `initialize` handshake.
  - Replaced the HTTP GET endpoint and `resources/subscribe` with `subscriptions/listen`.
  - Removed `ping` and `logging/setLevel`.
  - Removed `Last-Event-ID` resumability.
  - New required headers `Mcp-Method` and `Mcp-Name`.
  - `tools/list` results carry `ttlMs` and `cacheScope`.
  - The Logging feature is deprecated: "log to `stderr` (stdio) or use OpenTelemetry".

  Source: [2026-07-28 changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog)

- **Claude Code, HTTP with a static header:**
  ```bash
  claude mcp add --transport http secure-api https://api.example.com/mcp \
    --header "Authorization: Bearer your-token"
  ```
  Scopes: `-s/--scope local|project|user` (`local` is the default) — [Claude Code MCP docs](https://code.claude.com/docs/en/mcp)
- **Claude Code `.mcp.json`** supports env expansion `${VAR}` and `${VAR:-default}` in `command`, `args`, `env`, `url` and `headers`:
  ```json
  {
    "mcpServers": {
      "api-server": {
        "type": "http",
        "url": "${API_BASE_URL:-https://api.example.com}/mcp",
        "headers": { "Authorization": "Bearer ${API_KEY}" }
      }
    }
  }
  ```
  Credential variables such as `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN` expand to empty in a remote server's url and headers, so use your own variable name — [Claude Code MCP docs](https://code.claude.com/docs/en/mcp)
- **Claude Code `headersHelper`:**
  - A command that prints a JSON object of headers.
  - It has a 10s timeout, runs on each connection without caching, and dynamic headers override static ones.
  - On a 401 or 403 from a tool call, Claude Code re-runs the helper, reconnects and retries once.
  - The environment includes `CLAUDE_CODE_MCP_SERVER_NAME` and `CLAUDE_CODE_MCP_SERVER_URL`.

  Source: [Claude Code MCP docs](https://code.claude.com/docs/en/mcp)

- **Claude Code stdio:** `claude mcp add [options] <name> -- <command> [args...]`. Everything after `--` is passed to the server untouched — [Claude Code MCP docs](https://code.claude.com/docs/en/mcp)
- **Claude Code and protocol 2026-07-28:** the "v2 runtime" is built on MCP TypeScript SDK 2.0, which adds revision 2026-07-28. It "Receives `list_changed` notifications from servers on the newer revision over a stream it holds open" — [Claude Code MCP docs](https://code.claude.com/docs/en/mcp)
- **Claude Code `list_changed`:** in an interactive session it refetches tools, prompts and resources. In `-p` mode and the Agent SDK it refreshes tools only — [Claude Code MCP docs](https://code.claude.com/docs/en/mcp)
- **Claude Code `claude/channel` capability:** a server can push messages directly into the session (opt in with `--channels`). However, "a channel server that negotiates 2026-07-28 can't deliver channel messages" — [Claude Code MCP docs](https://code.claude.com/docs/en/mcp)
- **Claude Code limits and timeouts:**
  - Output warning at 10,000 tokens, with a default cap of 25,000 tokens (`MAX_MCP_OUTPUT_TOKENS`).
  - Text over 50,000 chars is saved to a file. A tool can raise its own limit via `_meta["anthropic/maxResultSizeChars"]` (max 500,000).
  - `MCP_TIMEOUT` sets the startup timeout. A per-server `"timeout"` in ms overrides `MCP_TOOL_TIMEOUT`.
  - `CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT` defaults to 5 min for HTTP.

  Source: [Claude Code MCP docs](https://code.claude.com/docs/en/mcp)

- **Claude Code resources** are referenced as `@server:protocol://resource/path`. Claude Code also exposes list and read resource tools automatically — [Claude Code MCP docs](https://code.claude.com/docs/en/mcp)
- **Codex `~/.codex/config.toml`** (or `.codex/config.toml` for trusted projects).

  Streamable HTTP server:

  ```toml
  [mcp_servers.figma]
  url = "https://mcp.figma.com/mcp"
  bearer_token_env_var = "FIGMA_OAUTH_TOKEN"
  http_headers = { "X-Figma-Region" = "us-east-1" }
  ```

  `env_http_headers` maps a header name to an env var name. `auth` defaults to `oauth`.

  Stdio server:

  ```toml
  [mcp_servers.x]
  command = "..."
  args = [...]
  env_vars = ["LOCAL_TOKEN"]

  [mcp_servers.x.env]
  K = "V"
  ```

  Shared options:

  | Option                | Notes                                           |
  | --------------------- | ----------------------------------------------- |
  | `startup_timeout_sec` | default 10                                      |
  | `tool_timeout_sec`    | default 60                                      |
  | `enabled_tools`       | allow list                                      |
  | `disabled_tools`      | deny list, applied after `enabled_tools`        |
  | `enabled`             | `false` disables the server without deleting it |

  CLI: `codex mcp add <name> --env K=V -- <cmd>`, `--url` for HTTP, `codex mcp list`, and `codex mcp login <name>` for OAuth.

  Source: [Codex MCP docs (developers.openai.com/codex/mcp, which redirects to learn.chatgpt.com)](https://learn.chatgpt.com/docs/extend/mcp?surface=cli)

### Inferences

- **Recommended auth for BiBCode:**
  - Reuse BiBCode's existing server tokens. Ideally mint a separate diagnostics-scoped, read-only token so a leaked token can't drive sessions.
  - Check it in the existing axum middleware.
  - Clients send it with `Authorization: Bearer` (Claude: `--header` or `${VAR}` in `.mcp.json`; Codex: `bearer_token_env_var`).
  - Skip OAuth/RFC 9728 for v1. Per the spec, a spec-strict client may probe for `WWW-Authenticate resource_metadata` on a 401. Static-header configs don't need it.
- **Remote host, option A (HTTP over a tunnel):**
  - `ssh -L 7777:127.0.0.1:<port> host`, then `url = "http://127.0.0.1:7777/api/diagnostics/mcp"`.
  - The remote Host header stays loopback, so rmcp's default `allowed_hosts` keeps working.
- **Remote host, option B (stdio over ssh):**
  - Claude: `claude mcp add bibcode-diag -- ssh host bibcode diag mcp --stdio`
  - Codex: `command = "ssh"`, `args = ["host", "bibcode", "diag", "mcp", "--stdio"]`
  - The stdio subcommand would proxy to the running server's API using a local credential. Per the spec, stdio gets credentials from the environment.
  - These command lines are inferred from the documented `-- <command> [args...]` and `command`/`args` syntax. Neither doc shows an ssh example explicitly.
- **Claude Code channels don't fit a 2026-07-28 server.** Live push into the conversation via `claude/channel` would require negotiating an older revision. Treat "streaming into the agent" as poll/read-based for now.
- **Keep tool output small:** paginate or cap entries and return summaries. Claude Code caps output at 25k tokens by default.

### Gaps

- I did not confirm which MCP protocol revisions the current Codex CLI client negotiates. Its rmcp 3.3.0 dependency suggests it supports 2026-07-28.
- I did not confirm whether Codex supports `subscriptions/listen`, `list_changed` or resources at all. The Codex MCP page I fetched documents only tool configuration.
- I found no official ssh-stdio example in either harness's docs.
- I did not read the Claude Code `#mcp-client-runtimes` section, which says when Claude Code uses the v2 runtime and so negotiates 2026-07-28. Read it before restricting the server to one protocol revision.
- Codex documents `auth` as defaulting to `oauth`. I did not confirm whether `bearer_token_env_var` alone is enough for a server that accepts only a bearer token, or whether `auth` must also be set. Test this, or check `codex mcp --help` and the config schema.

## 3. Security: DNS rebinding, localhost binding, read-only tools, prompt injection from log content, redaction

### Takeaway

The spec has three requirements for Streamable HTTP:

- Validate `Origin`, returning 403 if it is present and invalid.
- Bind to localhost when local.
- Authenticate every connection.

Tool outputs MUST be sanitized, and clients are told to treat annotations as untrusted. Log content returned to an agent is attacker-influenced data, so it is a prompt-injection vector. Redaction and framing are the server's job.

### Cited Findings

- **2026-07-28 Streamable HTTP security requirements:**
  - "Servers **MUST** validate the `Origin` header on all incoming connections to prevent DNS rebinding attacks. If the `Origin` header is present and invalid, servers **MUST** respond with HTTP 403 Forbidden."
  - "When running locally, servers **SHOULD** bind only to localhost (127.0.0.1)…"
  - "Servers **SHOULD** implement proper authentication for all connections."

  Source: [Streamable HTTP 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http)

- **Same page, other server obligations:**
  - Servers SHOULD send `X-Accel-Buffering: no` on SSE.
  - Servers should emit periodic `:` comment keep-alives on the `subscriptions/listen` stream.
  - Closing the SSE response stream MUST be treated as cancellation.
  - Servers MUST validate that the `Mcp-Method` and `Mcp-Name` headers match the body (error `-32020` HeaderMismatch).

  Source: [Streamable HTTP 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http)

- **Local MCP server compromise**, listed attack: "An attacker accesses an insecure local server that's left running on localhost via DNS rebinding". Local servers SHOULD "Use the `stdio` transport to limit access to just the MCP client" or, over HTTP, "Require an authorization token" or "Use unix domain sockets or other Interprocess Communication (IPC) mechanisms with restricted access" — [Security Best Practices 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28/basic/security_best_practices)
- **State handles:** "MCP servers **MUST NOT** treat possession of a state handle as authentication". Bind handles to `<user_id>:<handle>` — [Security Best Practices](https://modelcontextprotocol.io/specification/2026-07-28/basic/security_best_practices)
- **Tools security:** "Servers **MUST**: Validate all tool inputs; Implement proper access controls; Rate limit tool invocations; Sanitize tool outputs." Clients SHOULD "Validate tool results before passing to LLM". "Clients **MUST** consider tool annotations to be untrusted unless they come from trusted servers." — [Tools 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28/server/tools)
- **Tool definitions:** for no-arg tools use `{"type":"object","additionalProperties":false}`. Tools SHOULD be returned in deterministic order. If a tool returns `structuredContent`, it SHOULD also return the serialized JSON as text — [Tools 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28/server/tools)
- **Claude Code trust warning:** "Verify you trust each server before connecting it. Servers that fetch external content can expose you to prompt injection risk." — [Claude Code MCP docs](https://code.claude.com/docs/en/mcp)
- **Claude Code per-tool forced approval:** `_meta["anthropic/requiresUserInteraction"]: true` prompts on every call, even in bypass modes. This doesn't fit read-only diagnostics, but it is useful if any mutating tool (for example "start profiler") is ever added — [Claude Code MCP docs](https://code.claude.com/docs/en/mcp)
- **rmcp support:** loopback-only `allowed_hosts` by default (Host check against DNS rebinding). Origin validation is opt-in through `with_allowed_origins` or `enforce_origin_validation`. `max_request_body_bytes` returns 413 — [rmcp tower.rs](https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/src/transport/streamable_http_server/tower.rs)

### Inferences

- **Hardening checklist for BiBCode:**
  - Bind the diagnostics MCP route only on the loopback listener, or reuse the existing listener but reject non-loopback peers unless remote access is explicitly enabled.
  - Enable rmcp Origin enforcement, rejecting any browser `Origin`.
  - Keep the Host allow-list.
  - Require a bearer token on every request (2026-07-28 is stateless, so there is no session to trust).
  - Expose only read tools and no "run command" tools.
  - Cap result sizes and rate-limit.
- **Prompt-injection mitigations for log and event content:**
  - Return logs as structured JSON (`structuredContent` with fields such as level, target, message) rather than free prose.
  - Truncate each field.
  - Strip ANSI and control characters.
  - Redact secrets (tokens, Authorization headers, env values, home paths if desired).
  - State in the tool description and server instructions that the content is untrusted data from the application and not instructions.

  This reduces the risk but does not eliminate it. Provider output and file content that BiBCode logs can contain adversarial text.

- **Redaction placement:** redact at the source, in the diagnostics collector, so the MCP, CLI and UI paths share one policy. Don't redact separately in each transport.

### Gaps

- The 2026-07-28 tools page I fetched did not enumerate the annotation fields (`readOnlyHint`, `destructiveHint` and so on), and the Claude Code docs I read don't say whether `readOnlyHint` changes permission prompts. Verify against the schema reference before relying on it.
- I found no MCP-spec normative text specifically on prompt injection through tool _results_. Beyond "Sanitize tool outputs" and "Validate tool results before passing to LLM", the guidance above is inference.

## 4. Alternative: CLI subcommand with `--json` and `--follow`, used by agents through the shell; tradeoffs vs MCP

### Takeaway

A `bibcode diag ... --json [--follow]` CLI needs no harness configuration. It works identically in Claude Code, Codex and any shell-capable agent, locally and over `ssh host bibcode diag ...`. It can stream through stdout as NDJSON. MCP adds discoverable typed tools and schemas, harness-native permissions, and resources and `@`-mentions, at the cost of configuration and auth plumbing. The cheapest path to both is to build one diagnostics query layer and expose it through the CLI first and an MCP wrapper second.

### Cited Findings

- **Harness facts that bear on the tradeoff:**
  - Claude Code's tool search defers MCP tool definitions until needed, and it truncates tool descriptions and server instructions at 2,048 chars by default.
  - It caps MCP output at 25k tokens (10k warning).
  - Codex defaults to `tool_timeout_sec = 60`, so a long-running `--follow`-style MCP tool call would hit it unless the timeout is raised.

  Sources: [Claude Code MCP docs](https://code.claude.com/docs/en/mcp); [Codex MCP docs](https://learn.chatgpt.com/docs/extend/mcp?surface=cli)

- **MCP has no unbounded "stream data into the model" primitive.** Request-scoped `notifications/progress` flow only until the response. `subscriptions/listen` carries change notifications such as `resources/updated`, not payloads — [Streamable HTTP 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http)
- The spec recommends stdio for local servers to limit access to the client — [Security Best Practices](https://modelcontextprotocol.io/specification/2026-07-28/basic/security_best_practices)

### Inferences

- **CLI pros:**
  - Zero configuration in any harness.
  - Auth reuses whatever BiBCode's CLI already does.
  - `--follow` with NDJSON works with agent background-shell tools and can be bounded with `--duration` or `--max-events`.
  - Easy to test, and usable by humans too.
  - Remote use is just `ssh host bibcode diag ...`.
- **CLI cons:**
  - The agent must learn flags from `--help`, with no typed schema.
  - Shell permission prompts are coarser than per-MCP-tool rules.
  - Unbounded `--follow` can hang an agent's foreground shell or flood its context. Always provide `--duration` and `--limit`.
- **MCP pros:**
  - Typed, schema-described tools discovered automatically.
  - Per-tool allow and deny lists (Claude permission rules `mcp__<server>__<tool>`; Codex `enabled_tools`).
  - Resources addressable with `@`.
  - `structuredContent`.
- **MCP cons:**
  - Per-harness setup and token handling.
  - Spec churn: 2026-07-28 broke the transport shape.
  - "Live" data still has to be polled or re-read.
  - Claude Code channel push doesn't work on 2026-07-28 servers.
- **Suggested BiBCode shape:**
  - Diagnostics query functions live in `apps/server`.
  - Existing typed HTTP/WS RPC serves the UI.
  - `bibcode diag snapshot|events|profile --json [--follow --duration 30s]` is a thin client of the running server.
  - An optional `/…/mcp` route uses rmcp `#[tool]` wrappers over the same functions.
  - An optional `bibcode diag mcp --stdio` uses rmcp `transport-io` for ssh and stdio use.
  - Start with the CLI, which is the smaller diff, and add MCP when harness-native discovery or permissions are needed.

### Gaps

- I found no primary-source benchmark or vendor guidance comparing CLI and MCP effectiveness for agents. The tradeoffs above are reasoned, not measured.
