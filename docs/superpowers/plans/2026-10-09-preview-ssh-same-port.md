# Preview over SSH on the same local port — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Over desktop-managed SSH, a preview of `http://localhost:P` loads at exactly `http://localhost:P` on the client, falling back to a random local port (with a one-time note) when `P` is busy.

**Architecture:** The desktop's existing SSH forward to the server's preview-gateway listener accepts a preferred local port. When `P` is free on both `127.0.0.1` and `[::1]`, the forward binds it on both; otherwise it binds a random `127.0.0.1` port as today. The web resolver passes the canonical port as the preference and builds the bootstrap URL on `http://localhost:P` when it gets it. The gateway, server RPCs, and auth are unchanged.

**Tech Stack:** Rust (Tauri 2 desktop host, tokio, `portpicker`), TypeScript/React web client (Vite+, `vp test`), effect contracts.

**Spec:** `docs/superpowers/specs/2026-10-09-preview-ssh-same-port-design.md`

## Global Constraints

- A preferred port below 1024 is never used; the forward falls back to a random port.
- A preferred port binds both `127.0.0.1:P` and `[::1]:P`; only `127.0.0.1:P` when the client has no IPv6 loopback (binding `[::1]:0` fails).
- Fallback bind is a single `127.0.0.1:<random>` exactly as today.
- A live forward for a remote port is always reused, whatever its local port (a fallback tab never switches origin live).
- Same-port client origin is always `http://localhost:P` (the resolver canonicalizes loopback hosts to `localhost`).
- Fallback note, shown once per environment and canonical origin per app session: "localhost:P is in use on this computer, so this preview runs on a different local port. Apps that expect localhost:P (OAuth sign-in, for example) may not work until you free it." (`localhost:P` is the canonical host and port.)
- No server RPC, schema, or persistence change. Privileged work stays behind `DesktopBridge`.
- Unchanged: forward limits (8 per tunnel, 12 global), LRU eviction, release, `ControlPath=none`, loopback-only binds, auth prompts.

## Review Focus

- A local dev server on `[::1]:P` only (Node ≥ 17 `localhost`) while `127.0.0.1:P` is free must fall back, not bind IPv4 alone — Task 1 test `preferred_port_busy_on_ipv6_falls_back`.
- ssh losing the bind race (port taken between probe and spawn) must retry on a random port, not fail the preview — Task 1 test `preferred_port_lost_to_a_race_retries_on_a_random_port`.
- A wrong password must not be prompted twice because of the race retry — the retry only follows a bind failure (Task 1 `is_forward_bind_failure`, test `auth_failure_is_not_retried_on_a_random_port`).
- Re-resolving an open fallback tab must keep its random port after the preferred port frees up — Task 1 test `live_forward_is_reused_whatever_its_local_port`.
- Repeated navigations in a fallback tab must not repeat the note — Task 2 test `shows the busy-port note once per environment and origin`.

---

### Task 1: Desktop forward prefers the canonical local port

**Files:**
- Modify: `apps/desktop/src-tauri/src/ssh.rs` (`port_forward_plan` ~962, `ensure_port_forward` ~994, new helpers near `wait_for_ssh_port_forward_ready` ~2563, tests ~7422–7720)
- Modify: `apps/desktop/src-tauri/src/bridge.rs:2031-2041` (`desktop_bridge_ssh_forward`)
- Modify: `docs/architecture/remote.md:961-` ("Preview port forwards" paragraph)

**Interfaces:**
- Produces: Tauri command `desktop_bridge_ssh_forward(target, remote_port: u16, preferred_local_port: Option<u16>) -> Result<u16, String>` (JS args `{ target, remotePort, preferredLocalPort }`).
- Produces: `SshEnvironmentManager::ensure_port_forward(&self, app, prompts, target, remote_port: u16, preferred_local_port: Option<u16>) -> Result<u16, String>`.

- [ ] **Step 1: Write the failing tests** (add to the `#[cfg(test)]` module next to `port_forward_args_forward_to_loopback_remote_port`)

```rust
    #[test]
    fn preferred_port_forward_binds_both_loopback_families() {
        let manager = SshEnvironmentManager::with_ssh_program(
            "fixture-ssh",
            SshOperationDeadlines::default(),
        );
        let bind = ForwardBind { port: 5173, ipv6: true };
        let plan = manager
            .port_forward_plan(&fixture_target(), bind, 41000, &SshAuthOptions::batch())
            .expect("plan");
        let forwards: Vec<_> = plan
            .args
            .windows(2)
            .filter(|pair| pair[0] == "-L")
            .map(|pair| pair[1].clone())
            .collect();
        // IPv4 last: the fake ssh records and binds the last -L.
        assert_eq!(
            forwards,
            ["[::1]:5173:127.0.0.1:41000", "127.0.0.1:5173:127.0.0.1:41000"]
        );

        let v4_only = manager
            .port_forward_plan(
                &fixture_target(),
                ForwardBind { port: 5173, ipv6: false },
                41000,
                &SshAuthOptions::batch(),
            )
            .expect("plan");
        assert_eq!(
            v4_only.args.iter().filter(|arg| *arg == "-L").count(),
            1,
            "no IPv6 loopback, one IPv4 bind"
        );
    }

    #[test]
    fn preferred_port_probe() {
        // Below 1024 is never preferred.
        assert_eq!(probe_preferred_port(80), None);
        // A free port is preferred.
        let free = portpicker::pick_unused_port().expect("free port");
        assert_eq!(probe_preferred_port(free).map(|bind| bind.port), Some(free));
        // Busy on IPv4.
        let v4 = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("v4");
        assert_eq!(probe_preferred_port(v4.local_addr().unwrap().port()), None);
    }

    #[test]
    fn preferred_port_busy_on_ipv6_falls_back() {
        let Ok(v6) = std::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, 0)) else {
            return; // no IPv6 loopback on this host
        };
        let port = v6.local_addr().unwrap().port();
        if std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_err() {
            return; // the IPv4 side happens to be taken too; nothing to assert
        }
        assert_eq!(probe_preferred_port(port), None);
    }

    #[test]
    fn forward_bind_failures_are_recognised() {
        assert!(is_forward_bind_failure(
            "SSH port forward exited before becoming ready with status exit status: 255: bind [127.0.0.1]:5173: Address already in use"
        ));
        assert!(is_forward_bind_failure("Could not request local forwarding."));
        assert!(!is_forward_bind_failure("Permission denied (publickey,password)."));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preferred_port_forward_uses_the_canonical_local_port() {
        let fake = fake_ssh::FakeSsh::with_body(LISTENING_PORT_FORWARD);
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);
        let preferred = portpicker::pick_unused_port().expect("free port");

        let local_port = manager
            .ensure_port_forward(app.handle(), &prompts, fixture_target(), 41000, Some(preferred))
            .await
            .expect("forward");

        assert_eq!(local_port, preferred);
        assert_eq!(
            recorded_forwards(&fake),
            vec![format!("127.0.0.1:{preferred}:127.0.0.1:41000")]
        );
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn busy_preferred_port_falls_back_to_a_random_port() {
        let fake = fake_ssh::FakeSsh::with_body(LISTENING_PORT_FORWARD);
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);
        let busy = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("busy");
        let busy_port = busy.local_addr().unwrap().port();

        let local_port = manager
            .ensure_port_forward(app.handle(), &prompts, fixture_target(), 41000, Some(busy_port))
            .await
            .expect("forward");

        assert_ne!(local_port, busy_port);
        assert_eq!(
            recorded_forwards(&fake),
            vec![format!("127.0.0.1:{local_port}:127.0.0.1:41000")]
        );
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preferred_port_lost_to_a_race_retries_on_a_random_port() {
        // The fake refuses the first forward with ssh's bind error, then listens.
        let fake = fake_ssh::FakeSsh::with_body(&format!(
            r##"if [ -z "$*" ] && [ ! -e "$dir/raced" ]; then
  : >"$dir/raced"
  printf 'bind [127.0.0.1]:%s: Address already in use\n' "${{forward#127.0.0.1:}}" >&2
  exit 255
fi
{LISTENING_PORT_FORWARD}"##
        ));
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);
        let preferred = portpicker::pick_unused_port().expect("free port");

        let local_port = manager
            .ensure_port_forward(app.handle(), &prompts, fixture_target(), 41000, Some(preferred))
            .await
            .expect("the race falls back instead of failing");

        assert_ne!(local_port, preferred);
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn auth_failure_is_not_retried_on_a_random_port() {
        let fake = fake_ssh::FakeSsh::with_body(
            "printf 'Permission denied (publickey).\\n' >&2\nprintf x >>\"$dir/attempts\"\nexit 255\n",
        );
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);
        let preferred = portpicker::pick_unused_port().expect("free port");

        manager
            .ensure_port_forward(app.handle(), &prompts, fixture_target(), 41000, Some(preferred))
            .await
            .expect_err("auth failure");

        let attempts = fs::read_to_string(fake.path("attempts")).unwrap_or_default();
        assert_eq!(attempts.len(), 1, "a non-bind failure is not retried");
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn live_forward_is_reused_whatever_its_local_port() {
        let fake = fake_ssh::FakeSsh::with_body(LISTENING_PORT_FORWARD);
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);
        let busy = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("busy");
        let preferred = busy.local_addr().unwrap().port();

        let fallback = manager
            .ensure_port_forward(app.handle(), &prompts, fixture_target(), 41000, Some(preferred))
            .await
            .expect("fallback");
        drop(busy); // the preferred port frees up
        let again = manager
            .ensure_port_forward(app.handle(), &prompts, fixture_target(), 41000, Some(preferred))
            .await
            .expect("reuse");

        assert_eq!(again, fallback, "an open tab keeps its origin");
        assert_eq!(recorded_forwards(&fake).len(), 1);
        manager.shutdown().await;
    }
```

If `fixture_target()` is not in scope for the plan tests, use the `SshEnvironmentTarget` literal from `port_forward_args_forward_to_loopback_remote_port`. If the `auth_failure…` fake trips the existing password-prompt path differently (it runs `run_with_ssh_auth`), assert instead that `attempts` holds no more characters than the existing auth-failure tests in this file record for one `run_with_ssh_auth` call; the point is that no second, random-port round follows.

- [ ] **Step 2: Run tests to verify they fail**

Run: `systemd-run --user --scope -q -p MemoryMax=20G -p MemorySwapMax=0 env CARGO_BUILD_JOBS=4 cargo test -p bibcode-desktop --lib -- preferred_port forward_bind_failures auth_failure_is_not_retried live_forward_is_reused busy_preferred_port`
Expected: compile errors (`ForwardBind`, `probe_preferred_port`, `is_forward_bind_failure` missing; `ensure_port_forward` takes 4 arguments).

- [ ] **Step 3: Implement**

Add near `wait_for_ssh_port_forward_ready`:

```rust
/// The local end of a preview forward: a port on `127.0.0.1`, and on `[::1]`
/// too when `ipv6` (so `localhost` cannot reach another local server on the
/// other family).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ForwardBind {
    port: u16,
    ipv6: bool,
}

/// The bind for a preferred local port when it is free on IPv4 and, when the
/// client has IPv6 loopback, on IPv6. `None` means fall back to a random port.
fn probe_preferred_port(port: u16) -> Option<ForwardBind> {
    if port < 1024 {
        return None;
    }
    std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).ok()?;
    let ipv6 = match std::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, port)) {
        Ok(_) => true,
        // No IPv6 loopback at all: nothing else can answer `localhost` there.
        Err(_) if std::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, 0)).is_err() => false,
        Err(_) => return None,
    };
    Some(ForwardBind { port, ipv6 })
}

/// Whether a failed forward lost its local bind (another process took the
/// port after the probe), as opposed to an authentication or network failure.
fn is_forward_bind_failure(error: &str) -> bool {
    error.contains("Address already in use") || error.contains("Could not request local forwarding")
}
```

Change `port_forward_plan` to take `bind: ForwardBind` instead of `local_port: u16`:

```rust
    fn port_forward_plan(
        &self,
        target: &SshEnvironmentTarget,
        bind: ForwardBind,
        remote_port: u16,
        auth: &SshAuthOptions,
    ) -> Result<SshEnvironmentLaunchPlan, String> {
        let remote = RemoteLaunchResult {
            remote_port,
            // Not a server; the plan's server kind goes unused.
            server_kind: "external".to_string(),
        };
        let mut plan = self.forward_plan(target, bind.port, remote, auth)?;
        let spec = format!("{}:127.0.0.1:{remote_port}", bind.port);
        let index = plan
            .args
            .iter()
            .position(|arg| *arg == spec)
            .ok_or_else(|| "SSH port forward plan lost its forward.".to_string())?;
        plan.args[index] = format!("127.0.0.1:{spec}");
        if bind.ipv6 {
            // Before the IPv4 bind: the readiness probe and ssh's own order agree.
            plan.args
                .splice(index - 1..index - 1, ["-L".to_string(), format!("[::1]:{spec}")]);
        }
        plan.args
            .splice(0..0, ["-o".to_string(), "ControlPath=none".to_string()]);
        Ok(plan)
    }
```

Update the existing `port_forward_args_forward_to_loopback_remote_port` test call to
`port_forward_plan(&target, ForwardBind { port: 45123, ipv6: false }, 5173, …)`.

In `ensure_port_forward`, add the parameter and split the spawn into a helper so it can run twice. Signature:

```rust
    pub async fn ensure_port_forward<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        prompts: &SshPasswordPromptManager,
        target: SshEnvironmentTarget,
        remote_port: u16,
        preferred_local_port: Option<u16>,
    ) -> Result<u16, String> {
```

Keep the early checks, lock, and the live-forward reuse block exactly as they are (a live forward is reused whatever its local port). Replace the block from `let askpass_launcher = self.askpass_launcher()?;` through the `run_with_ssh_auth(...).await?;` that yields `(local_port, mut child)` with:

```rust
        let preferred = preferred_local_port.and_then(probe_preferred_port);
        let (local_port, mut child) = match self
            .spawn_port_forward(app, prompts, &key, &target, remote_port, preferred)
            .await
        {
            // Another process took the preferred port after the probe.
            Err(error) if preferred.is_some() && is_forward_bind_failure(&error) => {
                self.spawn_port_forward(app, prompts, &key, &target, remote_port, None)
                    .await?
            }
            result => result?,
        };
```

and add the helper (the moved body; `None` keeps today's random port):

```rust
    /// Spawns one forward child to `remote_port`, bound as `bind` or, when
    /// `None`, on a random `127.0.0.1` port picked just before each attempt.
    async fn spawn_port_forward<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        prompts: &SshPasswordPromptManager,
        key: &str,
        target: &SshEnvironmentTarget,
        remote_port: u16,
        bind: Option<ForwardBind>,
    ) -> Result<(u16, ManagedSshChild), String> {
        let askpass_launcher = self.askpass_launcher()?;
        let io_runtime = self.io_runtime.handle()?;
        self.run_with_ssh_auth(app, prompts, key, target, |auth| {
            // Each attempt picks its port just before spawning, so a
            // password prompt never leaves a picked port unbound for long.
            let plan = bind
                .map(Ok)
                .unwrap_or_else(|| {
                    portpicker::pick_unused_port()
                        .map(|port| ForwardBind { port, ipv6: false })
                        .ok_or_else(|| {
                            "Could not find an available local SSH port forward port.".to_string()
                        })
                })
                .and_then(|bind| self.port_forward_plan(target, bind, remote_port, &auth));
            let askpass_launcher = askpass_launcher.clone();
            let io_runtime = io_runtime.clone();
            async move {
                let plan = plan?;
                let local_port = plan.local_port;
                let child = run_on_ssh_io(&io_runtime, async move {
                    start_ssh_tunnel(&plan, &auth, askpass_launcher, SshForwardKind::PortForward)
                        .await
                })
                .await?;
                Ok((local_port, child))
            }
        })
        .await
    }
```

Match the real types: use the child type the moved code produced (`ManagedSshChild` per `SshPortForward.child`), and the exact `run_with_ssh_auth` argument types (`&key`/`&target` as in the original call). Keep `let stderr_drain = drain_forward_stderr(&io_runtime, &mut child);` after the match, fetching `io_runtime` with `self.io_runtime.handle()?` there.

Append `, None` to every other `ensure_port_forward(...)` call in `ssh.rs` tests (17 call sites: `rg -n '\.ensure_port_forward\(' apps/desktop/src-tauri/src/ssh.rs`).

In `bridge.rs`:

```rust
#[tauri::command]
pub async fn desktop_bridge_ssh_forward(
    app: AppHandle<DesktopRuntime>,
    ssh: State<'_, SshEnvironmentManager>,
    prompts: State<'_, SshPasswordPromptManager>,
    target: SshEnvironmentTarget,
    remote_port: u16,
    preferred_local_port: Option<u16>,
) -> Result<u16, String> {
    ssh.ensure_port_forward(&app, &prompts, target, remote_port, preferred_local_port)
        .await
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `systemd-run --user --scope -q -p MemoryMax=20G -p MemorySwapMax=0 env CARGO_BUILD_JOBS=4 cargo test -p bibcode-desktop --lib -- port_forward preferred_port forward_bind_failures auth_failure_is_not_retried live_forward_is_reused busy_preferred_port forwards_are_reaped`
Expected: all PASS (existing `port_forward_*` tests included).

- [ ] **Step 5: Update `docs/architecture/remote.md`** ("Preview port forwards"): `DesktopBridge.sshForward(target, remotePort, preferredLocalPort?)`; with a preference of 1024 or more that is free on `127.0.0.1` and (when the client has IPv6 loopback) `[::1]`, the child binds both (`-L [::1]:P:127.0.0.1:R -L 127.0.0.1:P:127.0.0.1:R`); otherwise, or when ssh then reports the bind lost (`Address already in use`, `Could not request local forwarding`), one `127.0.0.1:<random>` bind as before. A live forward for the remote port is reused whatever its local port.

- [ ] **Step 6: Gates and commit**

Run: `cargo fmt --all --check`, `systemd-run --user --scope -q -p MemoryMax=20G -p MemorySwapMax=0 env CARGO_BUILD_JOBS=4 cargo clippy -p bibcode-desktop --all-targets -- -D warnings`, and the full `cargo test -p bibcode-desktop --lib` (same `systemd-run` cap).
Expected: clean, all pass.

```bash
git add apps/desktop/src-tauri/src/ssh.rs apps/desktop/src-tauri/src/bridge.rs docs/architecture/remote.md
git commit -m "feat(desktop): bind SSH preview forwards on the canonical local port when free"
```

---

### Task 2: Web resolver opens SSH previews on the real origin

**Files:**
- Modify: `packages/contracts/src/ipc.ts:1300-1310` (`sshForward` signature + doc)
- Modify: `apps/web/src/tauriDesktopBridge.ts:534-535`
- Modify: `apps/web/src/tauriDesktopBridge.test.ts` (~1206)
- Modify: `apps/web/src/browser/linkNotices.ts` (new `showSamePortBusyNotice`)
- Modify: `apps/web/src/browser/previewGateway.ts` (SSH branch ~236–252, `resetPreviewGatewayForTests` ~369)
- Modify: `apps/web/src/browser/previewGateway.test.ts`
- Modify: `docs/user/workspace-ui.md:1002-1004`, `docs/testing/ssh-environments.md` (section `preview-gateway-over-ssh`), `docs/superpowers/specs/2026-10-07-internal-browser-preview-gateway-design.md` ("Out of scope"), `docs/superpowers/specs/2026-10-09-preview-ssh-same-port-design.md`

**Interfaces:**
- Consumes: Tauri command `desktop_bridge_ssh_forward` with `{ target, remotePort, preferredLocalPort }` (Task 1).
- Produces: `DesktopBridge.sshForward(target, remotePort, preferredLocalPort?: number) => Promise<number>`; `showSamePortBusyNotice(hostPort: string): void`.

- [ ] **Step 1: Write the failing tests** in `previewGateway.test.ts`

Add at the top, after the existing `vi.mock` lines:

```ts
const { showSamePortBusyNotice } = vi.hoisted(() => ({ showSamePortBusyNotice: vi.fn() }));
vi.mock("./linkNotices", () => ({ showSamePortBusyNotice }));
```

and `showSamePortBusyNotice.mockReset();` in `beforeEach`. Update the existing SSH expectation in "resolves SSH loopback through gateway plus local forward" to `expect(sshForward).toHaveBeenCalledWith(sshTarget, 41000, 5173);` (the mock returns 50000, so that test now also covers the fallback). Add:

```ts
  it("opens an SSH preview on the canonical origin when the same local port is free", async () => {
    readPreparedConnection.mockReturnValue(ssh());
    sshForward.mockImplementationOnce(async (_t: unknown, _g: number, preferred?: number) => preferred);
    const gatewayOpen = vi.fn(async () => opened(41000));

    await expect(
      resolveForNavigation({
        environmentId,
        threadId,
        canonicalUrl: "http://127.0.0.1:5173/cb?x=1",
        gatewayOpen,
      }),
    ).resolves.toEqual({
      kind: "ok",
      url: "http://localhost:5173/__bibcode/bootstrap?cap=CAP&to=%2Fcb%3Fx%3D1",
    });
    expect(sshForward).toHaveBeenCalledWith(sshTarget, 41000, 5173);
    expect(canonicalizePreviewUrl("http://localhost:5173/next")).toBe("http://localhost:5173/next");
    expect(showSamePortBusyNotice).not.toHaveBeenCalled();
  });

  it("shows the busy-port note once per environment and origin", async () => {
    readPreparedConnection.mockReturnValue(ssh());
    const gatewayOpen = vi.fn(async () => opened(41000));
    const open = (url: string) =>
      resolveForNavigation({ environmentId, threadId, canonicalUrl: url, gatewayOpen });

    await expect(open("http://localhost:5173/")).resolves.toMatchObject({
      kind: "ok",
      url: expect.stringMatching(/^http:\/\/127\.0\.0\.1:50000\//),
    });
    await open("http://localhost:5173/other");
    await open("http://localhost:8080/");

    expect(showSamePortBusyNotice).toHaveBeenCalledTimes(2);
    expect(showSamePortBusyNotice).toHaveBeenNthCalledWith(1, "localhost:5173");
    expect(showSamePortBusyNotice).toHaveBeenNthCalledWith(2, "localhost:8080");
  });

  it("sends no port preference below 1024", async () => {
    readPreparedConnection.mockReturnValue(ssh());
    const gatewayOpen = vi.fn(async () => opened(41000));

    await resolveForNavigation({ environmentId, threadId, canonicalUrl: "http://localhost/", gatewayOpen });

    expect(sshForward).toHaveBeenCalledWith(sshTarget, 41000, undefined);
    expect(showSamePortBusyNotice).not.toHaveBeenCalled();
  });
```

Check the `canonicalizePreviewUrl` assertion against its real signature in `previewGateway.ts` (it maps a client URL back to its canonical URL); if it takes different arguments, assert instead `isGatewayClientUrl("http://localhost:5173/next")` is `true`, which also proves the identity mapping was installed. In `tauriDesktopBridge.test.ts`, next to the `sshForward(sshTarget, 5173)` case, add one asserting `sshForward(sshTarget, 41000, 5173)` invokes `desktop_bridge_ssh_forward` with `{ target: sshTarget, remotePort: 41000, preferredLocalPort: 5173 }`, following that test's existing invoke-mock pattern.

- [ ] **Step 2: Run tests to verify they fail**

Run (from `apps/web`): `node ../../scripts/run-local-vp.mjs test run src/browser/previewGateway.test.ts src/tauriDesktopBridge.test.ts`
Expected: FAIL (`sshForward` called with 2 arguments; `showSamePortBusyNotice` not exported).

- [ ] **Step 3: Implement**

`packages/contracts/src/ipc.ts`, `sshForward`:

```ts
  /**
   * Forwards `remotePort` on the target's loopback over its live SSH
   * connection and resolves the local port. With `preferredLocalPort` (1024 or
   * more) free on this computer's 127.0.0.1 and ::1, the forward binds it on
   * both; otherwise, or when it is lost to another process, a random local
   * port. Idempotent per remote port: a live forward is reused whatever its
   * local port. The forward ends when the connection ends or reconnects, on
   * `releaseSshForward`, on LRU eviction when its connection holds more than
   * 8 forwards, or on budget eviction when all connections hold more than 12.
   * Eviction takes the least recently used forward, and only after the new
   * one is ready; a later call for the same port opens a new forward.
   */
  sshForward: (
    target: DesktopSshEnvironmentTarget,
    remotePort: number,
    preferredLocalPort?: number,
  ) => Promise<number>;
```

`apps/web/src/tauriDesktopBridge.ts`:

```ts
    sshForward: (target, remotePort, preferredLocalPort) =>
      tauriInvokeDesktop("desktop_bridge_ssh_forward", { target, remotePort, preferredLocalPort }),
```

`apps/web/src/browser/linkNotices.ts`:

```ts
/** An SSH preview fell back to another local port because `hostPort` is taken here. */
export function showSamePortBusyNotice(hostPort: string): void {
  toastManager.add(
    stackedThreadToast({
      type: "info",
      title: `${hostPort} is in use on this computer`,
      description: `This preview runs on a different local port. Apps that expect ${hostPort} (OAuth sign-in, for example) may not work until you free it.`,
    }),
  );
}
```

(Match the field names the other notices in this file pass to `stackedThreadToast`; the visible copy must read as the Global Constraints sentence, split into title and description.)

`apps/web/src/browser/previewGateway.ts`: import `showSamePortBusyNotice` from `"./linkNotices"`; add module state and reset:

```ts
/** `<environmentId> <canonical origin>` pairs whose busy-port note was shown this session. */
const samePortNotices = new Set<string>();
```

and `samePortNotices.clear();` in `resetPreviewGatewayForTests`. In the SSH branch replace the forward call and client origin:

```ts
    const canonicalPort = Number(canonical.port);
    const preferred = canonicalPort >= 1024 ? canonicalPort : undefined;
    let localPort: number;
    try {
      // Forwards end when the managed tunnel reconnects; the bridge call is
      // idempotent while one is alive, so every navigation re-establishes it.
      localPort = await bridge.sshForward(target, gatewayPort, preferred);
    } catch (cause) {
      return fail({
        message: isSshNotActive(cause)
          ? UNREACHABLE_MESSAGES.disconnected(connectionLabel ?? "This environment")
          : retryMessage(label),
        retryable: true,
      });
    }
    if (localPort === preferred) {
      // The real origin: OAuth redirects and absolute URLs to it just work.
      clientOrigin = canonical.origin;
    } else {
      clientOrigin = `http://127.0.0.1:${localPort}`;
      const noticeKey = `${environmentId} ${canonical.origin}`;
      if (preferred !== undefined && !samePortNotices.has(noticeKey)) {
        samePortNotices.add(noticeKey);
        showSamePortBusyNotice(canonical.host);
      }
    }
    // Mapped before settling: settling may release (and forget) it at once.
    canonicalOrigins.set(clientOrigin, canonical.origin);
    settle({ target, gatewayPort }, clientOrigin);
```

(`canonical.port` is `""` for a default port, giving `0` and no preference; `canonical.host` is `localhost:5173`.)

- [ ] **Step 4: Run tests to verify they pass**

Run (from `apps/web`): `node ../../scripts/run-local-vp.mjs test run src/browser/previewGateway.test.ts src/tauriDesktopBridge.test.ts src/browser src/components/preview`
Expected: PASS.

- [ ] **Step 5: Documentation**

- `docs/user/workspace-ui.md` (SSH bullet ~1002): "desktop-managed SSH, on desktop only; the desktop forwards the gateway port over the SSH connection. The preview loads from the same address as on the server (`http://localhost:5173`) when that port is free on your computer; otherwise from `http://127.0.0.1:<local port>`, and a note says the port is in use and that apps expecting it (OAuth sign-in, for example) may not work until you free it."
- `docs/testing/ssh-environments.md`, section "Preview gateway over SSH": add a step — preview a dev server whose page redirects to an absolute `http://localhost:<port>/callback`; the address bar and origin stay `localhost:<port>`. Then hold that port locally on IPv4 (`python3 -m http.server <port> --bind 127.0.0.1`) and, separately, on IPv6 (`--bind ::1`); each must open on a `127.0.0.1:<random>` origin and show the in-use note once.
- Phase 1 spec "Out of scope": replace "SSH same-port forwarding" with "SSH same-port forwarding (see `2026-10-09-preview-ssh-same-port-design.md`)".
- Same-port spec: in §2 replace "(`http://localhost:5173`, or `127.0.0.1` / `[::1]` if the URL used that host)" with "(always `http://localhost:P`: the resolver canonicalizes loopback hosts to `localhost`)"; in §1 replace the sentence "It honours the preference when it creates a forward, or when the live forward for that remote port sits on a different local port than the preference; otherwise it reuses the live forward as today." with "It honours the preference only when it creates a forward; a live forward for that remote port is reused whatever its local port, so an open fallback tab never switches origin."; in §4 replace "A live forward on the preferred port is reused; one on another port is replaced when the preference is free." with "A live forward is reused whatever its local port."

- [ ] **Step 6: Gates, reviews, and commit**

Run from the repo root: `node scripts/run-local-vp.mjs check`, `node scripts/run-local-vp.mjs run -r --concurrency-limit 1 typecheck`, and from `apps/web` the full `node ../../scripts/run-local-vp.mjs test run`.
Review the new notice against `UI.md` (actionable, names the port and the consequence) and record it in the final report; no React component changed, so state the `vercel-react-best-practices` review as not applicable.

```bash
git add packages/contracts/src/ipc.ts apps/web/src/tauriDesktopBridge.ts apps/web/src/tauriDesktopBridge.test.ts apps/web/src/browser/linkNotices.ts apps/web/src/browser/previewGateway.ts apps/web/src/browser/previewGateway.test.ts docs/user/workspace-ui.md docs/testing/ssh-environments.md docs/superpowers/specs/2026-10-07-internal-browser-preview-gateway-design.md docs/superpowers/specs/2026-10-09-preview-ssh-same-port-design.md
git commit -m "feat(web): open SSH previews on the real localhost origin when the port is free"
```

---

### Task 3: Preview storage per environment (spec §6)

**Files:**
- Modify: `apps/desktop/src-tauri/src/preview/host.rs` (`create_tab` profile selection), `apps/desktop/src-tauri/src/preview/commands.rs` (`desktop_preview_create_tab`, `desktop_preview_clear_data`)
- Modify: `packages/contracts/src/ipc.ts` (`DesktopPreviewBridge.createTab`)
- Modify: `apps/web/src/tauriPreviewBridge.ts` (+ test), `apps/web/src/browser/desktopTabLifetime.ts` (+ test), `apps/web/src/browser/DesktopPreviewTabHosts.tsx`, `apps/web/src/components/preview/PreviewView.tsx`
- Create: `apps/web/src/browser/previewPartition.ts` (+ test)
- Modify: `docs/architecture/overview.md` (single native view → one per environment), `docs/testing/ssh-environments.md`

**Interfaces:**
- Produces: Rust `preview_profile_dir_name(environment_id: Option<&str>) -> String` and `preview_data_store_id(environment_id: Option<&str>) -> [u8; 16]`; Tauri `desktop_preview_create_tab(tab_id, environment_id: Option<String>)`.
- Produces: TS `DesktopPreviewBridge.createTab(tabId: string, environmentId?: string | null)`; `previewPartitionFor(environmentId: EnvironmentId): string | null` (null for the primary environment); `acquireDesktopTab(tabId, partition)`; `navigateDesktopTab(tabId, partition, url, shouldNavigate?)`.

- [ ] **Step 1: Failing tests.** Rust (`host.rs` tests): `preview_profile_dir_name(None) == "preview-profile"`; for `Some("env-a")` it is `preview-profiles/<32 lowercase hex>`, stable across calls, different for `Some("env-b")`, and never contains the id (`Some("../evil")` yields only hex after the prefix); `preview_data_store_id(None) == *b"bibcodepreview01"`, `Some` ids differ from it and from each other. Web: `tauriPreviewBridge.test.ts` — tabs of the same partition reuse one native host (one `desktop_preview_create_tab`); a tab of another partition creates a second host with `{ tabId, environmentId }` and the first is never closed; commands, state, and new-window events route through each tab's own host; a hidden host's events map to its last active logical tab; the existing single-partition tests keep passing. `desktopTabLifetime.test.ts` — `createTab` receives the partition. `previewPartition.test.ts` — primary environment → `null`, another → its id.
- [ ] **Step 2: Run them; expect failures** (missing functions, extra argument).
- [ ] **Step 3: Implement.** Native: derive the profile from the partition (`sha2::Sha256` over `"bibcode-preview:" + id`); `create_tab(app, tab_id, environment_id)`; `clear_data` loops over every `preview-` webview. Web: replace `nativeHostTabId` with `hostByPartition: Map<string, string>` (key `""` for null), `partitionOfTab: Map<string, string>`, `activeByHost: Map<string, string | null>`; a new host applies the tab's stored bounds after creation.
- [ ] **Step 4: Run tests; expect PASS.** Rust: `cargo test -p bibcode-desktop --lib preview`; web: `vp test run src/tauriPreviewBridge.test.ts src/browser`.
- [ ] **Step 5: Docs** (overview.md native view sentence; runbook step: two SSH environments both previewing port 8123; `localStorage.setItem("probe", "<environment>")` in one is absent in the other; the local environment keeps its earlier preview data).
- [ ] **Step 6: Gates and commit** (fmt, desktop clippy, desktop lib tests, `vp check`, typecheck, web suite; Codex review). `git commit -m "feat(desktop): keep preview storage separate per environment"`

