# Pull request panel: paint from stored copies within 200 ms

Status: **Approved** on 2026-10-13. Tracking issue:
[#90](https://github.com/mubeda/BibCode/issues/90).

Builds on the approved
[GitLab merge request background sync](./2026-10-08-gitlab-merge-request-background-sync-design.md)
design, which added `pullRequests.readSnapshot`, the `pull_request_snapshots`
table and the GitLab poller.

## Problem and evidence

The user's requirement: the pull request list and an open pull or merge request
show within 200 ms, from a remote client as well as locally.

Measured on 2026-10-13 against v0.9.1 (Linux desktop server, macOS client on the
same LAN, self-hosted GitLab behind a VPN, github.com). Client timings are the
Profiler's `client.request` split; host timings are direct runs on the server.

| Read | Server time | Network | Note |
| --- | ---: | ---: | --- |
| `gitManager.getRefs`, `getCommits`, `getDiff` | 1–74 ms | ~5 ms | Already within target. |
| `pullRequests.readSnapshot` | 10–23 ms | — | SQLite only. |
| `pullRequests.getContext` | 650–3,310 ms | 4 ms | Blocks the whole panel. |
| `pullRequests.list` (GitLab) | 647–3,319 ms | 4–15 ms | |
| `pullRequests.get` / `getTimeline` (GitLab) | 3,170–3,330 ms | 5–23 ms | |
| `pullRequests.getChecks` / `getCommits` (GitLab) | 2,325–2,330 ms | 4–5 ms | |
| `pullRequests.getFiles` (GitLab) | 5,440 ms | 28 ms | |
| GitHub list | ~2 s (user report) | — | No stored copy for GitHub. |

### Every GitLab read is a 2.3 s `glab` process

| Measurement on the server | Time |
| --- | ---: |
| `glab api version`, `user`, MR, approvals, reviewers (each) | 2,311–2,528 ms |
| The same with `GLAB_SEND_TELEMETRY=0` | 1,172–1,207 ms |
| `glab version` (no API call) | 1,185 ms |
| Seven `glab api` reads in parallel (the shape of `pullRequests.get`) | 2,839 ms |
| `curl` to the same host, new connection (TCP 295 ms, TLS 270 ms) | 875 ms |
| `curl`, second request on the same connection | 312 ms |
| DNS lookup of the GitLab host (not cached on this machine) | 270 ms |
| `gh api user`, `gh api …/pulls` | 456–555 ms |

A `strace` of `glab version` shows a DNS lookup and an HTTPS connection to the
GitLab host before the command finishes: glab 1.122.0 sends a telemetry event
synchronously on every invocation unless `GLAB_SEND_TELEMETRY=0`.
`HostCommandRunner` sets `GLAB_CHECK_UPDATE=false` and `NO_COLOR=1` but not this
variable (`apps/server/src/pull_requests/host.rs:317-329`), and the
source-control service runs `glab` with an empty environment
(`apps/server/src/source_control/pull_request.rs:883`, `:1379`). Each call
therefore pays two new HTTPS connections: telemetry, then the request.

The 2026-10-08 harness injected 200 ms per response and measured about 25 ms of
`glab` overhead, so it concluded that a pooled HTTP client would not help much.
The harness has no DNS, TCP or TLS cost and no telemetry endpoint. On a real
VPN link the per-process cost is about 1.2 s with telemetry off and 2.3 s with
it on.

### What the user waits for

- **The panel waits for `getContext` before rendering anything.**
  `PullRequestsPanel` renders the list or detail only once the context query has
  data, and shows a skeleton until then. `getContext` runs the CLI's
  authentication probe alongside three host reads (`user`, project,
  `version`). The server caches the answer for 30 s
  (`apps/server/src/pull_requests/cache.rs:12`), and the client keeps the query
  for 5 minutes after the panel closes. After either expires, or after a server
  restart, every open waits 1.5–3.3 s before the stored copy can paint.
- **GitLab paints from stored copies once the context is known.** On a repeat
  open within the cache window the user saw the list and the merge request
  instantly. On the first open no stored copy existed: every
  `pull_request_snapshots` row for the project was written during that test.
- **GitHub has no stored copy.** `store_read` and `readSnapshot` are gated to
  GitLab (`gitlab_snapshot_database`, `apps/server/src/pull_requests/mod.rs:340`),
  and the list and detail views request `readSnapshot` only when the provider is
  GitLab. Every GitHub open waits on `gh`.

### Approve

The GitLab approve path runs, in order: the scope probe, `host.context`,
`action_detail` (five `glab` reads one after another:
`apps/server/src/pull_requests/gitlab/mod.rs:563-596`), then
`glab mr approve <n> --sha <head>`. At 2.3 s per call that is about 14 s before
the approval is sent. The only recorded attempt (2026-10-12, v0.9.0) ended in
`failure` after about 30 s. The server log for that day was replaced at restart,
and `rpc.timing` records only the outcome, so the error code is unknown. The
Approve control stays "Loading…" until the live `pullRequests.get` succeeds, by
design.

## Goals

1. Opening the panel, the list, or a previously viewed pull or merge request
   paints within 200 ms on a LAN client, for GitLab and GitHub, whenever a
   stored copy exists. This includes after a server restart and after the
   client's cache has expired.
2. Reads that must reach the host take one `glab` start-up instead of two.
3. Approve's pre-check is one parallel wave, and a failed action leaves its
   error code in the server log.

Not a goal: a cold first view of a never-opened request within 200 ms. A single
host request on this network costs at least 312 ms, so that is out of reach for
any design that reads the host on open.

## Alternatives

1. **Keep `glab` and `gh`; serve the last context and stored copies first
   (recommended).** Persist the last available context with the other stored
   copies and return it immediately when the in-memory answer has expired,
   refreshing it in the background. Extend stored copies to GitHub. Turn off
   `glab` telemetry. Gets the user to the stored copy in one SQLite read and
   halves every remaining host read. No new credential handling, no new
   dependency, and only two optional contract fields.
2. **Replace `glab` and `gh` with an in-process HTTP client.** A pooled
   connection would cut a warm read to about 312 ms and remove DNS and TLS from
   each call. BiBCode would then have to read the CLIs' tokens (glab's keyring
   or config file, gh's keyring), a new credential trust boundary, and every
   adapter, including the mutation commands such as `glab mr approve`, would be
   rewritten. Deferred. Revisit if cold reads remain a complaint after
   option 1.
3. **Client-side persistence of context and stored copies.** Works per client
   only: a second window, the macOS client, and a restarted browser would each
   start cold. Rejected in favour of the server's existing table.

## Design (option 1)

### 1. Context served stale, refreshed in the background

- After a successful `getContext` that returns `status: "available"`, the server
  writes the context to `pull_request_snapshots` with `kind = "context"`,
  `key = ""`, under the same host and project as the other rows. Unavailable
  answers are never stored.
- `getContext` without `rescan`:
  - in-memory answer under 30 s old: return it (unchanged);
  - otherwise, an in-memory answer of any age, or the stored row, exists:
    return it immediately and start one background refresh for that scope. The
    refresh runs the existing authenticated read and replaces both copies on
    success. Concurrent callers share the one refresh;
  - nothing stored: the existing live read.
- Resolving the scope for a stored answer must not run a CLI. Today
  `resolve_pending_scope` is CLI-free only for a named host (github.com,
  gitlab.com) or a host recorded in `ProviderHosts`. That record lives in
  memory, so after a restart a self-hosted origin is `Unidentified` and goes
  through `discover_hosts`, which runs both CLIs' authentication probes
  (`apps/server/src/pull_requests/context.rs:395`). The same applies to
  `readSnapshot` today. New rule: before discovery, an `Unidentified` origin
  whose host and repository have a stored context row is admitted with that
  row's provider, as a recorded host is (`ScopeAuthentication::Recorded`, so
  the background refresh still probes authentication). With no row, discovery
  runs as today. A Rescan ignores the row.
- A background refresh that returns `not_authenticated`,
  `repository_unreachable`, `cli_missing` or `cli_too_old` takes the existing
  invalidation path (clear the cached contexts; on `not_authenticated` also
  forget the host and delete its stored rows). In all four cases it also
  deletes the stored context row, so the next open reads live and shows the
  unavailable state.
- `rescan` and authentication recovery bypass both copies (unchanged).
- Action safety is unchanged. `runAction` and `checkout` re-read context and
  permissions from the host, and action controls bind only to a live detail
  read on this mount. A stale context can affect what the header shows, for
  example the "New merge request" control or capability-dependent tabs, for at
  most one open.

The open panel must learn the refreshed answer, because `getContext` is a
single request and response. Two optional, additive contract fields:

- output: an available context answered from a stored or expired copy carries
  `stale: true`. Older servers never send it.
- input: `fresh: true` asks for a current answer without clearing caches
  (unlike `rescan`). The server joins the background refresh already running
  for that scope, or reads live, and never answers from a stale copy. Older
  servers ignore the field, and the client sends it only after receiving
  `stale: true`.

When the panel paints from a context with `stale: true`, it issues one
`getContext` with `fresh: true` and replaces the shown context with its
answer, including an unavailable one, which then shows the unavailable state
as today. A failure of the fresh read keeps the stale context and shows the
existing failure banner.

### 2. Stored copies for GitHub

- `store_read` and `readSnapshot` use the database for both providers. The
  GitLab-only gate stays on `subscribe` and the poller: GitHub routes still do
  not subscribe or poll, as the living document requires.
- The list and detail views request `readSnapshot` for GitHub too and use the
  existing display rule: show whichever copy has the newer `observedAt`, and
  bind action controls only to the live detail.
- Deletion rules extend to GitHub unchanged: logout or an auth or permission
  failure deletes the host's rows; a successful `runAction` deletes that
  number's rows. Size limits are unchanged (a `files` payload over 1 MiB is not
  stored; the table is capped at 32 MiB).
- `host` already distinguishes github.com from GitLab hosts, so rows from the
  two providers cannot collide.

### 3. `glab` telemetry off for BiBCode's own calls

Every `glab` invocation BiBCode starts sets `GLAB_SEND_TELEMETRY=0`: the
pull request runner (`host.rs`) and the source-control service (status
enrichment, request creation, discovery probes). One shared helper supplies the
provider CLI environment, so a new call site cannot miss it. The user's own
`glab` configuration and their interactive use are unchanged. This also removes
a request per call to the user's GitLab instance, which is
privacy-positive.

### 4. Approve pre-check in one wave

`GitLabHost::action_detail` runs its five reads (request, approvals, reviewers,
GraphQL metadata, project) with `tokio::join!`, as `detail` already does. Order
does not matter: the permission computation consumes all five results.
`run_action` logs failures with `tracing::warn!` carrying the operation and the
error code only: no hostname, project path, payload or message text.

## Failure and concurrency

- A stale context and a live read that fails afterwards: the existing failure
  banner shows over the stored copies; the auth-recovery path runs as today.
- Two windows opening the same scope while the context is stale share one
  background refresh.
- A background refresh started before a logout must not write its context back.
  Stamp the write with the generation taken before the read, as `store_read`
  already does, and drop the write when the host's rows were deleted since.
- The server shutdown token cancels background refreshes.

## Profiler, crash and trace coverage

- RPC methods: none added. `getContext` keeps its timing, and its answer from a
  stored copy is visible as a fast `rpc.timing`. The background refresh runs
  outside an RPC, so it is not timed as one. It logs its failures with
  `tracing::warn!`.
- Child processes: no new spawn site. `glab` and `gh` still run through
  `ProcessRunner`, which uses `run_supervised` with attribution and exit
  reporting.
- Shutdown: background refreshes are cancelled by the existing service
  cancellation token before the profiler's teardown.
- Events: none added.

## Tests

- Server:
  - `getContext` returns the stored context with zero host calls after the
    in-memory answer expires and after a restart, then runs exactly one
    background refresh;
  - concurrent stale calls share one refresh, and a `fresh: true` call joins
    it instead of starting another;
  - after a restart, a self-hosted origin with a stored context row resolves
    with zero CLI calls (no `discover_hosts`), both for `getContext` and for
    `readSnapshot`; with no row, discovery runs;
  - a refresh that fails with `not_authenticated` deletes the context and host
    rows, and the next call reads live;
  - `rescan` bypasses both copies;
  - an unavailable context is never stored;
  - a refresh that finishes after a logout does not write.
- GitHub: `readSnapshot` returns the stored list and detail with zero `gh`
  calls; successful live reads store them; logout deletes them; `subscribe`
  still refuses GitHub.
- Environment: runner tests assert `GLAB_SEND_TELEMETRY=0` on `glab api`,
  `glab mr` and the source-control invocations.
- Approve: on the existing GitLab load harness, `action_detail` has five reads
  in flight at once.
- Client: the panel renders from a stored context, then issues one
  `fresh: true` read and replaces the context with its answer (available with
  changed capabilities, and unavailable); a GitHub list and detail
  paint from `readSnapshot` before the live read finishes; action controls stay
  inactive until the live detail.
- Re-measure on the LAN client with the Profiler: open the panel after a server
  restart; open a viewed MR and a viewed PR. Expected: under 200 ms to paint,
  with live reads finishing about 1.2 s (GitLab) and 0.5 s (GitHub) per wave
  afterwards.

## Living documents

- `docs/architecture/rpc-and-orchestration.md`, Pull Requests flow: stored
  context and its background refresh; stored copies for GitHub (no
  subscription); `glab` telemetry off.
- `docs/integrations/source-control-providers.md`: BiBCode runs `glab` with
  telemetry off.
- Testing runbooks: reviewed; no command changes expected.

## Out of scope

- Head-of-line blocking on remote connections. The Tailscale link to a remote
  client carried 1.08 GB in about 20 minutes, and replies waited 4–11 s behind
  it. That needs per-method outbound byte accounting first and is tracked in
  [#89](https://github.com/mubeda/BibCode/issues/89).
- An HTTP client in place of `glab` and `gh` (alternative 2).
- GitHub polling and subscriptions.
- DNS caching on the host machine. The 270 ms uncached lookup is the
  environment's, but it adds to every `glab` start.
