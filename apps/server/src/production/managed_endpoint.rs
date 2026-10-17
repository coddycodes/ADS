use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use process_wrap::tokio::{ChildWrapper, CommandWrap};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::{
    diagnostics::{
        AttributionKind, AttributionScope, NativeProcessSampler, ProcessAttributionRegistry,
        ProcessRegistration, ProcessRegistrationError, ProcessRegistrationMetadata,
        RegistrationSource,
        profiler::exits::{self, Exited, Watch},
    },
    preview::gateway::relay::{RELAY_PREVIEW_SLOTS, RelayHosts, RelayPreviewHosts},
    process::configure_supervised_background_command_wrap,
};

use super::connect_mcp::EndpointRuntime;

/// How often the running connector is checked for an exit nobody asked for.
const CONNECTOR_WATCH_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct ManagedEndpointRuntime {
    state: Arc<Mutex<Option<ActiveConnector>>>,
    admission: Arc<Semaphore>,
    accepting: Arc<AtomicBool>,
    process_attribution: ProcessAttributionRegistry,
    executable_override: Option<PathBuf>,
    /// The running tunnel's hostnames, shared with the preview gateway.
    relay_hosts: RelayHosts,
    /// Numbers each spawned connector, so its watcher knows when it is no longer the active one.
    spawned: Arc<AtomicU64>,
    watch_interval: Duration,
    #[cfg(test)]
    admission_race: Option<Arc<EndpointAdmissionRace>>,
}

impl Default for ManagedEndpointRuntime {
    fn default() -> Self {
        Self::with_process_attribution(ProcessAttributionRegistry::new())
    }
}

struct ActiveConnector {
    key: String,
    child: Box<dyn ChildWrapper>,
    _registration: ProcessRegistration,
    config: ManagedEndpointConfig,
    /// Its number from `ManagedEndpointRuntime::spawned`.
    generation: u64,
    pid: u32,
    exe: String,
    started: Instant,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManagedEndpointConfig {
    provider_kind: String,
    connector_token: String,
    tunnel_id: Option<String>,
    tunnel_name: Option<String>,
    #[serde(default)]
    hostname: Option<String>,
    #[serde(default)]
    preview_hostnames: Vec<String>,
}

impl ManagedEndpointConfig {
    /// The hostnames to publish, `None` without an endpoint hostname (an older relay).
    /// Every name must be a lowercase DNS name; previews beyond [`RELAY_PREVIEW_SLOTS`] are
    /// ignored so a newer relay that provisions more cannot break this server.
    fn relay_hosts(&self) -> Result<Option<RelayPreviewHosts>, String> {
        let Some(hostname) = &self.hostname else {
            return Ok(None);
        };
        if let Some(invalid) = std::iter::once(hostname)
            .chain(&self.preview_hostnames)
            .find(|name| !is_dns_name(name))
        {
            return Err(format!("invalid managed endpoint hostname: {invalid}"));
        }
        Ok(Some(RelayPreviewHosts {
            hostname: hostname.clone(),
            previews: self
                .preview_hostnames
                .iter()
                .take(RELAY_PREVIEW_SLOTS)
                .cloned()
                .collect(),
        }))
    }
}

/// A lowercase, multi-label DNS name.
fn is_dns_name(name: &str) -> bool {
    name.len() <= 253
        && name.contains('.')
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

#[cfg(test)]
#[derive(Debug)]
struct EndpointAdmissionRace {
    spawned: crate::test_support::FixtureEvent,
    release: crate::test_support::FixtureEvent,
    process_id: std::sync::atomic::AtomicU32,
}

impl ManagedEndpointRuntime {
    fn take_active(&self) -> Option<ActiveConnector> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    #[must_use]
    pub fn with_process_attribution(process_attribution: ProcessAttributionRegistry) -> Self {
        Self {
            state: Arc::default(),
            admission: Arc::new(Semaphore::new(1)),
            accepting: Arc::new(AtomicBool::new(true)),
            process_attribution,
            executable_override: None,
            relay_hosts: RelayHosts::default(),
            spawned: Arc::default(),
            watch_interval: CONNECTOR_WATCH_INTERVAL,
            #[cfg(test)]
            admission_race: None,
        }
    }

    /// Publishes the running tunnel's hostnames into `hosts` (shared with the preview gateway).
    #[must_use]
    pub fn with_relay_hosts(mut self, hosts: RelayHosts) -> Self {
        self.relay_hosts = hosts;
        self
    }

    #[must_use]
    pub fn relay_hosts(&self) -> RelayHosts {
        self.relay_hosts.clone()
    }

    #[must_use]
    pub fn with_executable_override(executable: PathBuf) -> Self {
        Self {
            executable_override: Some(executable),
            ..Self::default()
        }
    }

    #[cfg(all(test, unix))]
    fn with_fixture(
        executable: PathBuf,
        process_attribution: ProcessAttributionRegistry,
        admission_race: Option<Arc<EndpointAdmissionRace>>,
    ) -> Self {
        let mut runtime = Self::with_process_attribution(process_attribution);
        runtime.executable_override = Some(executable);
        runtime.admission_race = admission_race;
        runtime
    }

    #[must_use]
    pub fn endpoint(&self) -> EndpointRuntime {
        let runtime = self.clone();
        EndpointRuntime::new(move |config| {
            let runtime = runtime.clone();
            async move { runtime.apply(config).await }
        })
    }

    pub async fn shutdown(&self) {
        self.accepting.store(false, Ordering::Release);
        let permit = self.admission.acquire().await;
        self.relay_hosts.publish(None);
        let active = self.take_active();
        if let Some(active) = active {
            self.retire(active).await;
        }
        drop(permit);
    }

    pub async fn apply(&self, value: Value) -> Result<Value, String> {
        let _permit = self
            .admission
            .acquire()
            .await
            .map_err(|_| "managed endpoint admission is unavailable".to_owned())?;
        if !self.accepting.load(Ordering::Acquire) {
            return Err(ProcessRegistrationError::Shutdown.to_string());
        }
        if value.is_null() {
            self.relay_hosts.publish(None);
            if let Some(active) = self.take_active() {
                self.retire(active).await;
            }
            return Ok(json!({ "status": "disabled" }));
        }
        let config: ManagedEndpointConfig = serde_json::from_value(value)
            .map_err(|error| format!("invalid managed endpoint config: {error}"))?;
        if config.provider_kind != "cloudflare_tunnel" {
            self.relay_hosts.publish(None);
            if let Some(active) = self.take_active() {
                self.retire(active).await;
            }
            return Ok(json!({
                "status": "unsupported",
                "providerKind": config.provider_kind,
            }));
        }
        if config.connector_token.trim().is_empty() {
            return Err("connector token must not be empty".to_owned());
        }
        let hosts = config.relay_hosts()?;
        let key = format!(
            "{}\0{}\0{}",
            config.connector_token,
            config.tunnel_id.as_deref().unwrap_or_default(),
            config.tunnel_name.as_deref().unwrap_or_default(),
        );
        let previous = self.take_active();
        if let Some(mut active) = previous {
            if active.key == key {
                match connector_is_live(&mut *active.child) {
                    Ok(true) => {
                        // Same connector: keep it running and only republish the hostnames.
                        active.config = config;
                        self.relay_hosts.publish(hosts);
                        let status = running_status(&active);
                        *self
                            .state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(active);
                        return Ok(status);
                    }
                    Ok(false) => {}
                    Err(error) => {
                        self.retire(active).await;
                        return Err(error);
                    }
                }
            }
            self.retire(active).await;
        }
        let executable = self.executable_override.clone().or_else(|| {
            executable_on_path(if cfg!(windows) {
                "bibcode-connect.exe"
            } else {
                "bibcode-connect"
            })
        });
        let Some(executable) = executable else {
            return Ok(failed_status(&config, "The relay client is not installed."));
        };
        let exe = executable.to_string_lossy().into_owned();
        let mut command = CommandWrap::with_new(executable, |command| {
            command
                .args(["tunnel", "run"])
                .env("TUNNEL_TOKEN", &config.connector_token)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
        });
        configure_supervised_background_command_wrap(&mut command);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => return Ok(failed_status(&config, &error.to_string())),
        };
        #[cfg(test)]
        if let Some(race) = self.admission_race.as_ref() {
            race.process_id
                .store(child.id().unwrap_or_default(), Ordering::Release);
            race.spawned.publish();
            race.release.wait_after(0).await;
        }
        let identity = child
            .id()
            .and_then(|pid| NativeProcessSampler::process_identity(pid).ok());
        let Some(identity) = identity else {
            terminate_unregistered_connector(&mut *child).await;
            return Err("spawned managed endpoint has no stable process identity".to_owned());
        };
        match connector_is_live(&mut *child) {
            Ok(true) => {}
            Ok(false) => {
                terminate_unregistered_connector(&mut *child).await;
                return Err("managed endpoint exited before ownership admission".to_owned());
            }
            Err(error) => {
                terminate_unregistered_connector(&mut *child).await;
                return Err(error);
            }
        }
        let pid = identity.pid;
        let registration = match self.process_attribution.register_identity(
            identity,
            ProcessRegistrationMetadata {
                scope: AttributionScope::External,
                kind: AttributionKind::Helper,
                label: "managed endpoint tunnel".to_owned(),
                source: RegistrationSource::Helper,
                driver: None,
            },
        ) {
            Ok(registration) => registration,
            Err(
                error @ (ProcessRegistrationError::Shutdown | ProcessRegistrationError::Capacity),
            ) => {
                terminate_unregistered_connector(&mut *child).await;
                return Err(error.to_string());
            }
        };
        let generation = self.spawned.fetch_add(1, Ordering::Relaxed) + 1;
        let active = ActiveConnector {
            key,
            child,
            _registration: registration,
            config,
            generation,
            pid,
            exe,
            started: Instant::now(),
        };
        let status = running_status(&active);
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(active);
        self.relay_hosts.publish(hosts);
        self.watch_connector(generation);
        Ok(status)
    }

    /// Watches connector `generation` while it is the active one. An exit nobody asked for
    /// (apply and shutdown take the connector out before stopping it) withdraws the
    /// published hostnames, is reported (`report_connector_exit`), and ends the rest of the
    /// connector's process group. The watcher holds the runtime weakly, so a runtime dropped
    /// without shutdown still drops (and kills) its connector.
    fn watch_connector(&self, generation: u64) {
        let state = Arc::downgrade(&self.state);
        let admission = Arc::downgrade(&self.admission);
        let relay_hosts = self.relay_hosts.clone();
        let interval = self.watch_interval;
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(interval);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticks.tick().await;
            loop {
                ticks.tick().await;
                let (Some(state), Some(admission)) = (state.upgrade(), admission.upgrade()) else {
                    return;
                };
                // Serialized with apply and shutdown, which take the connector out of the
                // state before stopping it.
                let Ok(_permit) = admission.acquire().await else {
                    return;
                };
                let exited = {
                    let mut guard = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    match guard.as_ref() {
                        Some(active) if active.generation == generation => {
                            // Observed without reaping, so the group stays ours to end.
                            crate::process::exited_status(&*active.child)
                                .and_then(|status| guard.take().map(|active| (active, status)))
                        }
                        _ => return,
                    }
                };
                if let Some((active, status)) = exited {
                    relay_hosts.publish(None);
                    report_connector_exit(&active, &status);
                    terminate_connector(active).await;
                    return;
                }
            }
        });
    }

    /// Stops a connector taken out of the state. One that already exited died on its own
    /// since the last check, so it is reported first.
    async fn retire(&self, active: ActiveConnector) {
        if let Some(status) = crate::process::exited_status(&*active.child) {
            report_connector_exit(&active, &status);
        }
        self.relay_hosts.publish(None);
        terminate_connector(active).await;
    }
}

/// An exit of the connector nobody asked for: logged, and with the Profiler on a
/// `subprocess.exit` event and `subprocess.crash` finding.
fn report_connector_exit(active: &ActiveConnector, status: &std::process::ExitStatus) {
    tracing::warn!(
        %status,
        tunnel_id = active.config.tunnel_id.as_deref().unwrap_or_default(),
        "BiBCode Connect tunnel connector exited"
    );
    if exits::watching() {
        let label = std::path::Path::new(&active.exe).file_name().map_or_else(
            || active.exe.clone(),
            |name| name.to_string_lossy().into_owned(),
        );
        exits::report_exit(
            Watch::Failures,
            &Exited {
                kind: "command",
                label: &label,
                exe: &active.exe,
                pid: active.pid,
                started: active.started,
            },
            status,
            false,
        );
    }
}

fn connector_is_live(child: &mut dyn ChildWrapper) -> Result<bool, String> {
    #[cfg(unix)]
    {
        let process_id = child
            .id()
            .ok_or_else(|| "managed endpoint PID is unavailable".to_owned())?;
        let mut information = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: the writable siginfo is valid and WNOWAIT preserves the
        // leader identity/PGID authority while observing liveness.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                process_id as libc::id_t,
                information.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // SAFETY: successful waitid initialized siginfo.
        let information = unsafe { information.assume_init() };
        // SAFETY: si_pid reads initialized siginfo.
        Ok(unsafe { information.si_pid() } == 0)
    }
    // Never `try_wait`: it consumes the process handle, and `retire` must still read the
    // status of a connector that died on its own to report it.
    #[cfg(not(unix))]
    Ok(crate::process::exited_status(child).is_none())
}

async fn terminate_connector(mut active: ActiveConnector) {
    terminate_unregistered_connector(&mut *active.child).await;
}

async fn terminate_unregistered_connector(child: &mut dyn ChildWrapper) {
    let _ = child.start_kill();
    let _ = child.wait().await;
}

fn running_status(active: &ActiveConnector) -> Value {
    json!({
        "status": "running",
        "providerKind": "cloudflare_tunnel",
        "pid": active.child.id(),
        "tunnelId": active.config.tunnel_id,
        "tunnelName": active.config.tunnel_name,
    })
}

fn failed_status(config: &ManagedEndpointConfig, reason: &str) -> Value {
    json!({
        "status": "failed",
        "providerKind": "cloudflare_tunnel",
        "reason": reason,
        "tunnelId": config.tunnel_id,
        "tunnelName": config.tunnel_name,
    })
}

fn executable_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::test_support::TestSandbox;

    #[cfg(unix)]
    fn tunnel_config() -> Value {
        json!({"providerKind":"cloudflare_tunnel","connectorToken":"fixture-token"})
    }

    #[test]
    fn hostnames_must_be_lowercase_multi_label_dns_names() {
        for good in ["env-p1.zone.test", "a.b", "x1.example.com"] {
            assert!(is_dns_name(good), "{good}");
        }
        for bad in [
            "Env.zone", "env", "-a.zone", "a-.zone", "a..zone", "a_b.zone", "",
        ] {
            assert!(!is_dns_name(bad), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn disabled_invalid_and_unsupported_configs_are_bounded() {
        let runtime = ManagedEndpointRuntime::default();

        assert_eq!(
            runtime.apply(Value::Null).await.unwrap(),
            json!({"status":"disabled"})
        );
        assert!(
            runtime
                .apply(json!({"providerKind":"cloudflare_tunnel"}))
                .await
                .unwrap_err()
                .starts_with("invalid managed endpoint config:")
        );
        assert_eq!(runtime.apply(json!({"providerKind":"future_provider","connectorToken":"ignored","tunnelId":"tunnel-1","tunnelName":"Future tunnel"})).await.unwrap(), json!({"status":"unsupported","providerKind":"future_provider"}));
        assert_eq!(
            runtime
                .apply(json!({"providerKind":"cloudflare_tunnel","connectorToken":"  "}))
                .await
                .unwrap_err(),
            "connector token must not be empty"
        );
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn missing_connector_executable_returns_actionable_status() {
        let runtime = ManagedEndpointRuntime::default();
        let status = runtime.apply(json!({"providerKind":"cloudflare_tunnel","connectorToken":"fixture-token","tunnelId":"tunnel-1","tunnelName":"Fixture tunnel"})).await.unwrap();
        assert_eq!(status["providerKind"], "cloudflare_tunnel");
        assert_eq!(status["tunnelId"], "tunnel-1");
        assert_eq!(status["tunnelName"], "Fixture tunnel");
        assert_eq!(status["status"], "failed");
        assert_eq!(status["reason"], "The relay client is not installed.");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_unrequested_connector_exit_withdraws_hostnames_and_reports_a_crash() {
        let captured = crate::diagnostics::profiler::exits::CapturedExits::start();
        let config = json!({
            "providerKind":"cloudflare_tunnel",
            "connectorToken":"fixture-token",
            "hostname":"env.zone.test",
            "previewHostnames":["env-p1.zone.test"],
        });

        let sandbox = TestSandbox::new("managed-endpoint-exit");
        let dying = sandbox.executable_script("bibcode-connect", "sleep 0.3; exit 3", "");
        let mut runtime =
            ManagedEndpointRuntime::with_fixture(dying, ProcessAttributionRegistry::new(), None);
        runtime.watch_interval = std::time::Duration::from_millis(50);
        let hosts = runtime.relay_hosts();
        assert_eq!(
            runtime.apply(config.clone()).await.unwrap()["status"],
            "running"
        );
        assert!(hosts.current().is_some());
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while hosts.current().is_some() {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the connector's own exit withdraws the hostnames");
        let findings = captured.take();
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(
            findings[0]
                .iter()
                .any(|field| field.starts_with("bibcode-connect")),
            "{findings:?}"
        );

        // A connector stopped on request is never reported.
        let steady = sandbox.executable_script("bibcode-connect-steady", "exec sleep 30", "");
        let mut runtime =
            ManagedEndpointRuntime::with_fixture(steady, ProcessAttributionRegistry::new(), None);
        runtime.watch_interval = std::time::Duration::from_millis(50);
        assert_eq!(runtime.apply(config).await.unwrap()["status"], "running");
        runtime.apply(Value::Null).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(captured.take().is_empty());
        runtime.shutdown().await;
    }

    #[cfg(unix)]
    async fn wait_for_pid(path: &std::path::Path) -> u32 {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(pid) = std::fs::read_to_string(path)
                    .ok()
                    .and_then(|pid| pid.trim().parse().ok())
                {
                    return pid;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the fixture writes its pid")
    }

    #[cfg(unix)]
    async fn wait_until_gone(pid: u32) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            // SAFETY: signal 0 only checks that the process exists.
            while unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("process {pid} kept running"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_crash_found_by_the_next_apply_is_reported_and_its_group_ended() {
        let captured = crate::diagnostics::profiler::exits::CapturedExits::start();
        let sandbox = TestSandbox::new("managed-endpoint-apply-exit");
        let child_pid = sandbox.path("child.pid");
        // The leader leaves a child behind in its process group, then dies on its own.
        let dying = sandbox.executable_script(
            "bibcode-connect",
            &format!(
                "sleep 30 & echo $! > '{}'; sleep 0.3; exit 3",
                child_pid.display()
            ),
            "",
        );
        let mut runtime =
            ManagedEndpointRuntime::with_fixture(dying, ProcessAttributionRegistry::new(), None);
        // The watcher never ticks in time: the next apply finds the crash.
        runtime.watch_interval = std::time::Duration::from_secs(3600);
        assert_eq!(
            runtime.apply(tunnel_config()).await.unwrap()["status"],
            "running"
        );
        let child = wait_for_pid(&child_pid).await;
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        runtime.apply(tunnel_config()).await.unwrap();
        assert_eq!(captured.take().len(), 1, "the earlier crash is reported");
        wait_until_gone(child).await;
        runtime.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_watcher_ends_the_rest_of_a_crashed_connectors_group() {
        let _captured = crate::diagnostics::profiler::exits::CapturedExits::start();
        let sandbox = TestSandbox::new("managed-endpoint-group");
        let child_pid = sandbox.path("child.pid");
        let dying = sandbox.executable_script(
            "bibcode-connect",
            &format!(
                "sleep 30 & echo $! > '{}'; sleep 0.3; exit 3",
                child_pid.display()
            ),
            "",
        );
        let mut runtime =
            ManagedEndpointRuntime::with_fixture(dying, ProcessAttributionRegistry::new(), None);
        runtime.watch_interval = std::time::Duration::from_millis(50);
        runtime.apply(tunnel_config()).await.unwrap();
        wait_until_gone(wait_for_pid(&child_pid).await).await;
        runtime.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_dropped_runtime_drops_its_connector_despite_the_watcher() {
        let sandbox = TestSandbox::new("managed-endpoint-drop");
        let pid_path = sandbox.path("connector.pid");
        let steady = sandbox.executable_script(
            "bibcode-connect",
            &format!("echo $$ > '{}'; exec sleep 30", pid_path.display()),
            "",
        );
        let mut runtime =
            ManagedEndpointRuntime::with_fixture(steady, ProcessAttributionRegistry::new(), None);
        runtime.watch_interval = std::time::Duration::from_millis(50);
        runtime.apply(tunnel_config()).await.unwrap();
        let pid = wait_for_pid(&pid_path).await;
        drop(runtime);
        wait_until_gone(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn freeze_rejects_and_reaps_a_tunnel_spawn_before_registration() {
        let sandbox = TestSandbox::new("managed-endpoint-freeze");
        let pid_path = sandbox.path("tunnel.pid");
        let executable = sandbox.executable_script(
            "bibcode-connect",
            &format!("echo $$ > '{}'; exec sleep 30", pid_path.display()),
            "",
        );
        let registry = ProcessAttributionRegistry::new();
        let race = Arc::new(EndpointAdmissionRace {
            spawned: crate::test_support::FixtureEvent::default(),
            release: crate::test_support::FixtureEvent::default(),
            process_id: std::sync::atomic::AtomicU32::new(0),
        });
        let runtime =
            ManagedEndpointRuntime::with_fixture(executable, registry.clone(), Some(race.clone()));
        let apply_runtime = runtime.clone();
        let apply = tokio::spawn(async move { apply_runtime.apply(tunnel_config()).await });
        race.spawned.wait_after(0).await;
        let pid = race.process_id.load(Ordering::Acquire);
        assert_ne!(pid, 0, "tunnel PID is published before admission");
        assert!(registry.freeze_and_snapshot_identities().is_empty());
        race.release.publish();
        let error = apply
            .await
            .expect("tunnel apply task")
            .expect_err("frozen registry rejects tunnel");
        assert!(
            error.contains("closed for shutdown"),
            "unexpected error: {error}"
        );
        assert!(NativeProcessSampler::process_identity(pid).is_err());
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shutting_down_one_tunnel_runtime_preserves_its_live_peer() {
        let sandbox_a = TestSandbox::new("managed-endpoint-peer-a");
        let sandbox_b = TestSandbox::new("managed-endpoint-peer-b");
        let executable_a = sandbox_a.executable_script("bibcode-connect", "exec sleep 30", "");
        let executable_b = sandbox_b.executable_script("bibcode-connect", "exec sleep 30", "");
        let registry_a = ProcessAttributionRegistry::new();
        let registry_b = ProcessAttributionRegistry::new();
        let runtime_a =
            ManagedEndpointRuntime::with_fixture(executable_a, registry_a.clone(), None);
        let runtime_b =
            ManagedEndpointRuntime::with_fixture(executable_b, registry_b.clone(), None);
        let (status_a, status_b) = tokio::join!(
            runtime_a.apply(tunnel_config()),
            runtime_b.apply(tunnel_config())
        );
        let pid_a = status_a.expect("runtime A tunnel")["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .expect("A PID");
        let pid_b = status_b.expect("runtime B tunnel")["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .expect("B PID");
        let identity_a = NativeProcessSampler::process_identity(pid_a).expect("A identity");
        let identity_b = NativeProcessSampler::process_identity(pid_b).expect("B identity");
        assert_eq!(registry_a.freeze_and_snapshot_identities(), [identity_a]);

        runtime_a.shutdown().await;
        assert!(NativeProcessSampler::process_identity(pid_a).is_err());
        assert_eq!(
            NativeProcessSampler::process_identity(pid_b).expect("B remains live"),
            identity_b
        );
        assert_eq!(registry_b.freeze_and_snapshot_identities(), [identity_b]);
        runtime_b.shutdown().await;
        assert!(NativeProcessSampler::process_identity(pid_b).is_err());
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn capacity_rejection_reaps_the_spawned_tunnel_and_shutdown_is_idempotent() {
        let sandbox = TestSandbox::new("managed-endpoint-capacity");
        let executable = sandbox.executable_script("bibcode-connect", "exec sleep 30", "");
        let registry = ProcessAttributionRegistry::new();
        let registrations = (0..512_u32)
            .map(|index| {
                registry
                    .register_identity(
                        crate::diagnostics::ProcessIdentity {
                            pid: 20_000 + index,
                            started_at: u64::from(index) + 1,
                        },
                        ProcessRegistrationMetadata {
                            scope: AttributionScope::External,
                            kind: AttributionKind::Helper,
                            label: "capacity fixture".to_owned(),
                            source: RegistrationSource::Helper,
                            driver: None,
                        },
                    )
                    .expect("fill process registry")
            })
            .collect::<Vec<_>>();
        let race = Arc::new(EndpointAdmissionRace {
            spawned: crate::test_support::FixtureEvent::default(),
            release: crate::test_support::FixtureEvent::default(),
            process_id: std::sync::atomic::AtomicU32::new(0),
        });
        let runtime =
            ManagedEndpointRuntime::with_fixture(executable, registry, Some(race.clone()));
        let apply_runtime = runtime.clone();
        let apply = tokio::spawn(async move { apply_runtime.apply(tunnel_config()).await });
        race.spawned.wait_after(0).await;
        let pid = race.process_id.load(Ordering::Acquire);
        let identity =
            NativeProcessSampler::process_identity(pid).expect("capacity tunnel identity");
        race.release.publish();
        let error = apply
            .await
            .expect("capacity tunnel apply task")
            .expect_err("full registry rejects tunnel");
        assert!(
            error.contains("capacity is exhausted"),
            "unexpected error: {error}"
        );
        assert!(
            !matches!(NativeProcessSampler::process_identity(pid), Ok(current) if current == identity),
            "capacity-rejected tunnel survived exact cleanup"
        );
        runtime.shutdown().await;
        runtime.shutdown().await;
        drop(registrations);
    }
}
