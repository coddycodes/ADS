# Coddy

Coddy Studio drives the Coddy agent CLI. The CLI runs the agent loop, tools,
permissions, questions, MCP servers and skills; Coddy Studio starts its HTTP + SSE
server on loopback and translates that stream into the chat.

## Prerequisites

Coddy Studio refuses Coddy turns until the **Coddy Device Agent** is installed
and running on the host. Its health endpoint is
`http://127.0.0.1:9098/api/v1/agent/health`; see
[Studio config](../reference/studio-config.md) for the URL and the development
override.

Install the Coddy CLI on the machine or remote environment running the Coddy
Studio server, then sign in once:

```bash
coddy login
```

`CODDY_API_KEY` in the instance environment works too. The CLI stores its
credentials and sessions under `~/.coddy/cli` unless the instance sets a
**Data directory**.

## Models

The catalog comes from the Coddy CLI's resolved providers. By default Studio
offers only the `coddy` provider, i.e. the Coddy API at
`https://api.coddy.id/v1` discovered from `/v1/models`. Other model providers
stay hidden until they are listed under `models.providers` in
[`~/.coddy/studio/config.yaml`](../reference/studio-config.md).

## Instance settings

| Field | Meaning |
| --- | --- |
| Binary path | The `coddy` executable. Default `coddy`. |
| Data directory | Optional `-D` root for the CLI's config and sessions. Blank uses the CLI default. |
| Server URL | Optional already-running `coddy cli server` endpoint. Blank lets Coddy Studio spawn one. |

Coddy Studio resolves the binary with the instance environment, starts
`coddy cli server --host tcp://127.0.0.1:0 --port-file …`, waits for the health
and version handshake, creates a workspace for the thread's directory and
creates or resumes the CLI session.

## Capabilities

- Streamed assistant text, reasoning, tool calls and tool results.
- Permission requests (allow once, allow for the session, deny) and question
  batches, including a resumed session's pending asks.
- Token usage from the CLI's session counters, with the model's context window
  when the catalog reports one.
- MCP server status from the CLI's MCP events.
- Skills from the CLI's Coddyworks discovery.
- Context handoff when a resume cursor points at a conversation the CLI no
  longer has, or when a legacy thread is continued with Coddy.

Not supported in this release:

- **Steering** a running turn. A second prompt is queued as its own turn, so
  Coddy Studio keeps its durable queue and reports `supportsTurnSteer = false`.
- **Conversation rewind** at a checkpoint. Revert restores the files and says
  so; `supportsConversationRewind = false`.
- **Observed provider terminal.** The `coddy` TUI opens in the provider
  terminal but its activity is not mirrored into the chat. Use the chat for
  activity; the terminal is a plain TUI.
- **Attachments.** A turn's text is delivered; image and file attachments are
  not forwarded to the CLI yet.

## Security

The managed server binds `127.0.0.1` on an ephemeral port. Do not configure a
non-loopback **Server URL** for a shared or relayed environment: the CLI API has
no authentication, and the loopback bind is the boundary. Credentials stay in
the instance environment and are redacted from settings, logs and profiler
records like every other provider.

## Troubleshooting

| Symptom | Fix |
| --- | --- |
| `provider executable was not found` | Install `coddy` on the server host, or set **Binary path** to its absolute path. |
| `speaks a newer server API` | Install a 1.x Coddy CLI, or point **Binary path** at one. |
| `did not publish its bound address` | Run `coddy cli server --host tcp://127.0.0.1:0 --port-file …` by hand and read the error; usually a bad data directory or a locked SQLite store. |
| A turn stays uncertain after a restart | The server could not prove the prompt arrived. Retry sends it to the same CLI session; the CLI does not deduplicate by run id. |
