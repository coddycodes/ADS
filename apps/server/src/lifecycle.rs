use std::{net::SocketAddr, sync::Arc, time::Duration};

use thiserror::Error;
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::{
    auth::{
        AuthService, SecretStore,
        pairing_code::{
            REMOTE_PAIRING_CODE_VERSION, RemotePairingCodePayload, RemotePairingReach,
            encode_pairing_code, pairing_deep_link,
        },
    },
    config::{ServerConfig, ServerMode},
    data_root::{DataRootError, ResolvedDataRoot, resolve_data_root},
    diagnostics::{
        DesktopUiProcessObserver, NotApplicableUiProcessObserver,
        UnavailableDesktopUiProcessObserver,
    },
    http, logging,
    maintenance::{
        UpdateMaintenance, maintenance_routes_enabled, update_maintenance_owner_enabled,
    },
    persistence::{
        Database, Repositories, StatePaths, StorageInstanceId, StoreRuntimeGuard, prepare_store,
    },
    production::http_routes::{HttpRouteError, HttpRoutesState},
    production::runtime::ProductionRuntime,
    production::{
        connect_mcp::{
            ConnectMcpConfig, ConnectMcpService, PairingCredential, PairingIssuer, PreviewInvoker,
        },
        jwt::PersistentJwtCodec,
        server_terminal::ProcessTreeCleanup,
    },
    remote_update::RemoteUpdateDelegate,
    rpc::{E2eePreauthAdmission, RpcRegistry},
};

const SIGNING_KEY_NAME: &str = "server-signing-key";
const SIGNING_KEY_BYTES: usize = 32;
const ASSET_KEY_NAME: &str = "asset-access-key";
const ASSET_KEY_BYTES: usize = 32;

pub struct ServerRuntime;

fn connect_environment_descriptor(config: &ServerConfig) -> serde_json::Value {
    serde_json::json!({
        "environmentId": config.environment_id,
        "label": config.environment_label,
        "platform": { "os": std::env::consts::OS, "arch": std::env::consts::ARCH },
        "serverVersion": config.server_version,
        "storageInstanceId": config
            .storage_instance_id
            .expect("a running server has a prepared persistent store")
            .to_string(),
        "bootId": config.boot_id.map(|id| id.to_string()),
        "remoteUpdateSupport": config.remote_update_support,
        "remoteProtocolVersion": crate::http::REMOTE_PROTOCOL_VERSION,
        "minCompatibleRemoteProtocol": crate::http::MIN_COMPATIBLE_REMOTE_PROTOCOL,
        "capabilities": {
            "repositoryIdentity": true,
            "remoteUpdateControl": true,
            "remoteUpdateProgress": true,
            "terminalOrderedInput": true,
            "terminalSizeOwnership": true,
            "terminalImagePaste": true,
            "pullRequestCreateOptions": true,
            "vcsCloneReattach": true,
            "attachmentStaging": true,
            "profiler": true,
        },
    })
}

pub struct ServerHandle {
    local_addr: SocketAddr,
    data_root: ResolvedDataRoot,
    startup_access: Option<StartupAccess>,
    database: Option<Database>,
    _store_runtime_guard: StoreRuntimeGuard,
    _production_runtime: Option<Arc<ProductionRuntime>>,
    update_maintenance: Option<Arc<UpdateMaintenance>>,
    _log_sink: Arc<logging::LogSinkLease>,
    shutdown: CancellationToken,
    task: Option<JoinHandle<Result<(), std::io::Error>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartupAccess {
    pub connection_string: String,
    pub credential: String,
    pub pairing_url: String,
    /// Full `bibcode://pair?code=…` link for the desktop Add Server dialog,
    /// present only when the bind address is routable from other devices.
    pub pairing_link: Option<String>,
}

#[derive(Debug, Error)]
pub enum ServerError {
    #[error(transparent)]
    DataRoot(#[from] DataRootError),
    #[error("failed to create the server base directory")]
    CreateBaseDirectory(#[source] std::io::Error),
    #[error("failed to initialize native server state files: {0}")]
    StateFiles(String),
    #[error("failed to initialize native server logging: {0}")]
    Logging(String),
    #[error("failed to bind the server listener on {address}: {source}")]
    Bind {
        address: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to initialize environment authentication: {0}")]
    AuthInitialize(String),
    #[error("failed to initialize SQLite persistence: {0}")]
    PersistenceInitialize(String),
    #[error("failed to initialize the native production runtime: {0}")]
    ProductionInitialize(String),
    #[error("the server task failed")]
    Serve(#[source] std::io::Error),
    #[error("the server task was cancelled unexpectedly")]
    Join(#[source] tokio::task::JoinError),
    #[error("the server task was already joined")]
    AlreadyJoined,
}

impl ServerRuntime {
    pub async fn start(config: ServerConfig) -> Result<ServerHandle, ServerError> {
        let ui_process_observer = default_ui_process_observer(config.mode);
        Self::start_internal(
            config,
            None,
            ui_process_observer,
            ProcessTreeCleanup::EmbeddedHost,
            None,
        )
        .await
    }

    pub(crate) async fn start_standalone(
        config: ServerConfig,
    ) -> Result<ServerHandle, ServerError> {
        let ui_process_observer = default_ui_process_observer(config.mode);
        Self::start_internal(
            config,
            None,
            ui_process_observer,
            ProcessTreeCleanup::StandaloneServer,
            None,
        )
        .await
    }

    pub async fn start_with_ui_process_observer(
        config: ServerConfig,
        ui_process_observer: Arc<dyn DesktopUiProcessObserver>,
    ) -> Result<ServerHandle, ServerError> {
        Self::start_internal(
            config,
            None,
            ui_process_observer,
            ProcessTreeCleanup::EmbeddedHost,
            None,
        )
        .await
    }

    pub async fn start_with_desktop_integration(
        config: ServerConfig,
        ui_process_observer: Arc<dyn DesktopUiProcessObserver>,
        update_delegate: Arc<dyn RemoteUpdateDelegate>,
    ) -> Result<ServerHandle, ServerError> {
        Self::start_internal(
            config,
            None,
            ui_process_observer,
            ProcessTreeCleanup::EmbeddedHost,
            Some(update_delegate),
        )
        .await
    }

    pub async fn start_with_registry(
        config: ServerConfig,
        rpc_registry: RpcRegistry,
    ) -> Result<ServerHandle, ServerError> {
        let ui_process_observer = default_ui_process_observer(config.mode);
        Self::start_internal(
            config,
            Some(rpc_registry),
            ui_process_observer,
            ProcessTreeCleanup::EmbeddedHost,
            None,
        )
        .await
    }

    async fn start_internal(
        mut config: ServerConfig,
        custom_registry: Option<RpcRegistry>,
        ui_process_observer: Arc<dyn DesktopUiProcessObserver>,
        process_tree_cleanup: ProcessTreeCleanup,
        update_delegate: Option<Arc<dyn RemoteUpdateDelegate>>,
    ) -> Result<ServerHandle, ServerError> {
        let resolved_data_root = resolve_data_root(config.data_root_request.clone())?;
        config.base_dir = resolved_data_root.effective.clone();
        config.resolved_data_root = Some(resolved_data_root.clone());
        tokio::fs::create_dir_all(&config.base_dir)
            .await
            .map_err(ServerError::CreateBaseDirectory)?;
        let state_paths = StatePaths::from_config(&config);
        state_paths
            .ensure_directories_without_database_side_effects()
            .await
            .map_err(|error| ServerError::StateFiles(error.to_string()))?;
        let log_sink = Arc::new(
            logging::initialize_owned(&state_paths.server_log)
                .map_err(|error| ServerError::Logging(error.to_string()))?,
        );
        if let (Some(static_dir), Some(source)) = (&config.static_dir, config.static_dir_source) {
            tracing::info!(
                static_dir = %static_dir.display(),
                source = source.as_str(),
                "selected static web assets"
            );
        }
        let store_runtime_guard = StoreRuntimeGuard::acquire(&config.base_dir)
            .await
            .map_err(|error| ServerError::PersistenceInitialize(error.to_string()))?;
        // Only the runtime-guard owner repoints the shim; a second server for this data root
        // fails the guard first. Without the shim, `$BROWSER` in agents and terminals fails to
        // resolve; the server itself is unaffected, so this never stops startup.
        if let Err(error) = open_url_shim_target()
            .and_then(|exe| crate::open_url::write_shims(&state_paths.open_url_shim_dir, &exe))
        {
            tracing::warn!(%error, "could not write the open-url shim");
        }
        let prepared_store = prepare_store(&config)
            .await
            .map_err(|error| ServerError::PersistenceInitialize(error.to_string()))?;
        config.storage_instance_id = Some(prepared_store.storage_instance_id);
        config.boot_id = Some(uuid::Uuid::new_v4());
        let storage_instance_id = prepared_store.storage_instance_id;
        let store_classification = prepared_store.classification;
        let database = prepared_store.database;
        let listener = bind_listener(&config.host, config.port, config.listener_bind_retry).await?;
        let local_addr = listener.local_addr().map_err(|source| ServerError::Bind {
            address: format!("{}:{}", config.host, config.port),
            source,
        })?;
        let state_directory = config.base_dir.join(if config.dev_url.is_some() {
            "dev"
        } else {
            "userdata"
        });
        let secret_store = SecretStore::new(state_directory.join("secrets"))
            .await
            .map_err(|error| ServerError::AuthInitialize(error.to_string()))?;
        let signing_secret = secret_store
            .get_or_create_random(SIGNING_KEY_NAME, SIGNING_KEY_BYTES)
            .await
            .map_err(|error| ServerError::AuthInitialize(error.to_string()))?;
        let asset_secret = secret_store
            .get_or_create_random(ASSET_KEY_NAME, ASSET_KEY_BYTES)
            .await
            .map_err(|error| ServerError::AuthInitialize(error.to_string()))?;
        let auth = AuthService::new_with_persistence(
            &config,
            signing_secret,
            secret_store,
            Repositories::new(database.clone()),
        )
        .await
        .map_err(|error| ServerError::AuthInitialize(format!("{error:?}")))?;
        let startup_access = if config.mode == crate::config::ServerMode::Web
            && !config.unsafe_no_auth
        {
            let issued = auth
                .issue_startup_pairing()
                .await
                .map_err(|error| ServerError::AuthInitialize(format!("{error:?}")))?;
            let mut access = build_startup_access(local_addr, issued.credential)?;
            if config.startup_pairing_offer
                && let Some(endpoint) = startup_offer_endpoint(local_addr)
            {
                access.pairing_link =
                    Some(mint_startup_pairing_link(&auth, storage_instance_id, endpoint).await?);
            }
            Some(access)
        } else {
            None
        };
        let (rpc_registry, http_routes, production_runtime) = match custom_registry {
            Some(mut registry) => {
                crate::auth::register_rpc_handlers(&mut registry, auth.clone());
                (registry, fallback_http_routes(auth.clone()), None)
            }
            None => {
                let runtime = Arc::new(
                    ProductionRuntime::start_with_process_tree_cleanup(
                        &config,
                        database.clone(),
                        auth.clone(),
                        asset_secret,
                        ui_process_observer,
                        process_tree_cleanup,
                        update_delegate,
                    )
                    .await
                    .map_err(ServerError::ProductionInitialize)?,
                );
                let jwt = PersistentJwtCodec::open(state_directory.join("environment-jwt.json"))
                    .await
                    .map_err(|error| ServerError::ProductionInitialize(error.to_string()))?;
                let endpoint = runtime.managed_endpoint_runtime();
                let pairing_auth = auth.clone();
                let pairing = PairingIssuer::new(move |thumbprint| {
                    let auth = pairing_auth.clone();
                    async move {
                        auth.issue_cloud_pairing(thumbprint)
                            .await
                            .map(|issued| PairingCredential {
                                credential: issued.credential,
                                expires_at: issued.expires_at,
                            })
                            .map_err(|error| format!("{error:?}"))
                    }
                });
                let automation = runtime.preview_automation.clone();
                let preview = PreviewInvoker::new(
                    move |scope, operation, input, tab_id, cancellation| {
                        let automation = automation.clone();
                        async move {
                            let operation = crate::mcp::preview_automation::PreviewAutomationOperation::from_wire(&operation)
                                .ok_or_else(|| format!("unsupported preview operation: {operation}"))?;
                            automation
                                .invoke(
                                    crate::mcp::preview_automation::PreviewAutomationInvokeInput {
                                        environment_id: scope.environment_id,
                                        thread_id: scope.thread_id,
                                        provider_session_id: scope.provider_session_id,
                                        provider_instance_id: scope.provider_instance_id,
                                        operation,
                                        input,
                                        tab_id,
                                        timeout_ms: None,
                                    },
                                )
                                .await
                                .map_err(|error| format!("{}: {}", error.tag(), error.message()))
                                .and_then(|value| {
                                    if cancellation.is_cancelled() {
                                        Err("preview automation request was cancelled".to_owned())
                                    } else {
                                        Ok(value)
                                    }
                                })
                        }
                    },
                );
                let descriptor = connect_environment_descriptor(&config);
                let connect = Arc::new(
                    ConnectMcpService::open(
                        config.database_path(),
                        ConnectMcpConfig {
                            environment_id: config.environment_id.clone(),
                            descriptor,
                            mcp_endpoint: format!("http://{local_addr}/mcp"),
                            open_url_endpoint: format!(
                                "{}/api/preview/open-url",
                                local_http_base(local_addr)
                            ),
                            now_epoch_seconds: Arc::new(|| {
                                time::OffsetDateTime::now_utc().unix_timestamp()
                            }),
                            max_mcp_credentials: 1_024,
                            max_mcp_sessions: 1_024,
                        },
                        jwt.jwt_codec(),
                        endpoint.endpoint(),
                        pairing,
                        preview,
                    )
                    .await
                    .map_err(|error| ServerError::ProductionInitialize(format!("{error:?}")))?,
                );
                runtime.attach_connect_mcp(connect.clone()).await;
                // The tunnel starts only on a link; a restart brings it back from the stored one.
                tokio::spawn({
                    let connect = connect.clone();
                    async move {
                        match connect.restore_managed_endpoint().await {
                            Ok(Some(status)) if status["status"] != "running" => tracing::warn!(
                                status = %status["status"],
                                "BiBCode Connect tunnel was not restored"
                            ),
                            Ok(_) => {}
                            Err(error) => tracing::warn!(
                                status = %error.status(),
                                "BiBCode Connect tunnel could not be restored"
                            ),
                        }
                    }
                });
                (
                    runtime.registry.clone(),
                    core_http_routes(auth.clone(), runtime.clone(), connect),
                    Some(runtime),
                )
            }
        };
        let shutdown = CancellationToken::new();
        let admission_gate = rpc_registry.admission_gate();
        let update_maintenance_owner = if update_maintenance_owner_enabled(&config) {
            production_runtime.as_ref().map(|runtime| {
                UpdateMaintenance::new(
                    admission_gate.clone(),
                    runtime.clone(),
                    database.clone(),
                    state_paths.clone(),
                    storage_instance_id,
                    store_classification,
                    config.server_version.clone(),
                    shutdown.clone(),
                    config.update_maintenance_drain_timeout,
                    config.update_maintenance_lease,
                )
            })
        } else {
            None
        };
        // HTTP gets the owner only on a loopback or desktop-owned WSL bind.
        let update_maintenance = if maintenance_routes_enabled(&config) {
            update_maintenance_owner.clone()
        } else {
            None
        };
        let preview_gateway = production_runtime
            .as_ref()
            .map(|runtime| runtime.preview_gateway());
        // The main port serves BiBCode Connect previews, and its graceful shutdown waits for
        // their exchanges, which only the gateway's drain ends: start the drain as soon as
        // shutdown is signaled, not after the server stops; the runtime's later call waits
        // for this same drain.
        if let Some(gateway) = preview_gateway.clone() {
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                shutdown.cancelled().await;
                gateway.shutdown().await;
            });
        }
        let app = http::build_router(http::AppState {
            config: Arc::new(config),
            shutdown: shutdown.clone(),
            rpc_registry,
            e2ee_preauth_admission: E2eePreauthAdmission::new(),
            http_routes,
            auth,
            admission_gate,
            update_maintenance,
            preview_gateway,
        });
        let server_shutdown = shutdown.clone();
        let completion_signal = shutdown.clone();
        let cleanup_runtime = production_runtime.clone();
        let task_log_sink = log_sink.clone();
        let task = tokio::spawn(async move {
            let _log_sink = task_log_sink;
            let result = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<http::ConnectionAddrs>(),
            )
            .with_graceful_shutdown(server_shutdown.cancelled_owned())
            .await;
            if let Some(runtime) = cleanup_runtime {
                runtime.shutdown().await;
            }
            completion_signal.cancel();
            result
        });

        Ok(ServerHandle {
            local_addr,
            data_root: resolved_data_root,
            startup_access,
            database: Some(database),
            _store_runtime_guard: store_runtime_guard,
            _production_runtime: production_runtime,
            update_maintenance: update_maintenance_owner,
            _log_sink: log_sink,
            shutdown,
            task: Some(task),
        })
    }
}

/// First pause before retrying a listener bind whose port is still in use.
const LISTENER_BIND_RETRY_INITIAL_BACKOFF: Duration = Duration::from_millis(25);
/// The pause doubles up to this cap.
const LISTENER_BIND_RETRY_MAX_BACKOFF: Duration = Duration::from_millis(250);

/// Binds the server listener on `host:port`.
///
/// With a `retry` window, a bind that fails because the port is still in use
/// is retried with backoff until the window ends, so a server restarted on
/// the port its predecessor has only just released does not fail on a socket
/// that is still closing. Any other failure, or a port still in use when the
/// window ends, returns that last bind error. Without a window the first
/// failure is final.
async fn bind_listener(
    host: &str,
    port: u16,
    retry: Option<Duration>,
) -> Result<TcpListener, ServerError> {
    let address = format!("{host}:{port}");
    let started = tokio::time::Instant::now();
    let mut backoff = LISTENER_BIND_RETRY_INITIAL_BACKOFF;
    let mut attempts: u32 = 0;
    loop {
        attempts = attempts.saturating_add(1);
        let source = match TcpListener::bind((host, port)).await {
            Ok(listener) => {
                if attempts > 1 {
                    tracing::info!(
                        %address,
                        attempts,
                        elapsed = ?started.elapsed(),
                        "bound the server listener once its port was released"
                    );
                }
                return Ok(listener);
            }
            Err(source) => source,
        };
        let Some(window) = retry.filter(|_| source.kind() == std::io::ErrorKind::AddrInUse) else {
            return Err(ServerError::Bind { address, source });
        };
        let elapsed = started.elapsed();
        let Some(remaining) = window.checked_sub(elapsed).filter(|left| !left.is_zero()) else {
            tracing::warn!(
                %address,
                attempts,
                ?elapsed,
                ?window,
                "the server listener port stayed in use for the whole bind retry window"
            );
            return Err(ServerError::Bind { address, source });
        };
        tracing::debug!(
            %address,
            attempt = attempts,
            ?elapsed,
            "the server listener port is still in use; retrying the bind"
        );
        tokio::time::sleep(backoff.min(remaining)).await;
        backoff = backoff
            .saturating_mul(2)
            .min(LISTENER_BIND_RETRY_MAX_BACKOFF);
    }
}

fn core_http_routes(
    auth: AuthService,
    runtime: Arc<ProductionRuntime>,
    connect: Arc<ConnectMcpService>,
) -> HttpRoutesState {
    let authorize = authorize_handler(auth);
    let json_runtime = runtime.clone();
    let json_connect = connect.clone();
    let json = Arc::new(move |operation, payload, context| {
        let runtime = json_runtime.clone();
        let connect = json_connect.clone();
        Box::pin(async move {
            match operation {
                crate::production::http_routes::JsonOperation::ConnectLinkProof
                | crate::production::http_routes::JsonOperation::ConnectRelayConfig
                | crate::production::http_routes::JsonOperation::ConnectLinkState
                | crate::production::http_routes::JsonOperation::ConnectUnlink
                | crate::production::http_routes::JsonOperation::ConnectHealth
                | crate::production::http_routes::JsonOperation::ConnectMintCredential => {
                    connect.json_http(operation, payload, context).await
                }
                _ => runtime.json(operation, payload, context).await,
            }
        }) as crate::production::http_routes::BoxFuture<_>
    });
    let diagnostic_runtime = runtime.clone();
    let diagnostic_logs = Arc::new(move |frontend_log, include_crash_dumps, _context| {
        let runtime = diagnostic_runtime.clone();
        Box::pin(async move {
            runtime
                .diagnostic_logs(frontend_log, include_crash_dumps)
                .await
        }) as crate::production::http_routes::BoxFuture<_>
    });
    let transfer_download = runtime.transfer_download_handler();
    let transfer_upload = runtime.transfer_upload_handler();
    let preview = runtime.preview.clone();
    let asset_runtime = runtime;
    let assets = Arc::new(move |token, path, _context| {
        let runtime = asset_runtime.clone();
        Box::pin(async move { runtime.asset(token, path).await })
            as crate::production::http_routes::BoxFuture<_>
    });
    let open_url_connect = connect.clone();
    let mcp = Arc::new(move |method, body, context| {
        let connect = connect.clone();
        Box::pin(async move { connect.mcp_http(method, body, context).await })
            as crate::production::http_routes::BoxFuture<_>
    });
    let mut routes = HttpRoutesState::new(
        authorize,
        json,
        diagnostic_logs,
        assets,
        mcp,
        transfer_download,
        transfer_upload,
    );
    routes.open_url = Arc::new(move |body, context| {
        let connect = open_url_connect.clone();
        let preview = preview.clone();
        Box::pin(async move { connect.open_url_http(&preview, body, context).await })
            as crate::production::http_routes::BoxFuture<_>
    });
    routes
}

fn default_ui_process_observer(mode: ServerMode) -> Arc<dyn DesktopUiProcessObserver> {
    match mode {
        ServerMode::Web => Arc::new(NotApplicableUiProcessObserver),
        ServerMode::Desktop => Arc::new(UnavailableDesktopUiProcessObserver),
    }
}

fn authorize_handler(auth: AuthService) -> crate::production::http_routes::AuthorizeHandler {
    Arc::new(move |headers, method, uri, scope, _cancellation| {
        let auth = auth.clone();
        Box::pin(async move {
            crate::auth::authorize_http_request(&auth, &headers, &method, &uri, scope)
                .await
                .map(|principal| crate::production::http_routes::Authorized {
                    scopes: principal.scopes,
                })
                .map_err(crate::auth::auth_error_response)
        }) as crate::production::http_routes::BoxFuture<_>
    })
}

fn fallback_http_routes(auth: AuthService) -> HttpRoutesState {
    let authorize = authorize_handler(auth);
    let json = Arc::new(move |_operation, _payload, _context| {
        Box::pin(async move {
            Err(HttpRouteError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                serde_json::json!({
                    "_tag": "NativeRuntimeUnavailableError",
                    "message": "The native production runtime is unavailable."
                }),
            ))
        }) as crate::production::http_routes::BoxFuture<_>
    });
    let assets = Arc::new(move |_token, _path, _context| {
        Box::pin(async move {
            Err(HttpRouteError::new(
                axum::http::StatusCode::NOT_FOUND,
                serde_json::json!({ "_tag": "AssetNotFoundError" }),
            ))
        }) as crate::production::http_routes::BoxFuture<_>
    });
    let diagnostic_logs = Arc::new(move |_frontend_log, _include_crash_dumps, _context| {
        Box::pin(async move {
            Err(HttpRouteError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                serde_json::json!({
                    "_tag": "NativeRuntimeUnavailableError",
                    "message": "The native production runtime is unavailable."
                }),
            ))
        }) as crate::production::http_routes::BoxFuture<_>
    });
    let mcp = Arc::new(move |_method, _body, _context| {
        Box::pin(async move {
            Err(HttpRouteError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                serde_json::json!({ "_tag": "McpUnavailableError" }),
            ))
        }) as crate::production::http_routes::BoxFuture<_>
    });
    let transfer_download = Arc::new(move |_token, _context| {
        Box::pin(async move { Err(transfer_unavailable()) })
            as crate::production::http_routes::BoxFuture<_>
    });
    let transfer_upload = Arc::new(move |_token, _name, _overwrite, _body, _context| {
        Box::pin(async move { Err(transfer_unavailable()) })
            as crate::production::http_routes::BoxFuture<_>
    });
    HttpRoutesState::new(
        authorize,
        json,
        diagnostic_logs,
        assets,
        mcp,
        transfer_download,
        transfer_upload,
    )
}

fn transfer_unavailable() -> HttpRouteError {
    HttpRouteError::new(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        serde_json::json!({
            "_tag": "NativeRuntimeUnavailableError",
            "message": "The native production runtime is unavailable."
        }),
    )
}

impl ServerHandle {
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    #[must_use]
    pub fn data_root(&self) -> &ResolvedDataRoot {
        &self.data_root
    }

    #[must_use]
    pub fn startup_access(&self) -> Option<&StartupAccess> {
        self.startup_access.as_ref()
    }

    /// The runtime's update-maintenance owner, for the desktop host's in-process
    /// protection. `Some` for every desktop-mode production runtime with a bootstrap
    /// token, whatever its bind; the HTTP routes may still be hidden.
    #[must_use]
    pub fn update_maintenance(&self) -> Option<Arc<UpdateMaintenance>> {
        self.update_maintenance.clone()
    }

    /// The production runtime's committed `enableProfiler`, which a desktop host follows to
    /// run the crash monitor for itself and this in-process server. `None` without one.
    #[must_use]
    pub fn profiler_enabled_signal(&self) -> Option<tokio::sync::watch::Receiver<bool>> {
        self._production_runtime
            .as_ref()
            .map(|runtime| runtime.profiler_enabled_signal())
    }

    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    pub async fn wait_for_shutdown(&self) {
        self.shutdown.cancelled().await;
    }

    pub async fn join(mut self) -> Result<(), ServerError> {
        let task = self.task.take().ok_or(ServerError::AlreadyJoined)?;
        let result = match task.await {
            Ok(result) => result.map_err(ServerError::Serve),
            Err(error) => Err(ServerError::Join(error)),
        };
        drop(self.update_maintenance.take());
        drop(self._production_runtime.take());
        if let Some(database) = self.database.take() {
            database.close().await;
        }
        result
    }
}

fn build_startup_access(
    local_addr: SocketAddr,
    credential: String,
) -> Result<StartupAccess, ServerError> {
    let host = if local_addr.ip().is_unspecified() {
        "localhost".to_owned()
    } else {
        local_addr.ip().to_string()
    };
    let authority = if local_addr.is_ipv6() && !local_addr.ip().is_unspecified() {
        format!("[{host}]:{}", local_addr.port())
    } else {
        format!("{host}:{}", local_addr.port())
    };
    let connection_string = format!("http://{authority}");
    let mut pairing_url = url::Url::parse(&connection_string)
        .map_err(|error| ServerError::AuthInitialize(error.to_string()))?;
    pairing_url.set_path("/pair");
    pairing_url.set_query(None);
    let fragment = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("token", &credential)
        .finish();
    pairing_url.set_fragment(Some(&fragment));
    Ok(StartupAccess {
        connection_string,
        credential,
        pairing_url: pairing_url.to_string(),
        pairing_link: None,
    })
}

/// What the open-url shim runs. An AppImage's mounted executable depends on the environment
/// AppRun sets, which agents and terminals do not inherit, so the image itself is the entry.
fn open_url_shim_target() -> std::io::Result<std::path::PathBuf> {
    let exe = std::env::current_exe()?;
    Ok(appimage_entry(
        &exe,
        std::env::var_os("APPIMAGE"),
        std::env::var_os("APPDIR"),
    )
    .unwrap_or(exe))
}

/// The AppImage that `exe` runs from. `APPIMAGE` alone may be inherited from an unrelated
/// AppImage, so `exe` must lie inside the mounted `APPDIR` too.
fn appimage_entry(
    exe: &std::path::Path,
    appimage: Option<std::ffi::OsString>,
    appdir: Option<std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let image = std::path::PathBuf::from(appimage?);
    let appdir = std::path::PathBuf::from(appdir?);
    (image.is_absolute() && appdir.is_absolute() && exe.starts_with(&appdir) && image.is_file())
        .then_some(image)
}

/// How a process on this host reaches the server; a wildcard bind answers on loopback.
fn local_http_base(local_addr: SocketAddr) -> String {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    let ip = match local_addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    format!("http://{}", SocketAddr::new(ip, local_addr.port()))
}

/// Endpoint other devices can reach, derived from the bound socket address.
/// Loopback and unspecified binds have no usable advertised endpoint.
pub(crate) fn startup_offer_endpoint(local_addr: SocketAddr) -> Option<String> {
    let ip = local_addr.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        return None;
    }
    Some(format!("http://{local_addr}"))
}

/// Mints one share-shaped off-host offer through the live auth service. This
/// is a second grant beside the startup token: the startup token is an
/// administrative bootstrap without reach and must never be embedded in a code.
async fn mint_startup_pairing_link(
    auth: &AuthService,
    storage_instance_id: StorageInstanceId,
    endpoint: String,
) -> Result<String, ServerError> {
    let name = crate::default_pairing_offer_name();
    let issued = auth
        .issue_share_pairing(
            crate::auth::default_standard_scopes(),
            Some(name.clone()),
            "another-device".to_owned(),
            true,
        )
        .await
        .map_err(|error| ServerError::AuthInitialize(format!("{error:?}")))?;
    let payload = RemotePairingCodePayload {
        v: REMOTE_PAIRING_CODE_VERSION,
        endpoint,
        name,
        token: issued.credential,
        host_key: auth.host_identity().public_key_base64url(),
        reach: RemotePairingReach::AnotherDevice,
        storage_instance_id: storage_instance_id.to_string(),
    };
    let code = encode_pairing_code(&payload)
        .map_err(|error| ServerError::AuthInitialize(error.to_string()))?;
    Ok(pairing_deep_link(&code))
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.shutdown.cancel();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::test_support::hermetic_providers;

    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn open_url_shim_runs_the_appimage_only_from_inside_it() {
        let temp = tempfile::tempdir().unwrap();
        let image = temp.path().join("BiBCode.AppImage");
        std::fs::write(&image, b"image").unwrap();
        let appdir = temp.path().join(".mount_BiBCod");
        let inside = appdir.join("usr/bin/bibcode-desktop");
        let os = |path: &std::path::Path| Some(path.as_os_str().to_owned());

        assert_eq!(
            appimage_entry(&inside, os(&image), os(&appdir)),
            Some(image.clone())
        );
        // A standalone server that inherited another AppImage's variables keeps its own exe.
        assert_eq!(
            appimage_entry(
                std::path::Path::new("/usr/bin/bibcode"),
                os(&image),
                os(&appdir)
            ),
            None
        );
        assert_eq!(appimage_entry(&inside, os(&image), None), None);
        assert_eq!(
            appimage_entry(&inside, os(&temp.path().join("missing")), os(&appdir)),
            None
        );
    }

    #[test]
    fn local_http_base_reaches_wildcard_binds_on_loopback() {
        use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
        for (ip, expected) in [
            (IpAddr::V4(Ipv4Addr::UNSPECIFIED), "http://127.0.0.1:3773"),
            (IpAddr::V6(Ipv6Addr::UNSPECIFIED), "http://[::1]:3773"),
            (
                IpAddr::V4(Ipv4Addr::new(100, 64, 0, 10)),
                "http://100.64.0.10:3773",
            ),
            (IpAddr::V6(Ipv6Addr::LOCALHOST), "http://[::1]:3773"),
        ] {
            assert_eq!(local_http_base(SocketAddr::new(ip, 3773)), expected);
        }
    }

    #[test]
    fn startup_offer_endpoint_requires_a_routable_bind() {
        use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
        assert_eq!(
            startup_offer_endpoint(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3773)),
            None
        );
        assert_eq!(
            startup_offer_endpoint(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 3773)),
            None
        );
        assert_eq!(
            startup_offer_endpoint(SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 3773)),
            None
        );
        assert_eq!(
            startup_offer_endpoint(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(100, 105, 196, 60)),
                3773
            )),
            Some("http://100.105.196.60:3773".to_owned())
        );
        assert_eq!(
            startup_offer_endpoint(SocketAddr::new(
                IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 5)),
                3773
            )),
            Some("http://[fd00::5]:3773".to_owned())
        );
    }

    /// Holds a loopback port the way a predecessor's listener would.
    fn hold_loopback_port() -> (std::net::TcpListener, u16) {
        let holder = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .expect("a loopback port should bind");
        let port = holder.local_addr().expect("held port address").port();
        (holder, port)
    }

    fn release_after(
        holder: std::net::TcpListener,
        delay: Duration,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            drop(holder);
        })
    }

    fn assert_address_in_use(error: &ServerError, port: u16) {
        match error {
            ServerError::Bind { address, source } => {
                assert_eq!(address, &format!("127.0.0.1:{port}"));
                assert_eq!(source.kind(), std::io::ErrorKind::AddrInUse);
            }
            other => panic!("expected an address-in-use Bind error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_bind_retry_window_outlasts_a_port_released_within_it() {
        let (holder, port) = hold_loopback_port();
        let release = release_after(holder, Duration::from_millis(200));

        let listener = bind_listener("127.0.0.1", port, Some(Duration::from_secs(3)))
            .await
            .expect("the bind should succeed once the port is released");

        assert_eq!(listener.local_addr().expect("bound address").port(), port);
        release.join().expect("the holder thread should finish");
    }

    #[tokio::test]
    async fn a_bind_retry_window_ends_with_the_bind_error_while_the_port_stays_held() {
        let (_holder, port) = hold_loopback_port();
        let window = Duration::from_millis(300);
        let started = std::time::Instant::now();

        let error = bind_listener("127.0.0.1", port, Some(window))
            .await
            .expect_err("a port held throughout the window must fail the bind");

        let elapsed = started.elapsed();
        assert!(
            elapsed >= window,
            "the bind gave up after {elapsed:?}, before its {window:?} window ended"
        );
        assert_address_in_use(&error, port);
    }

    #[tokio::test]
    async fn without_a_bind_retry_window_the_first_failure_is_final() {
        let (holder, port) = hold_loopback_port();
        // The port frees half a second in; a bind that retried would succeed.
        let release = release_after(holder, Duration::from_millis(500));

        let error = bind_listener("127.0.0.1", port, None)
            .await
            .expect_err("without a window a held port must fail the bind at once");

        assert!(
            !release.is_finished(),
            "the bind should fail while the port is still held"
        );
        assert_address_in_use(&error, port);
        release.join().expect("the holder thread should finish");
    }

    #[tokio::test]
    async fn server_start_applies_the_configured_bind_retry_window() {
        let (_holder, port) = hold_loopback_port();
        let temp = tempfile::tempdir().expect("temporary base directory");
        let window = Duration::from_secs(1);
        let started = std::time::Instant::now();

        let error = match ServerRuntime::start_with_registry(
            ServerConfig::new(temp.path())
                .with_bind("127.0.0.1", port)
                .with_listener_bind_retry(window),
            RpcRegistry::empty(),
        )
        .await
        {
            Ok(handle) => {
                drop(handle);
                panic!("a port held throughout the window must fail startup");
            }
            Err(error) => error,
        };

        let elapsed = started.elapsed();
        assert!(
            elapsed >= window,
            "startup gave up after {elapsed:?}, before its {window:?} bind window ended"
        );
        assert_address_in_use(&error, port);
    }

    #[tokio::test]
    async fn loopback_serve_has_no_startup_pairing_link() {
        let temp = tempfile::tempdir().expect("temporary base directory");
        let config = ServerConfig::new(temp.path()).with_bind("127.0.0.1", 0);
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let handle = ServerRuntime::start(config).await.expect("server starts");
        let access = handle.startup_access().expect("web mode startup access");
        assert_eq!(access.pairing_link, None);
        handle.shutdown();
        handle.join().await.expect("server joins");
    }

    /// Uses the host's outbound interface address (no packets are sent by a
    /// connected UDP socket). Skips on hosts with no routable address.
    #[tokio::test]
    async fn routable_serve_prints_a_share_shaped_startup_offer() {
        let probe = std::net::UdpSocket::bind("0.0.0.0:0").expect("udp probe");
        let Ok(()) = probe.connect("192.0.2.1:9") else {
            return;
        };
        let ip = probe.local_addr().expect("probe address").ip();
        if ip.is_loopback() || ip.is_unspecified() {
            return;
        }
        let temp = tempfile::tempdir().expect("temporary base directory");
        let config = ServerConfig::new(temp.path()).with_bind(ip.to_string(), 0);
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let handle = ServerRuntime::start(config)
            .await
            .expect("server starts on the routable address");
        let access = handle.startup_access().expect("web mode startup access");
        let link = access
            .pairing_link
            .as_deref()
            .expect("routable bind mints a startup offer");
        let code = link
            .strip_prefix("bibcode://pair?code=")
            .expect("deep link shape");
        let payload = crate::auth::pairing_code::decode_pairing_code(code).expect("decodes");
        assert_eq!(payload.endpoint, format!("http://{}", handle.local_addr()));
        assert_eq!(
            payload.reach,
            crate::auth::pairing_code::RemotePairingReach::AnotherDevice
        );
        assert_ne!(
            payload.token, access.credential,
            "the startup token is never embedded"
        );

        let second = tempfile::tempdir().expect("second root");
        let disabled = {
            let mut config = ServerConfig::new(second.path()).with_bind(ip.to_string(), 0);
            config.startup_pairing_offer = false;
            hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
            ServerRuntime::start(config)
                .await
                .expect("server starts without an offer")
        };
        assert_eq!(
            disabled.startup_access().expect("access").pairing_link,
            None
        );
        disabled.shutdown();
        disabled.join().await.expect("second server joins");
        handle.shutdown();
        handle.join().await.expect("server joins");
    }

    #[test]
    fn connect_descriptor_advertises_remote_update_support() {
        let mut config = ServerConfig::new("/tmp/bibcode-connect-descriptor-test");
        config.storage_instance_id = Some(crate::persistence::StorageInstanceId::from_uuid(
            uuid::Uuid::nil(),
        ));
        config.boot_id = Some(uuid::Uuid::nil());
        let descriptor = connect_environment_descriptor(&config);
        assert_eq!(descriptor["bootId"], "00000000-0000-0000-0000-000000000000");
        assert_eq!(descriptor["capabilities"]["repositoryIdentity"], true);
        assert_eq!(descriptor["capabilities"]["remoteUpdateControl"], true);
        assert_eq!(descriptor["capabilities"]["remoteUpdateProgress"], true);
        assert_eq!(descriptor["capabilities"]["terminalOrderedInput"], true);
        assert_eq!(descriptor["capabilities"]["terminalSizeOwnership"], true);
        assert_eq!(descriptor["capabilities"]["terminalImagePaste"], true);
        assert_eq!(descriptor["capabilities"]["pullRequestCreateOptions"], true);
        assert_eq!(descriptor["capabilities"]["vcsCloneReattach"], true);
        assert_eq!(descriptor["capabilities"]["attachmentStaging"], true);
        assert_eq!(descriptor["capabilities"]["profiler"], true);
        assert_eq!(
            descriptor["remoteUpdateSupport"],
            serde_json::json!({
                "installMode": "manual",
                "reason": "manual-update-required",
                "installKind": "unknown"
            })
        );
    }

    #[tokio::test]
    async fn rejects_relative_programmatic_data_roots_before_creating_state() {
        let error = match ServerRuntime::start(ServerConfig::new("relative/.bibcode")).await {
            Ok(_) => panic!("relative data root must fail at runtime start"),
            Err(error) => error,
        };

        assert!(matches!(error, ServerError::DataRoot(_)));
    }

    #[tokio::test]
    async fn default_ui_observers_match_the_server_runtime_mode() {
        let rows = Arc::<[crate::diagnostics::ProcessRow]>::from([]);
        let server_identity = crate::diagnostics::ProcessIdentity {
            pid: std::process::id(),
            started_at: 1,
        };
        let web = default_ui_process_observer(crate::config::ServerMode::Web)
            .observe(rows.clone(), server_identity)
            .await;
        assert_eq!(
            web.coverage.status,
            crate::diagnostics::UiCoverageStatus::NotApplicable
        );
        assert!(web.coverage.message.is_none());

        let desktop = default_ui_process_observer(crate::config::ServerMode::Desktop)
            .observe(rows, server_identity)
            .await;
        assert_eq!(
            desktop.coverage.status,
            crate::diagnostics::UiCoverageStatus::Unavailable
        );
        let message = desktop.coverage.message.expect("unavailable explanation");
        assert!(message.contains("Native server usage is included"));
        assert!(message.contains("local UI/WebView usage"));
        assert!(message.chars().count() <= 160);
    }

    #[tokio::test]
    async fn server_runtime_covers_production_fallback_startup_access_and_shutdown_paths() {
        let production_state = tempfile::tempdir().expect("production state directory");
        let production_config =
            ServerConfig::new(production_state.path()).with_bind("127.0.0.1", 0);
        hermetic_providers::write_hermetic_settings(&production_config.state_dir(), json!({}));
        let production = ServerRuntime::start(production_config)
            .await
            .expect("production server should start");
        let startup = production
            .startup_access()
            .expect("web server should issue startup access")
            .clone();
        let client = reqwest::Client::new();
        let descriptor = reqwest::get(format!(
            "http://{}/.well-known/bibcode/environment",
            production.local_addr()
        ))
        .await
        .expect("environment descriptor should respond");
        assert!(descriptor.status().is_success());
        let descriptor = descriptor
            .json::<serde_json::Value>()
            .await
            .expect("environment descriptor should decode");
        assert!(descriptor["capabilities"].get("worktreeCatalog").is_none());
        assert!(
            descriptor["capabilities"]
                .get("worktreeCatalogRefreshReason")
                .is_none()
        );
        let token = client
            .post(format!("http://{}/oauth/token", production.local_addr()))
            .form(&[
                (
                    "grant_type",
                    "urn:ietf:params:oauth:grant-type:token-exchange",
                ),
                ("subject_token", startup.credential.as_str()),
                (
                    "subject_token_type",
                    "urn:bibcode:params:oauth:token-type:environment-bootstrap",
                ),
                (
                    "requested_token_type",
                    "urn:ietf:params:oauth:token-type:access_token",
                ),
            ])
            .send()
            .await
            .expect("startup credential should exchange")
            .json::<serde_json::Value>()
            .await
            .expect("token response should decode");
        let snapshot = client
            .get(format!(
                "http://{}/api/orchestration/snapshot",
                production.local_addr()
            ))
            .bearer_auth(token["access_token"].as_str().expect("access token"))
            .send()
            .await
            .expect("orchestration snapshot should respond");
        assert!(snapshot.status().is_success());
        let link_state = client
            .get(format!(
                "http://{}/api/connect/link-state",
                production.local_addr()
            ))
            .bearer_auth(token["access_token"].as_str().expect("access token"))
            .send()
            .await
            .expect("connect link state should respond");
        assert!(link_state.status().is_success());
        let diagnostic = client
            .post(format!(
                "http://{}/api/diagnostics/logs.zip",
                production.local_addr()
            ))
            .bearer_auth(token["access_token"].as_str().expect("access token"))
            .json(&serde_json::json!({"frontendLog":"unit lifecycle log"}))
            .send()
            .await
            .expect("diagnostic logs should respond");
        assert!(diagnostic.status().is_success());
        production.shutdown();
        production.wait_for_shutdown().await;
        production
            .join()
            .await
            .expect("production server should join");

        let fallback_state = tempfile::tempdir().expect("fallback state directory");
        let fallback_config = ServerConfig::new(fallback_state.path()).with_bind("127.0.0.1", 0);
        let fallback = ServerRuntime::start_with_registry(fallback_config, RpcRegistry::empty())
            .await
            .expect("fallback server should start");
        let fallback_credential = fallback
            .startup_access()
            .expect("fallback server should issue startup access")
            .credential
            .clone();
        let fallback_token = client
            .post(format!("http://{}/oauth/token", fallback.local_addr()))
            .form(&[
                (
                    "grant_type",
                    "urn:ietf:params:oauth:grant-type:token-exchange",
                ),
                ("subject_token", fallback_credential.as_str()),
                (
                    "subject_token_type",
                    "urn:bibcode:params:oauth:token-type:environment-bootstrap",
                ),
                (
                    "requested_token_type",
                    "urn:ietf:params:oauth:token-type:access_token",
                ),
            ])
            .send()
            .await
            .expect("fallback startup credential should exchange")
            .json::<serde_json::Value>()
            .await
            .expect("fallback token response should decode");
        let response = client
            .post(format!(
                "http://{}/api/orchestration/dispatch",
                fallback.local_addr()
            ))
            .bearer_auth(
                fallback_token["access_token"]
                    .as_str()
                    .expect("fallback access token"),
            )
            .json(&serde_json::json!({}))
            .send()
            .await
            .expect("fallback route should respond");
        assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        for response in [
            client
                .post(format!(
                    "http://{}/api/diagnostics/logs.zip",
                    fallback.local_addr()
                ))
                .bearer_auth(
                    fallback_token["access_token"]
                        .as_str()
                        .expect("fallback access token"),
                )
                .json(&serde_json::json!({"frontendLog":"fallback"}))
                .send()
                .await
                .expect("fallback diagnostics should respond"),
            client
                .get(format!(
                    "http://{}/api/assets/token/file",
                    fallback.local_addr()
                ))
                .bearer_auth(
                    fallback_token["access_token"]
                        .as_str()
                        .expect("fallback access token"),
                )
                .send()
                .await
                .expect("fallback asset should respond"),
            client
                .post(format!("http://{}/mcp", fallback.local_addr()))
                .bearer_auth(
                    fallback_token["access_token"]
                        .as_str()
                        .expect("fallback access token"),
                )
                .body("{}")
                .send()
                .await
                .expect("fallback MCP should respond"),
        ] {
            assert!(response.status().is_client_error() || response.status().is_server_error());
        }
        fallback.shutdown();
        fallback.join().await.expect("fallback server should join");

        let ipv4 = build_startup_access(
            "0.0.0.0:3773".parse().expect("IPv4 socket address"),
            "pairing credential".to_string(),
        )
        .expect("IPv4 startup access should build");
        assert_eq!(ipv4.connection_string, "http://localhost:3773");
        assert!(ipv4.pairing_url.contains("token=pairing+credential"));

        let ipv6 = build_startup_access(
            "[::]:3774".parse().expect("IPv6 socket address"),
            "credential".to_string(),
        )
        .expect("IPv6 startup access should build");
        assert_eq!(ipv6.connection_string, "http://localhost:3774");
    }
}
