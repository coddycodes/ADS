use std::{
    fmt,
    io::{self, BufRead},
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(unix)]
use std::{fs::File, io::BufReader, os::fd::FromRawFd};

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Deserialize;
use thiserror::Error;
use url::Url;

use crate::data_root::{DataRootError, DataRootRequest, DataRootSource, ResolvedDataRoot};
use crate::diagnostics::profiler::cli::ProfilerArgs;
use crate::persistence::StorageInstanceId;
use crate::remote_update::{RemoteUpdateInstallKind, RemoteUpdateSupport};
use crate::static_assets::{StaticDirError, StaticDirSource, resolve_static_dir};

pub const DEFAULT_PORT: u16 = 3773;

/// The label desktop-owned servers declare, and the default for embedders that
/// build a [`ServerConfig`] directly.
const LOCAL_ENVIRONMENT_LABEL: &str = "Local";

/// This machine's hostname, trimmed, or `None` when the host reports none.
pub(crate) fn machine_host_name() -> Option<String> {
    non_blank_host_name(sysinfo::System::host_name())
}

fn non_blank_host_name(host_name: Option<String>) -> Option<String> {
    host_name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
}

/// A headless server names itself after its machine, so other devices can
/// tell it apart from their own "Local" environment.
fn headless_environment_label(host_name: Option<String>) -> String {
    host_name.unwrap_or_else(|| LOCAL_ENVIRONMENT_LABEL.to_owned())
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum ServerMode {
    Desktop,
    #[default]
    Web,
}

impl fmt::Display for ServerMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Desktop => formatter.write_str("desktop"),
            Self::Web => formatter.write_str("web"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub mode: ServerMode,
    pub host: String,
    pub port: u16,
    pub base_dir: PathBuf,
    pub data_root_request: DataRootRequest,
    pub resolved_data_root: Option<ResolvedDataRoot>,
    pub static_dir: Option<PathBuf>,
    pub static_dir_source: Option<StaticDirSource>,
    pub dev_url: Option<Url>,
    pub no_browser: bool,
    pub startup_pairing_offer: bool,
    pub desktop_bootstrap_token: Option<String>,
    /// True only for a desktop-owned server launched through the WSL bootstrap transport.
    #[doc(hidden)]
    pub desktop_wsl_transport: bool,
    pub unsafe_no_auth: bool,
    pub environment_id: String,
    pub environment_label: String,
    pub server_version: String,
    pub storage_instance_id: Option<StorageInstanceId>,
    /// Random per start; set by `ServerRuntime`; never persisted.
    pub boot_id: Option<uuid::Uuid>,
    /// How this server can be updated remotely (spec section 4.5). Headless
    /// default is manual; the desktop host overrides at launch.
    pub remote_update_support: RemoteUpdateSupport,
    pub(crate) update_maintenance_drain_timeout: Duration,
    pub(crate) update_maintenance_lease: Duration,
    pub(crate) hosting_executable_dir_for_integration_test: Option<PathBuf>,
    /// How long the listener bind keeps retrying while its port is still in
    /// use. `None`, the default, fails the bind at once.
    pub(crate) listener_bind_retry: Option<Duration>,
}

impl ServerConfig {
    pub fn new(base_dir: impl AsRef<Path>) -> Self {
        let base_dir = base_dir.as_ref().to_path_buf();
        Self {
            mode: ServerMode::Web,
            host: "127.0.0.1".to_owned(),
            port: DEFAULT_PORT,
            data_root_request: DataRootRequest::explicit(
                DataRootSource::Cli,
                base_dir.clone(),
                dirs::home_dir().unwrap_or_default(),
            ),
            resolved_data_root: None,
            base_dir,
            static_dir: None,
            static_dir_source: None,
            dev_url: None,
            no_browser: false,
            startup_pairing_offer: true,
            desktop_bootstrap_token: None,
            desktop_wsl_transport: false,
            unsafe_no_auth: false,
            environment_id: "local".to_owned(),
            environment_label: LOCAL_ENVIRONMENT_LABEL.to_owned(),
            server_version: env!("CARGO_PKG_VERSION").to_owned(),
            storage_instance_id: None,
            boot_id: None,
            remote_update_support: RemoteUpdateSupport::manual(),
            update_maintenance_drain_timeout: Duration::from_secs(30),
            update_maintenance_lease: Duration::from_secs(90),
            hosting_executable_dir_for_integration_test: None,
            listener_bind_retry: None,
        }
    }

    #[must_use]
    pub fn with_bind(mut self, host: impl Into<String>, port: u16) -> Self {
        self.host = host.into();
        self.port = port;
        self
    }

    #[must_use]
    pub fn with_remote_update_support(mut self, support: RemoteUpdateSupport) -> Self {
        self.remote_update_support = support;
        self
    }

    /// Keeps retrying the listener bind for up to `window` while the port is
    /// still in use, backing off from 25 ms to 250 ms between attempts. For an
    /// embedder restarting a server on a port its predecessor has only just
    /// released; when the window ends, startup fails with the last bind error.
    #[must_use]
    pub fn with_listener_bind_retry(mut self, window: Duration) -> Self {
        self.listener_bind_retry = Some(window);
        self
    }

    pub fn with_desktop(mut self, bootstrap_token: impl Into<String>) -> Result<Self, ConfigError> {
        let bootstrap_token = bootstrap_token.into();
        if bootstrap_token.trim().is_empty() {
            return Err(ConfigError::EmptyDesktopBootstrapToken);
        }
        self.mode = ServerMode::Desktop;
        self.no_browser = true;
        self.desktop_bootstrap_token = Some(bootstrap_token);
        Ok(self)
    }

    #[must_use]
    pub fn with_static_dir(mut self, static_dir: impl AsRef<Path>) -> Self {
        self.static_dir = Some(static_dir.as_ref().to_path_buf());
        self.static_dir_source = Some(StaticDirSource::Explicit);
        self
    }

    #[must_use]
    pub fn with_dev_url(mut self, dev_url: Url) -> Self {
        self.dev_url = Some(dev_url);
        self
    }

    #[must_use]
    pub fn with_unsafe_no_auth(mut self) -> Self {
        self.unsafe_no_auth = true;
        self
    }

    /// Overrides maintenance timing for deterministic integration tests.
    #[doc(hidden)]
    #[must_use]
    pub fn with_update_maintenance_timing_for_integration_test(
        mut self,
        drain_timeout: Duration,
        lease: Duration,
    ) -> Self {
        self.update_maintenance_drain_timeout = drain_timeout;
        self.update_maintenance_lease = lease;
        self
    }

    /// Pins hosting commands to a test-owned directory; Git remains on PATH.
    #[doc(hidden)]
    #[must_use]
    pub fn with_hosting_executable_dir_for_integration_test(mut self, dir: PathBuf) -> Self {
        self.hosting_executable_dir_for_integration_test = Some(dir);
        self
    }

    #[must_use]
    pub fn state_dir(&self) -> PathBuf {
        self.base_dir.join(if self.dev_url.is_some() {
            "dev"
        } else {
            "userdata"
        })
    }

    #[must_use]
    pub fn database_path(&self) -> PathBuf {
        self.state_dir().join("state.sqlite")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_root::DataRootSource;
    use clap::{CommandFactory, FromArgMatches, Parser};

    #[test]
    fn owned_builder_inputs_cover_desktop_and_static_configuration() {
        let state_dir = "state".to_owned();
        let static_dir = "static".to_owned();
        let config = ServerConfig::new(state_dir)
            .with_static_dir(static_dir)
            .with_desktop("desktop-token".to_owned())
            .expect("desktop config should build");
        assert_eq!(config.mode, ServerMode::Desktop);
        assert_eq!(
            config.desktop_bootstrap_token.as_deref(),
            Some("desktop-token")
        );
        assert_eq!(config.static_dir, Some(PathBuf::from("static")));
        assert!(
            ServerConfig::new("state")
                .with_desktop(String::new())
                .is_err()
        );
    }

    #[test]
    fn server_config_defaults_to_manual_remote_update_support() {
        let config = ServerConfig::new("/tmp/coddy-studio-test");
        assert_eq!(
            config.remote_update_support,
            crate::remote_update::RemoteUpdateSupport {
                install_mode: crate::remote_update::RemoteUpdateInstallMode::Manual,
                reason: crate::remote_update::RemoteUpdateSupportReason::ManualUpdateRequired,
                install_kind: RemoteUpdateInstallKind::Unknown,
            }
        );
    }

    #[test]
    fn listener_bind_retry_is_opt_in() {
        assert_eq!(
            ServerConfig::new("/tmp/coddy-studio-test").listener_bind_retry,
            None
        );
        assert_eq!(
            ServerConfig::new("/tmp/coddy-studio-test")
                .with_listener_bind_retry(Duration::from_secs(3))
                .listener_bind_retry,
            Some(Duration::from_secs(3))
        );
    }

    #[test]
    fn startup_pairing_offer_defaults_on_and_global_disable_flag_turns_it_off() {
        let default = Cli::try_parse_from(["coddy-studio", "serve"])
            .expect("parse default serve CLI")
            .into_server_config()
            .expect("build default server config");
        assert!(default.startup_pairing_offer);

        let disabled = Cli::try_parse_from(["coddy-studio", "serve", "--no-startup-pairing-offer"])
            .expect("parse disabled startup offer CLI")
            .into_server_config()
            .expect("build disabled startup offer config");
        assert!(!disabled.startup_pairing_offer);
    }

    #[test]
    fn headless_server_declares_its_hostname_as_the_environment_label() {
        let config = Cli::try_parse_from(["coddy-studio", "serve"])
            .expect("parse serve CLI")
            .into_server_config()
            .expect("build serve config");
        let hostname = machine_host_name();

        assert_eq!(config.mode, ServerMode::Web);
        assert_eq!(
            config.environment_label,
            hostname.unwrap_or_else(|| "Local".to_owned())
        );
    }

    #[test]
    fn browser_launching_commands_keep_the_local_environment_label() {
        for args in [
            vec!["coddy-studio"],
            vec!["coddy-studio", "start"],
            vec!["coddy-studio", "start", "--no-browser"],
        ] {
            let config = Cli::try_parse_from(args)
                .expect("parse browser CLI")
                .into_server_config()
                .expect("build browser config");
            assert_eq!(config.mode, ServerMode::Web);
            assert_eq!(config.environment_label, "Local");
        }
    }

    #[test]
    fn host_names_are_trimmed_and_a_missing_name_keeps_the_local_label() {
        assert_eq!(
            non_blank_host_name(Some("  ai-server\n".to_owned())),
            Some("ai-server".to_owned())
        );
        assert_eq!(non_blank_host_name(Some(" \t ".to_owned())), None);
        assert_eq!(non_blank_host_name(None), None);
        assert_eq!(
            headless_environment_label(Some("ai-server".to_owned())),
            "ai-server"
        );
        assert_eq!(headless_environment_label(None), "Local");
    }

    #[test]
    fn desktop_mode_and_embedded_servers_keep_the_local_environment_label() {
        let desktop = Cli::try_parse_from(["coddy-studio", "--mode", "desktop", "serve"])
            .expect("parse desktop serve CLI")
            .into_server_config()
            .expect("build desktop serve config");

        assert_eq!(desktop.mode, ServerMode::Desktop);
        assert_eq!(desktop.environment_label, "Local");
        assert_eq!(
            ServerConfig::new("/tmp/coddy-studio-test").environment_label,
            "Local"
        );
    }

    #[test]
    fn desktop_bootstrap_wsl_transport_is_explicit_and_defaults_closed() {
        let base = serde_json::json!({
            "mode": "desktop",
            "noBrowser": true,
            "port": 3773,
            "coddyStudioHome": null,
            "host": "0.0.0.0",
            "desktopBootstrapToken": "token",
            "tailscaleServeEnabled": false,
            "tailscaleServePort": 443
        });
        let native: DesktopBootstrap =
            serde_json::from_value(base.clone()).expect("native bootstrap should decode");
        assert!(!native.wsl_transport);
        let mut wsl = base;
        wsl["wslTransport"] = serde_json::json!(true);
        let wsl: DesktopBootstrap =
            serde_json::from_value(wsl).expect("WSL bootstrap should decode");
        assert!(wsl.wsl_transport);
    }

    #[test]
    fn cli_base_dir_preserves_the_raw_cli_data_root_request() {
        let config = Cli::try_parse_from([
            "coddy-studio",
            "serve",
            "--base-dir",
            "/var/lib/coddy-studio",
        ])
        .expect("parse CLI")
        .into_server_config()
        .expect("build server config");

        assert_eq!(config.data_root_request.source, DataRootSource::Cli);
        assert_eq!(
            config.data_root_request.requested,
            Some(PathBuf::from("/var/lib/coddy-studio"))
        );
    }

    #[test]
    fn cli_discovers_packaged_web_from_the_injected_executable() {
        let distribution = tempfile::tempdir().expect("distribution root");
        let executable = distribution.path().join("coddy-studio");
        let web = distribution.path().join("web");
        std::fs::create_dir(&web).expect("web directory");
        std::fs::write(&executable, b"binary").expect("binary fixture");
        std::fs::write(web.join("index.html"), b"<main>Coddy Studio</main>")
            .expect("web entry point");

        let action = Cli::try_parse_from(["coddy-studio", "serve"])
            .expect("parse CLI")
            .into_action_with_executable(&executable)
            .expect("build server action");
        let CliAction::Run(config) = action else {
            panic!("serve must produce a run action");
        };

        assert_eq!(config.static_dir, Some(web));
        assert_eq!(config.static_dir_source, Some(StaticDirSource::Packaged));
        assert_eq!(
            config.remote_update_support.install_kind,
            crate::remote_update::RemoteUpdateInstallKind::Archive
        );
    }

    #[test]
    fn environment_base_dir_preserves_the_raw_environment_data_root_request() {
        let request = select_data_root_request(
            None,
            Some(std::ffi::OsString::from("/var/lib/coddy-studio")),
            None,
            PathBuf::from("/home/alice"),
        );

        assert_eq!(request.source, DataRootSource::Environment);
        assert_eq!(
            request.requested,
            Some(PathBuf::from("/var/lib/coddy-studio"))
        );
    }

    #[test]
    fn pairing_issue_resolves_the_cli_data_root_and_is_not_a_server_command() {
        let temp = tempfile::tempdir().expect("temporary base directory");
        let base_dir = temp.path().to_string_lossy().into_owned();

        let action = pairing_cli_without_dev_env(&[
            "coddy-studio",
            "pairing",
            "issue",
            "--base-dir",
            base_dir.as_str(),
            "--label",
            "SSH bootstrap",
            "--json",
        ])
        .into_action()
        .expect("build pairing action");
        let CliAction::Pairing(PairingCommand::Issue {
            root,
            dev_url,
            label,
            json,
        }) = action
        else {
            panic!("pairing issue must produce a pairing action");
        };
        assert_eq!(root.requested, PathBuf::from(base_dir.as_str()));
        assert_eq!(dev_url, None);
        assert_eq!(label.as_deref(), Some("SSH bootstrap"));
        assert!(json);

        let error = Cli::try_parse_from([
            "coddy-studio",
            "pairing",
            "issue",
            "--base-dir",
            base_dir.as_str(),
        ])
        .expect("parse pairing issue CLI")
        .into_server_config()
        .expect_err("pairing issue is not a server command");
        assert!(matches!(error, ConfigError::PairingCommandIsNotServer));
    }

    #[test]
    fn service_install_builds_a_spec_from_the_global_server_flags_only() {
        let executable = PathBuf::from("/usr/bin/coddy-studio");
        let action = Cli::try_parse_from([
            "coddy-studio",
            "service",
            "install",
            "--host",
            "100.105.196.60",
            "--port",
            "4000",
            "--base-dir",
            "/srv/coddy-studio",
            "--bootstrap-fd",
            "7",
            "--json",
        ])
        .expect("parse service install CLI")
        .into_action_with_executable(&executable)
        .expect("build service action");
        let CliAction::Service(ServiceCommand::Install { spec, json }) = action else {
            panic!("service install must produce a service action");
        };
        assert_eq!(spec.executable, executable);
        assert_eq!(spec.host, "100.105.196.60");
        assert_eq!(spec.port, 4000);
        assert_eq!(spec.base_dir, Some(PathBuf::from("/srv/coddy-studio")));
        assert_eq!(spec.static_dir, None);
        assert_eq!(spec.path_env, std::env::var_os("PATH"));
        assert!(json);
        assert!(
            !spec
                .serve_arguments()
                .iter()
                .any(|argument| argument == "--bootstrap-fd")
        );

        let action = Cli::try_parse_from(["coddy-studio", "service", "status"])
            .expect("parse service status")
            .into_action_with_executable(&executable)
            .expect("build status action");
        assert!(matches!(
            action,
            CliAction::Service(ServiceCommand::Status { json: false })
        ));

        let error = Cli::try_parse_from(["coddy-studio", "service", "uninstall"])
            .expect("parse service uninstall")
            .into_server_config()
            .expect_err("service commands are not server commands");
        assert!(matches!(error, ConfigError::ServiceCommandIsNotServer));
    }

    #[test]
    fn cli_parses_open_url_without_data_root() {
        // Control: a relative explicit data root cannot be resolved.
        let unusable_root = "relative-root";
        assert!(matches!(
            Cli::try_parse_from([
                "coddy-studio",
                "--base-dir",
                unusable_root,
                "storage",
                "inspect"
            ])
            .expect("parse storage inspect")
            .into_action(),
            Err(ConfigError::DataRoot(
                DataRootError::RelativeExplicit { .. }
            ))
        ));
        let action = Cli::try_parse_from([
            "coddy-studio",
            "--base-dir",
            unusable_root,
            "open-url",
            "http://localhost:5173/?key=1",
        ])
        .expect("parse open-url")
        .into_action()
        .expect("open-url never resolves a data root");
        assert!(matches!(
            action,
            CliAction::OpenUrl { url } if url == "http://localhost:5173/?key=1"
        ));
    }

    #[test]
    fn pairing_offer_resolves_the_data_root_and_defaults_reach_to_another_device() {
        let temp = tempfile::tempdir().expect("temporary base directory");
        let base_dir = temp.path().to_string_lossy().into_owned();

        let action = pairing_cli_without_dev_env(&[
            "coddy-studio",
            "pairing",
            "offer",
            "--base-dir",
            base_dir.as_str(),
            "--endpoint",
            "http://100.105.196.60:3773",
            "--label",
            "laptop",
            "--json",
        ])
        .into_action()
        .expect("build pairing action");
        let CliAction::Pairing(PairingCommand::Offer {
            root,
            dev_url,
            endpoint,
            reach,
            name,
            label,
            json,
        }) = action
        else {
            panic!("pairing offer must produce an offer action");
        };
        assert_eq!(root.requested, PathBuf::from(base_dir.as_str()));
        assert_eq!(dev_url, None);
        assert_eq!(endpoint, "http://100.105.196.60:3773");
        assert_eq!(reach, "another-device");
        assert_eq!(name, None);
        assert_eq!(label.as_deref(), Some("laptop"));
        assert!(json);

        assert!(
            Cli::try_parse_from([
                "coddy-studio",
                "pairing",
                "offer",
                "--base-dir",
                base_dir.as_str()
            ])
            .is_err(),
            "--endpoint is required"
        );
        assert!(
            Cli::try_parse_from([
                "coddy-studio",
                "pairing",
                "offer",
                "--endpoint",
                "http://10.0.0.5:3773",
                "--reach",
                "everywhere",
            ])
            .is_err(),
            "reach is an enumerated value"
        );
    }

    #[test]
    fn pairing_commands_preserve_dev_url_and_select_the_matching_state() {
        for subcommand in ["offer", "issue"] {
            for (dev_url, state) in [(None, "userdata"), (Some("http://localhost:5733"), "dev")] {
                let temp = tempfile::tempdir().expect("temporary pairing root");
                let base_dir = temp.path().to_string_lossy().into_owned();
                let mut args = vec![
                    "coddy-studio",
                    "pairing",
                    subcommand,
                    "--base-dir",
                    &base_dir,
                ];
                if subcommand == "offer" {
                    args.extend(["--endpoint", "http://100.105.196.60:3773"]);
                }
                if let Some(dev_url) = dev_url {
                    args.extend(["--dev-url", dev_url]);
                }
                let action = pairing_cli_without_dev_env(&args)
                    .into_action()
                    .expect("build pairing action");
                let (root, selected_dev_url) = match action {
                    CliAction::Pairing(PairingCommand::Offer { root, dev_url, .. })
                    | CliAction::Pairing(PairingCommand::Issue { root, dev_url, .. }) => {
                        (root, dev_url)
                    }
                    _ => panic!("pairing command must produce a pairing action"),
                };
                assert_eq!(
                    selected_dev_url,
                    dev_url.map(|value| value.parse::<Url>().expect("dev URL")),
                    "{subcommand} with {dev_url:?}"
                );
                let mut config = ServerConfig::new(&root.effective);
                config.dev_url = selected_dev_url;
                assert_eq!(config.state_dir(), root.effective.join(state));
            }
        }
    }

    fn pairing_cli_without_dev_env(args: &[&str]) -> Cli {
        let matches = Cli::command()
            .mut_arg("dev_url", |arg| arg.env(None::<&str>))
            .try_get_matches_from(args)
            .expect("parse pairing CLI without an ambient dev URL");
        Cli::from_arg_matches(&matches).expect("pairing CLI")
    }
}

#[derive(Debug, Parser)]
#[command(name = "coddy-studio", version, about = "Run the Coddy Studio server.")]
pub struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,

    #[command(flatten)]
    root: ServerArgs,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    #[command(about = "Run the Coddy Studio server without opening a browser.")]
    Serve,
    #[command(about = "Run the Coddy Studio server.")]
    Start,
    #[command(about = "Inspect or explicitly recover offline project data.")]
    Storage(StorageArgs),
    #[command(about = "Manage one-time pairing credentials for a data root.")]
    Pairing(PairingArgs),
    #[command(
        about = "Read profiler findings, events and crash records from disk, with or without a running server."
    )]
    Profiler(ProfilerArgs),
    #[command(
        about = "Install, remove, or inspect the per-user background service that keeps `coddy-studio serve` running."
    )]
    Service(ServiceArgs),
    #[command(
        name = "open-url",
        about = "Ask the Coddy Studio client showing this agent or terminal's thread to open an http(s) URL.",
        hide = true
    )]
    OpenUrl { url: String },
}

#[derive(Debug, Args)]
struct StorageArgs {
    #[command(subcommand)]
    command: StorageSubcommand,
}

#[derive(Debug, Subcommand)]
enum StorageSubcommand {
    #[command(about = "Inspect an offline Coddy Studio project-data store.")]
    Inspect {
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Restore a verified backup into an offline store.")]
    Restore {
        #[arg(long)]
        backup_id: uuid::Uuid,
        #[arg(long)]
        json: bool,
    },
    #[command(
        name = "start-empty",
        about = "Preserve an offline store and start empty."
    )]
    StartEmpty {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct PairingArgs {
    #[command(subcommand)]
    command: PairingSubcommand,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum PairingReachArg {
    AnotherDevice,
    ThisComputer,
    Custom,
}

impl PairingReachArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::AnotherDevice => "another-device",
            Self::ThisComputer => "this-computer",
            Self::Custom => "custom",
        }
    }
}

#[derive(Debug, Subcommand)]
enum PairingSubcommand {
    #[command(about = "Create a five-minute administrative pairing credential for this data root.")]
    Issue {
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        json: bool,
    },
    #[command(
        about = "Create a five-minute encrypted pairing offer (coddy-studio://pair?code=…) for another Coddy Studio client."
    )]
    Offer {
        /// The http(s) address the other device will connect to.
        #[arg(long)]
        endpoint: String,
        #[arg(long, value_enum, default_value_t = PairingReachArg::AnotherDevice)]
        reach: PairingReachArg,
        /// Display name shown on the other device; defaults to this machine's hostname.
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct ServiceArgs {
    #[command(subcommand)]
    command: ServiceSubcommand,
}

#[derive(Debug, Subcommand)]
enum ServiceSubcommand {
    #[command(
        about = "Install and start a per-user service running `coddy-studio serve` with the given --host, --port, --base-dir, and --static-dir."
    )]
    Install {
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Stop and remove the per-user service.")]
    Uninstall {
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Report whether the per-user service is installed and running.")]
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Debug)]
pub enum ServiceCommand {
    Install {
        spec: crate::service_manager::ServiceSpec,
        json: bool,
    },
    Uninstall {
        json: bool,
    },
    Status {
        json: bool,
    },
}

#[derive(Clone, Debug)]
pub enum CliAction {
    Run(Box<ServerConfig>),
    Storage(StorageCommand),
    Pairing(PairingCommand),
    Service(ServiceCommand),
    /// Run by the open-url shim; touches no data root.
    OpenUrl {
        url: String,
    },
    /// Reads `<state_dir>/profiler`; never starts a server.
    Profiler {
        args: ProfilerArgs,
        state_dir: PathBuf,
    },
}

#[derive(Clone, Debug)]
pub enum PairingCommand {
    Issue {
        root: ResolvedDataRoot,
        dev_url: Option<Url>,
        label: Option<String>,
        json: bool,
    },
    Offer {
        root: ResolvedDataRoot,
        dev_url: Option<Url>,
        endpoint: String,
        reach: String,
        name: Option<String>,
        label: Option<String>,
        json: bool,
    },
}

#[derive(Clone, Debug)]
pub enum StorageCommand {
    Inspect {
        root: ResolvedDataRoot,
        json: bool,
    },
    Restore {
        root: ResolvedDataRoot,
        backup_id: uuid::Uuid,
        json: bool,
    },
    StartEmpty {
        root: ResolvedDataRoot,
        json: bool,
    },
}

#[derive(Clone, Debug, Default, Args)]
struct ServerArgs {
    #[arg(long, value_enum, env = "CODDY_STUDIO_MODE", global = true)]
    mode: Option<ServerMode>,

    #[arg(long, env = "CODDY_STUDIO_HOST", global = true)]
    host: Option<String>,

    #[arg(long, env = "CODDY_STUDIO_PORT", global = true)]
    port: Option<u16>,

    #[arg(long, global = true)]
    base_dir: Option<PathBuf>,

    #[arg(long, global = true)]
    static_dir: Option<PathBuf>,

    #[arg(long, env = "VITE_DEV_SERVER_URL", global = true)]
    dev_url: Option<Url>,

    #[arg(long, env = "CODDY_STUDIO_NO_BROWSER", global = true)]
    no_browser: bool,

    /// Do not mint the five-minute pairing offer printed as `pairingCode` at startup.
    #[arg(long, env = "CODDY_STUDIO_NO_STARTUP_PAIRING_OFFER", global = true)]
    no_startup_pairing_offer: bool,

    #[arg(long, env = "CODDY_STUDIO_BOOTSTRAP_FD", global = true)]
    bootstrap_fd: Option<i32>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("bootstrap file descriptor {0} is unsupported on this platform")]
    UnsupportedBootstrapFd(i32),
    #[error("failed to read the desktop bootstrap envelope")]
    BootstrapRead(#[source] io::Error),
    #[error("the desktop bootstrap envelope was empty")]
    EmptyBootstrap,
    #[error("failed to decode the desktop bootstrap envelope")]
    BootstrapDecode(#[source] serde_json::Error),
    #[error("desktop bootstrap token must not be empty")]
    EmptyDesktopBootstrapToken,
    #[error("storage commands cannot be converted into a server configuration")]
    StorageCommandIsNotServer,
    #[error("pairing commands cannot be converted into a server configuration")]
    PairingCommandIsNotServer,
    #[error("service commands cannot be converted into a server configuration")]
    ServiceCommandIsNotServer,
    #[error("open-url cannot be converted into a server configuration")]
    OpenUrlCommandIsNotServer,
    #[error("profiler commands cannot be converted into a server configuration")]
    ProfilerCommandIsNotServer,
    #[error("failed to resolve the current executable for static web discovery")]
    CurrentExecutable(#[source] io::Error),
    #[error(transparent)]
    StaticAssets(#[from] StaticDirError),
    #[error(transparent)]
    DataRoot(#[from] DataRootError),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopBootstrap {
    mode: ServerModeWire,
    no_browser: bool,
    port: u16,
    #[serde(alias = "bibcodeHome")]
    coddy_studio_home: Option<PathBuf>,
    host: String,
    desktop_bootstrap_token: String,
    #[serde(default)]
    wsl_transport: bool,
    #[allow(dead_code)]
    tailscale_serve_enabled: bool,
    #[allow(dead_code)]
    tailscale_serve_port: u16,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ServerModeWire {
    Desktop,
}

impl Cli {
    pub fn into_server_config(self) -> Result<ServerConfig, ConfigError> {
        match self.into_action()? {
            CliAction::Run(config) => Ok(*config),
            CliAction::Storage(_) => Err(ConfigError::StorageCommandIsNotServer),
            CliAction::Pairing(_) => Err(ConfigError::PairingCommandIsNotServer),
            CliAction::Service(_) => Err(ConfigError::ServiceCommandIsNotServer),
            CliAction::OpenUrl { .. } => Err(ConfigError::OpenUrlCommandIsNotServer),
            CliAction::Profiler { .. } => Err(ConfigError::ProfilerCommandIsNotServer),
        }
    }

    pub fn into_action(self) -> Result<CliAction, ConfigError> {
        self.into_action_with_optional_executable(None)
    }

    #[cfg(test)]
    fn into_action_with_executable(self, executable: &Path) -> Result<CliAction, ConfigError> {
        self.into_action_with_optional_executable(Some(executable))
    }

    fn into_action_with_optional_executable(
        self,
        executable: Option<&Path>,
    ) -> Result<CliAction, ConfigError> {
        let Self {
            command,
            root: args,
        } = self;
        let command = match command {
            Some(CliCommand::OpenUrl { url }) => return Ok(CliAction::OpenUrl { url }),
            Some(CliCommand::Storage(storage)) => {
                let home_dir = dirs::home_dir().ok_or(DataRootError::HomeDirectoryUnavailable)?;
                let request = select_data_root_request(
                    args.base_dir,
                    coddy_studio_env_var("CODDY_STUDIO_HOME"),
                    None,
                    home_dir,
                );
                let root = crate::data_root::resolve_data_root(request)?;
                return Ok(CliAction::Storage(match storage.command {
                    StorageSubcommand::Inspect { json } => StorageCommand::Inspect { root, json },
                    StorageSubcommand::Restore { backup_id, json } => StorageCommand::Restore {
                        root,
                        backup_id,
                        json,
                    },
                    StorageSubcommand::StartEmpty { json } => {
                        StorageCommand::StartEmpty { root, json }
                    }
                }));
            }
            Some(CliCommand::Pairing(pairing)) => {
                let home_dir = dirs::home_dir().ok_or(DataRootError::HomeDirectoryUnavailable)?;
                let request = select_data_root_request(
                    args.base_dir,
                    coddy_studio_env_var("CODDY_STUDIO_HOME"),
                    None,
                    home_dir,
                );
                let root = crate::data_root::resolve_data_root(request)?;
                return Ok(CliAction::Pairing(match pairing.command {
                    PairingSubcommand::Issue { label, json } => PairingCommand::Issue {
                        root,
                        dev_url: args.dev_url,
                        label,
                        json,
                    },
                    PairingSubcommand::Offer {
                        endpoint,
                        reach,
                        name,
                        label,
                        json,
                    } => PairingCommand::Offer {
                        root,
                        dev_url: args.dev_url,
                        endpoint,
                        reach: reach.as_str().to_owned(),
                        name,
                        label,
                        json,
                    },
                }));
            }
            Some(CliCommand::Profiler(profiler)) => {
                return Ok(CliAction::Profiler {
                    args: profiler,
                    state_dir: profiler_state_dir(args.base_dir, args.dev_url)?,
                });
            }
            Some(CliCommand::Service(service)) => {
                return Ok(CliAction::Service(match service.command {
                    ServiceSubcommand::Install { json } => {
                        let current_executable = match executable {
                            Some(executable) => executable.to_path_buf(),
                            None => {
                                std::env::current_exe().map_err(ConfigError::CurrentExecutable)?
                            }
                        };
                        ServiceCommand::Install {
                            spec: crate::service_manager::ServiceSpec {
                                executable: current_executable,
                                host: args.host.unwrap_or_else(|| "127.0.0.1".to_owned()),
                                port: args.port.unwrap_or(DEFAULT_PORT),
                                base_dir: args.base_dir,
                                static_dir: args.static_dir,
                                path_env: std::env::var_os("PATH"),
                            },
                            json,
                        }
                    }
                    ServiceSubcommand::Uninstall { json } => ServiceCommand::Uninstall { json },
                    ServiceSubcommand::Status { json } => ServiceCommand::Status { json },
                }));
            }
            command => command,
        };
        let headless = matches!(command, Some(CliCommand::Serve));
        let bootstrap = args.bootstrap_fd.map(read_bootstrap).transpose()?.flatten();

        let mode = args
            .mode
            .or_else(|| {
                bootstrap.as_ref().map(|value| match value.mode {
                    ServerModeWire::Desktop => ServerMode::Desktop,
                })
            })
            .unwrap_or_default();
        let home_dir = dirs::home_dir().ok_or(DataRootError::HomeDirectoryUnavailable)?;
        let data_root_request = select_data_root_request(
            args.base_dir,
            studio_data_root_env(),
            bootstrap
                .as_ref()
                .and_then(|value| value.coddy_studio_home.clone()),
            home_dir,
        );
        let host = args
            .host
            .or_else(|| bootstrap.as_ref().map(|value| value.host.clone()))
            .unwrap_or_else(|| "127.0.0.1".to_owned());
        let port = args
            .port
            .or_else(|| bootstrap.as_ref().map(|value| value.port))
            .unwrap_or(DEFAULT_PORT);

        let mut config = ServerConfig::new(
            data_root_request
                .requested
                .clone()
                .unwrap_or_else(|| data_root_request.home_dir.join(".coddy/studio")),
        )
        .with_bind(host, port);
        config.data_root_request = data_root_request;
        config.mode = mode;
        // Only headless web servers name themselves after the machine. Browser
        // launches and desktop-owned backends remain this device's "Local".
        if headless && mode == ServerMode::Web {
            config.environment_label = headless_environment_label(machine_host_name());
        }
        let current_executable = match executable {
            Some(executable) => executable.to_path_buf(),
            None => std::env::current_exe().map_err(ConfigError::CurrentExecutable)?,
        };
        let resolved_static_dir =
            resolve_static_dir(args.static_dir.as_deref(), &current_executable)?;
        config.static_dir = resolved_static_dir
            .as_ref()
            .map(|resolved| resolved.path.clone());
        config.static_dir_source = resolved_static_dir.as_ref().map(|resolved| resolved.source);
        config.remote_update_support.install_kind = resolved_static_dir
            .as_ref()
            .map_or(RemoteUpdateInstallKind::Unknown, |resolved| {
                resolved.install_kind
            });
        config.dev_url = args.dev_url;
        config.no_browser = headless
            || args.no_browser
            || bootstrap.as_ref().is_some_and(|value| value.no_browser)
            || mode == ServerMode::Desktop;
        config.startup_pairing_offer = !args.no_startup_pairing_offer;
        let desktop_bootstrap_token = bootstrap
            .as_ref()
            .map(|value| value.desktop_bootstrap_token.clone());
        if desktop_bootstrap_token
            .as_deref()
            .is_some_and(|token| token.trim().is_empty())
        {
            return Err(ConfigError::EmptyDesktopBootstrapToken);
        }
        config.desktop_bootstrap_token = desktop_bootstrap_token;
        config.desktop_wsl_transport = bootstrap.as_ref().is_some_and(|value| value.wsl_transport);
        Ok(CliAction::Run(Box::new(config)))
    }
}

fn coddy_studio_env_var(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name)
}

/// The environment-provided data-root override. A Studio-specific name wins over
/// the legacy prefix, and `CODDY_HOME` selects its `studio` child so the rest of
/// the Coddy home keeps its own owners.
fn studio_data_root_env() -> Option<std::ffi::OsString> {
    for name in ["CODDY_STUDIO_HOME", "BIBCODE_HOME"] {
        if let Some(value) = coddy_studio_env_var(name) {
            return Some(value);
        }
    }
    coddy_studio_env_var("CODDY_HOME").map(|home| {
        let mut path = PathBuf::from(home);
        path.push("studio");
        path.into_os_string()
    })
}

/// The state directory a server started with `base_dir` and `dev_url` uses, resolved as every
/// command that reads a data root does. With neither, it is the desktop's: its in-process
/// server takes `CODDY_STUDIO_HOME` or `~/.coddy/studio` and never a dev URL.
pub(crate) fn profiler_state_dir(
    base_dir: Option<PathBuf>,
    dev_url: Option<Url>,
) -> Result<PathBuf, ConfigError> {
    let home_dir = dirs::home_dir().ok_or(DataRootError::HomeDirectoryUnavailable)?;
    let request = select_data_root_request(base_dir, studio_data_root_env(), None, home_dir);
    let root = crate::data_root::resolve_data_root(request)?;
    let mut config = ServerConfig::new(&root.effective);
    config.dev_url = dev_url;
    Ok(config.state_dir())
}

fn select_data_root_request(
    cli_base_dir: Option<PathBuf>,
    environment_base_dir: Option<std::ffi::OsString>,
    desktop_bootstrap_base_dir: Option<PathBuf>,
    home_dir: PathBuf,
) -> DataRootRequest {
    match (cli_base_dir, environment_base_dir) {
        (Some(path), _) => DataRootRequest::explicit(DataRootSource::Cli, path, home_dir),
        (None, Some(path)) => {
            DataRootRequest::explicit(DataRootSource::Environment, PathBuf::from(path), home_dir)
        }
        (None, None) => match desktop_bootstrap_base_dir {
            Some(path) => DataRootRequest::explicit(DataRootSource::Cli, path, home_dir),
            None => DataRootRequest::default(home_dir),
        },
    }
}

fn read_bootstrap(fd: i32) -> Result<Option<DesktopBootstrap>, ConfigError> {
    #[cfg(not(unix))]
    if fd != 0 {
        return Err(ConfigError::UnsupportedBootstrapFd(fd));
    }

    let mut line = String::new();
    let read = read_bootstrap_line(fd, &mut line).map_err(ConfigError::BootstrapRead)?;
    if read == 0 || line.trim().is_empty() {
        return Err(ConfigError::EmptyBootstrap);
    }
    let bootstrap = serde_json::from_str(&line).map_err(ConfigError::BootstrapDecode)?;
    Ok(Some(bootstrap))
}

#[cfg(unix)]
fn read_bootstrap_line(fd: i32, line: &mut String) -> Result<usize, io::Error> {
    if fd == 0 {
        return io::stdin().lock().read_line(line);
    }
    if fd < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "bootstrap file descriptor must be non-negative",
        ));
    }

    // SAFETY: the bootstrap fd is an inherited, one-shot descriptor whose
    // ownership is transferred to this process by the launcher contract.
    let file = unsafe { File::from_raw_fd(fd) };
    BufReader::new(file).read_line(line)
}

#[cfg(not(unix))]
fn read_bootstrap_line(fd: i32, line: &mut String) -> Result<usize, io::Error> {
    debug_assert_eq!(fd, 0);
    io::stdin().lock().read_line(line)
}
