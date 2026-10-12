use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
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
    },
    preview::gateway::relay::{RELAY_PREVIEW_SLOTS, RelayHosts, RelayPreviewHosts},
    process::configure_supervised_background_command_wrap,
};

use super::connect_mcp::EndpointRuntime;

#[derive(Clone)]
pub struct ManagedEndpointRuntime {
    state: Arc<Mutex<Option<ActiveConnector>>>,
    admission: Arc<Semaphore>,
    accepting: Arc<AtomicBool>,
    process_attribution: ProcessAttributionRegistry,
    executable_override: Option<PathBuf>,
    /// The running tunnel's hostnames, shared with the preview gateway.
    relay_hosts: RelayHosts,
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
            terminate_connector(active).await;
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
                terminate_connector(active).await;
            }
            return Ok(json!({ "status": "disabled" }));
        }
        let config: ManagedEndpointConfig = serde_json::from_value(value)
            .map_err(|error| format!("invalid managed endpoint config: {error}"))?;
        if config.provider_kind != "cloudflare_tunnel" {
            self.relay_hosts.publish(None);
            if let Some(active) = self.take_active() {
                terminate_connector(active).await;
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
                        self.relay_hosts.publish(None);
                        terminate_connector(active).await;
                        return Err(error);
                    }
                }
            }
            self.relay_hosts.publish(None);
            terminate_connector(active).await;
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
        let active = ActiveConnector {
            key,
            child,
            _registration: registration,
            config,
        };
        let status = running_status(&active);
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(active);
        self.relay_hosts.publish(hosts);
        Ok(status)
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
    #[cfg(not(unix))]
    child
        .try_wait()
        .map(|status| status.is_none())
        .map_err(|error| error.to_string())
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
