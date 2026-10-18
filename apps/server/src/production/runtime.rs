use std::{
    collections::HashSet,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, OnceLock},
    time::Duration,
};

use axum::http::StatusCode;
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;

use crate::{
    ServerConfig,
    activity::{
        ActivityCancellationService, ActivityProjections, ActivityRepository,
        AgentActivityController, AgentActivitySource, register_activity_rpc,
    },
    assets::{AssetAccess, ResolvedAsset},
    auth::AuthService,
    crypto::sha256_hex,
    diagnostic_bundle::DiagnosticBundleService,
    diagnostics::{
        DesktopUiProcessObserver, DiagnosticsMonitor, NativeProcessSampler, NativeResourceSampler,
        ProcessAttributionRegistry, TraceDiagnosticsStore, bound_diagnostic_string,
        profiler::{
            crash::MonitorDetector,
            latency::LatencyDetector,
            profiler_dir,
            runtime::{Detector, ProfilerRuntime, SessionHeader},
            sampler::SamplerDetector,
            worktree_audit::{AuditSources, WorktreeAuditDetector},
        },
    },
    git::GitRepository,
    mcp::preview_automation::PreviewAutomationBroker,
    orchestration::{EngineOptions, OrchestrationCommand, OrchestrationEngine, load_snapshot},
    persistence::{Database, Repositories, StatePaths},
    preview::{
        PreviewManager,
        gateway::{
            registry::{PreviewGateway, gateway_context},
            relay::RelayHosts,
        },
        tunnel::PreviewTunnels,
    },
    process::configure_background_command,
    production::{
        agent_activity::ProductionAgentActivity,
        clone_operations::CloneRuntime,
        connect_mcp::ConnectMcpService,
        control::{NativeServerControl, ProviderUpdateCheckTask},
        git_manager_rpc::{GitManagerRpcServices, register_git_manager_rpc},
        git_vcs::{GitVcsRpcServices, WorktreeRemovalTaskTracker, register_git_vcs_rpc},
        http_routes::{
            AssetHttpResponse, DiagnosticLogsHttpResponse, HttpRouteError, JsonOperation,
            JsonRouteResponse, RouteContext, TransferDownloadHandler, TransferUploadHandler,
        },
        managed_endpoint::ManagedEndpointRuntime,
        operational_logs::{OperationalLogOptions, OperationalLogs},
        orchestration_effects::{
            BoxEffectFuture, EffectsOptions, OrchestrationEffectCallbacks, OrchestrationEffects,
            ProviderRewind, SetupScriptLaunch, process_compatible_path,
        },
        orchestration_rpc::register_orchestration_rpc_with_delivery_and_availability,
        provider_runtime::{
            NativeProviderDriverFactory, ProviderRuntimeSupervisor, SupervisorOptions,
            reconcile_abandoned_provider_sessions, rewind_thread_conversation,
        },
        pull_requests_rpc::{PullRequestsRpcServices, register_pull_requests_rpc},
        relay::relay_client_service,
        server_terminal::{
            ProcessTreeCleanup, ServerTerminalServices, register_server_terminal_rpc,
        },
        transfer_routes,
        turn_delivery::{TurnDeliveryService, workspace_admission_detail},
        workspace_availability::WorkspaceAdmissionController,
        workspace_preview::{WorkspacePreviewRpcServices, register_workspace_preview_rpc},
        worktree_catalog_rpc::{
            WorktreeCatalogOperationRuntime, WorktreeCatalogRpcServices,
            register_worktree_catalog_rpc,
        },
        worktree_runtime::WorktreeRuntime,
    },
    provider::attachments::{AttachmentMaterializer, MAX_ATTACHMENT_BYTES},
    provider_terminal::{
        ClaudeTerminalObserverFactory, CodexTerminalObserverFactory,
        OpenCodeTerminalObserverFactory, ProviderSettingsInventoryAuthority,
        ProviderTerminalActivitySupervisor, ProviderTerminalObserverFactories,
    },
    provider_usage::{ProviderUsageService, production_fetchers},
    review::{
        ReviewBackend, ReviewDiffPreviewInput, ReviewDiffPreviewResult, ReviewError, ReviewService,
        ReviewSource,
    },
    rpc::RpcRegistry,
    server_settings::ProviderSettingsStore,
    terminal::{PortablePtyBackend, TerminalManager, TerminalManagerOptions},
    transfer::TransferAccess,
    workspace::{AssetContextResolver, WorkspaceRpc, WorkspaceRpcDependencies, WorkspaceService},
    worktree_catalog::{WorkspaceAvailabilityRegistry, WorktreeCatalogService},
};

pub struct ProductionRuntime {
    pub registry: RpcRegistry,
    attachment_uploads: crate::transfer::staging::UploadRegistry,
    upload_sweeper: crate::transfer::staging::UploadSweepTask,
    pub orchestration: OrchestrationEngine,
    pub activity_projections: ActivityProjections,
    pub preview_automation: PreviewAutomationBroker,
    pub preview: PreviewManager,
    preview_gateway: PreviewGateway,
    preview_tunnels: PreviewTunnels,
    asset_access: AssetAccess,
    transfer_access: TransferAccess,
    terminal_services: ServerTerminalServices,
    provider_runtime: Arc<ProviderRuntimeSupervisor>,
    turn_delivery: Arc<TurnDeliveryService>,
    operational_logs: OperationalLogs,
    server_control: Arc<NativeServerControl>,
    provider_update_checks: ProviderUpdateCheckTask,
    orchestration_effects: OrchestrationEffects,
    diagnostic_bundle: DiagnosticBundleService,
    managed_endpoint: ManagedEndpointRuntime,
    _worktree_catalog: WorktreeCatalogService,
    worktree_catalog_operations: WorktreeCatalogOperationRuntime,
    worktree_runtime: WorktreeRuntime,
    worktree_removal_tasks: WorktreeRemovalTaskTracker,
    clone_operations: CloneRuntime,
    status_broadcaster: crate::git::StatusBroadcaster,
    workspace: WorkspaceRpc,
    _resource_sampler: Arc<NativeResourceSampler>,
    process_monitor: Arc<DiagnosticsMonitor<NativeResourceSampler>>,
    profiler: Arc<ProfilerRuntime>,
    update_quiesced: tokio::sync::Mutex<bool>,
}

pub fn finalize_rpc_registry(
    registry: &RpcRegistry,
    control: &NativeServerControl,
) -> Result<(), String> {
    registry.validate_complete()?;
    control.mark_activity_protocol_registered_after_validation();
    Ok(())
}

impl ProductionRuntime {
    pub async fn attach_connect_mcp(&self, service: Arc<ConnectMcpService>) {
        self.provider_runtime.attach_connect_mcp(service).await;
    }

    /// The committed `enableProfiler`, which drives a desktop host's crash monitor.
    #[must_use]
    pub fn profiler_enabled_signal(&self) -> tokio::sync::watch::Receiver<bool> {
        self.server_control.profiler_enabled_signal()
    }

    #[must_use]
    pub fn managed_endpoint_runtime(&self) -> ManagedEndpointRuntime {
        self.managed_endpoint.clone()
    }

    #[must_use]
    pub fn preview_gateway(&self) -> PreviewGateway {
        self.preview_gateway.clone()
    }

    pub fn preview_tunnels(&self) -> &PreviewTunnels {
        &self.preview_tunnels
    }

    /// The handler that serves signed download URLs.
    #[must_use]
    pub fn transfer_download_handler(&self) -> TransferDownloadHandler {
        transfer_routes::download_handler(self.transfer_access.clone())
    }

    /// The handler that accepts signed uploads.
    ///
    /// An upload is a workspace mutation that never passes through `run_workspace_mutation`, so
    /// the callback has to publish both of that path's observable effects itself: drop the
    /// entry-index snapshot for the root, and tell the Git status broadcaster the working tree
    /// changed, exactly as the terminal-exit callback below does.
    #[must_use]
    pub fn transfer_upload_handler(&self) -> TransferUploadHandler {
        let workspace = self.workspace.clone();
        let status_broadcaster = self.status_broadcaster.clone();
        transfer_routes::upload_handler(
            self.transfer_access.clone(),
            Arc::new(move |root| {
                let workspace = workspace.clone();
                let status_broadcaster = status_broadcaster.clone();
                Box::pin(async move {
                    workspace.invalidate_index(&root.to_string_lossy()).await;
                    status_broadcaster.notify_local_change(&root).await;
                })
            }),
        )
    }

    pub async fn start(
        config: &ServerConfig,
        database: Database,
        auth: AuthService,
        asset_secret: Vec<u8>,
        ui_process_observer: Arc<dyn DesktopUiProcessObserver>,
    ) -> Result<Self, String> {
        Self::start_with_process_tree_cleanup(
            config,
            database,
            auth,
            asset_secret,
            ui_process_observer,
            ProcessTreeCleanup::EmbeddedHost,
            None,
        )
        .await
    }

    pub(crate) async fn start_with_process_tree_cleanup(
        config: &ServerConfig,
        database: Database,
        auth: AuthService,
        asset_secret: Vec<u8>,
        ui_process_observer: Arc<dyn DesktopUiProcessObserver>,
        process_tree_cleanup: ProcessTreeCleanup,
        remote_update_delegate: Option<Arc<dyn crate::remote_update::RemoteUpdateDelegate>>,
    ) -> Result<Self, String> {
        let state_paths = StatePaths::from_config(config);
        let trace_diagnostics =
            TraceDiagnosticsStore::new(config.state_dir().join("logs/server.trace.ndjson"));
        let auth_descriptor =
            serde_json::to_value(auth.descriptor()).map_err(|error| error.to_string())?;
        let control = Arc::new(
            NativeServerControl::with_trace_diagnostics(
                config.clone(),
                auth_descriptor,
                trace_diagnostics.clone(),
            )
            .await,
        );
        let provider_update_checks = control.start_provider_update_checks();
        // Built before the profiler, whose sampler reads it; it has no dependencies.
        let process_attribution = ProcessAttributionRegistry::new();
        // Starting replays and compacts the findings file, so it runs off the async workers.
        let profiler_state_dir = config.state_dir();
        let profiler_enabled = control.profiler_enabled_signal();
        // The worktree audit reads the catalog, which is built later and bound below.
        let worktree_audit_sources = Arc::new(OnceLock::new());
        let sampler = SamplerDetector::new(process_attribution.clone());
        let mut profiler_detectors: Vec<Box<dyn Detector>> = vec![
            Box::new(SessionHeader::new(match process_tree_cleanup {
                ProcessTreeCleanup::StandaloneServer => "server",
                ProcessTreeCleanup::EmbeddedHost => "desktop",
            })),
            Box::new(match process_tree_cleanup {
                ProcessTreeCleanup::StandaloneServer => sampler,
                ProcessTreeCleanup::EmbeddedHost => sampler.in_desktop_host(),
            }),
            Box::new(LatencyDetector),
            Box::new(WorktreeAuditDetector::new(
                worktree_audit_sources.clone(),
                process_attribution.clone(),
            )),
        ];
        // One crash hook per process: a desktop host runs the monitor for the server inside it.
        if process_tree_cleanup == ProcessTreeCleanup::StandaloneServer {
            profiler_detectors.push(Box::new(MonitorDetector::new(
                profiler_dir(&profiler_state_dir),
                "server",
            )));
        }
        let profiler = Arc::new(
            tokio::task::spawn_blocking(move || {
                ProfilerRuntime::start(&profiler_state_dir, profiler_enabled, profiler_detectors)
            })
            .await
            .map_err(|error| error.to_string())?,
        );
        let chat_activity_controller = AgentActivityController::new(
            control
                .agent_activity_enabled(AgentActivitySource::Chat)
                .await,
        );
        let terminal_activity_controller = AgentActivityController::new(
            control
                .agent_activity_enabled(AgentActivitySource::Terminal)
                .await,
        );
        let orchestration = OrchestrationEngine::start(
            database,
            EngineOptions {
                queue_capacity: 128,
                ..EngineOptions::default()
            },
        )
        .await
        .map_err(|error| error.to_string())?;
        let repositories = orchestration.repositories();
        let activity_projections = ActivityProjections::new(
            ActivityRepository::new(repositories.database().clone()),
            chat_activity_controller.clone(),
            terminal_activity_controller.clone(),
        );
        activity_projections
            .terminal()
            .interrupt_unresolved_terminal_scopes()
            .await
            .map_err(|error| error.to_string())?;
        reconcile_abandoned_provider_sessions(&orchestration)
            .await
            .map_err(|error| error.to_string())?;
        let referenced_attachment_ids = repositories
            .list_referenced_attachment_ids()
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .collect::<HashSet<_>>();
        AttachmentMaterializer::new(state_paths.attachments_dir.clone())
            .reconcile_startup(&referenced_attachment_ids)
            .await
            .map_err(|error| error.to_string())?;
        crate::transfer::staging::wipe_chat_partials(&state_paths.attachment_uploads_dir)
            .await
            .map_err(|error| error.to_string())?;
        let attachment_uploads = crate::transfer::staging::UploadRegistry::new(
            state_paths.attachment_uploads_dir.clone(),
            crate::transfer::staging::UploadLimits::default(),
            Arc::new(tokio::time::Instant::now),
        );
        let upload_sweeper = attachment_uploads.start_sweeper();
        // The Connect tunnel publishes its preview hostnames here; the preview gateway reads them.
        let relay_hosts = RelayHosts::default();
        let managed_endpoint =
            ManagedEndpointRuntime::with_process_attribution(process_attribution.clone())
                .with_relay_hosts(relay_hosts.clone());
        let provider_settings = ProviderSettingsStore::new(&state_paths.state_dir);
        let provider_terminal_preparer = ProviderTerminalActivitySupervisor::new_with_authority(
            Arc::new(ProviderSettingsInventoryAuthority::new(provider_settings)),
            terminal_activity_controller.clone(),
            activity_projections.terminal(),
            process_attribution.clone(),
            state_paths.state_dir.join("runtime/provider-terminal"),
            production_provider_terminal_observer_factories(),
        )
        .map(|supervisor| {
            Arc::new(supervisor) as Arc<dyn crate::provider_terminal::TerminalLaunchPreparer>
        })
        .map_err(|_| {
            tracing::warn!("optional provider terminal observation is unavailable");
        })
        .ok();
        let process_sampler = Arc::new(NativeProcessSampler::default());
        let resource_sampler = Arc::new(NativeResourceSampler::new(
            process_sampler.clone(),
            process_attribution.clone(),
            ui_process_observer,
        ));
        // Bound to the credential store when lifecycle attaches it to the provider runtime.
        let open_url =
            crate::open_url::OpenUrlEnvironment::new(state_paths.open_url_shim_dir.clone());
        let terminal_open_url = open_url.clone();
        let terminal_manager = TerminalManager::with_process_attribution(
            Arc::new(PortablePtyBackend),
            TerminalManagerOptions {
                launch_preparer: provider_terminal_preparer,
                session_env: Some(TerminalManagerOptions::session_env_hook(
                    move |thread_id, env| {
                        let open_url = terminal_open_url.clone();
                        Box::pin(async move {
                            if let Some(session) = open_url.issue(thread_id).await {
                                session.apply(env);
                            }
                        })
                    },
                )),
                ..TerminalManagerOptions::default()
            },
            process_attribution.clone(),
        );
        let diagnostic_bundle = DiagnosticBundleService::new(&state_paths.logs_dir)
            .with_profiler_dir(profiler_dir(&state_paths.state_dir));
        let operational_logs = OperationalLogs::start(
            &state_paths,
            &terminal_manager,
            OperationalLogOptions::default(),
        )
        .await?;
        let provider_runtime = Arc::new(ProviderRuntimeSupervisor::start_with_operational_log(
            orchestration.clone(),
            Arc::new(
                NativeProviderDriverFactory::with_process_attribution_and_agent_activity_controller(
                    state_paths.attachments_dir.clone(),
                    process_attribution.clone(),
                    chat_activity_controller.clone(),
                )
                .with_published_inventory(control.published_provider_inventory()),
            ),
            activity_projections.chat(),
            SupervisorOptions::default().with_open_url(open_url),
            operational_logs.provider(),
        ));
        let activity_cancellation = ActivityCancellationService::new(
            activity_projections.chat().activity_control_registry(),
            provider_runtime.clone(),
        );
        provider_runtime
            .attach_activity_cancellation(activity_cancellation.clone())
            .await;
        let preview_gateway_secret = asset_secret.clone();
        let transfer_access = TransferAccess::new(asset_secret.clone());
        let asset_access = AssetAccess::new(asset_secret, state_paths.attachments_dir.clone());
        // One host observation: Pull Requests scope resolution and Settings discovery
        // write it, status reads (and the create path) only read it.
        let provider_hosts = Arc::new(crate::source_control::ProviderHosts::default());
        let git_repository = Arc::new(GitRepository::with_worktree_settings(
            control.clone(),
            provider_hosts.clone(),
        ));
        let workspace_availability = WorkspaceAvailabilityRegistry::new();
        let worktree_catalog = WorktreeCatalogService::new_with_availability_registry(
            Arc::new(repositories.clone()),
            git_repository.clone(),
            workspace_availability.clone(),
        );
        let _ = worktree_audit_sources.set(AuditSources {
            catalog: worktree_catalog.clone(),
            repositories: repositories.clone(),
        });
        let pull_requests = PullRequestsRpcServices::with_dependencies(
            config.state_dir(),
            repositories.clone(),
            provider_hosts.clone(),
        );
        let git_vcs = GitVcsRpcServices::with_production_dependencies(
            git_repository.clone(),
            terminal_manager.clone(),
            repositories.clone(),
            control.automatic_git_fetch_interval_signal(),
            provider_hosts,
        )
        .with_created_request_observer(Arc::new(pull_requests.service.clone()))
        .with_availability_registry(workspace_availability.clone());
        let git_vcs = if let Some(dir) = &config.hosting_executable_dir_for_integration_test {
            git_vcs.with_hosting_executable_dir_for_integration_test(dir.clone())
        } else {
            git_vcs
        };
        let worktree_removal_tasks = git_vcs.worktree_removal_tasks();
        let clone_operations = git_vcs.clone_operations();
        let status_broadcaster = git_vcs.status_broadcaster();
        let terminal_status_broadcaster = status_broadcaster.clone();
        terminal_manager.set_process_exit_callback(Arc::new(move |cwd| {
            let status_broadcaster = terminal_status_broadcaster.clone();
            Box::pin(async move {
                status_broadcaster.notify_local_change(&cwd).await;
            })
        }));
        let workspace = WorkspaceRpc::with_dependencies(
            WorkspaceService::default(),
            WorkspaceRpcDependencies {
                asset_access: Some(asset_access.clone()),
                transfer_access: Some(transfer_access.clone()),
                archive_limits: crate::transfer::archive::ArchiveLimits::default(),
                asset_context_resolver: Some(Arc::new(ProjectionAssetContext {
                    repositories: repositories.clone(),
                })),
                review_service: Some(ReviewService::new(Arc::new(GitReviewBackend))),
                mutation_observer: Some(Arc::new(git_vcs.clone())),
            },
        )
        .with_availability_registry(workspace_availability.clone());
        let workspace_for_effects = workspace.clone();
        let preview = PreviewManager::new();
        let preview_automation = PreviewAutomationBroker::new();
        // Gateway listeners bind the same interface as the main server.
        let preview_gateway = PreviewGateway::new(
            config.host.clone(),
            config.environment_label.clone(),
            gateway_context(
                Arc::new(auth.clone()),
                preview_gateway_secret,
                auth.cookie_name().to_owned(),
            ),
            preview.clone(),
            relay_hosts,
        );
        let preview_tunnels = PreviewTunnels::new(config.port, Arc::new(auth.clone()));
        let workspace_preview = WorkspacePreviewRpcServices::new(
            workspace.clone(),
            preview.clone(),
            preview_automation.clone(),
        )
        .with_gateway(preview_gateway.clone())
        .with_tunnels(preview_tunnels.clone());

        let process_monitor = Arc::new(DiagnosticsMonitor::new(
            resource_sampler.clone(),
            Duration::from_secs(2),
        ));
        let provider_usage =
            ProviderUsageService::new(production_fetchers(), Arc::new(OffsetDateTime::now_utc));
        let relay = relay_client_service(config.state_dir());
        let agent_activity = Arc::new(ProductionAgentActivity::new(
            activity_projections.clone(),
            provider_runtime.clone(),
            terminal_manager.clone(),
            trace_diagnostics.clone(),
            config.environment_id.clone(),
        ));
        agent_activity.record_startup(0).await;
        control.attach_agent_activity_handler(agent_activity).await;
        let terminal_services = ServerTerminalServices::new_with_process_tree_cleanup(
            terminal_manager,
            process_sampler,
            resource_sampler.clone(),
            process_monitor.clone(),
            provider_usage,
            relay,
            control.clone(),
            process_tree_cleanup,
        )
        .with_workspace_admission(workspace_availability.clone(), repositories.clone())
        .with_profiler(&profiler)
        .with_environment_id(&config.environment_id);
        terminal_services.watch_workspace_changes(orchestration.subscribe_events());
        let worktree_runtime = WorktreeRuntime::start(
            orchestration.clone(),
            provider_runtime.clone(),
            terminal_services.clone(),
            workspace_availability.clone(),
        );
        worktree_catalog.set_workspace_loss_observer(Arc::new(worktree_runtime.clone()));
        let orchestration_effects = OrchestrationEffects::start(
            orchestration.clone(),
            git_repository.clone(),
            Arc::new(RuntimeEffectCallbacks {
                repositories: orchestration.repositories(),
                engine: orchestration.clone(),
                settings_root: config.state_dir(),
                admission: WorkspaceAdmissionController::new(
                    workspace_availability.clone(),
                    orchestration.repositories(),
                ),
                provider: provider_runtime.clone(),
                terminals: terminal_services.clone(),
                workspace: workspace_for_effects,
                preview: preview.clone(),
                preview_gateway: preview_gateway.clone(),
            }),
            EffectsOptions::default(),
        )
        .await
        .map_err(|error| error.to_string())?;
        let turn_delivery = Arc::new(TurnDeliveryService::start_with_availability(
            orchestration.clone(),
            provider_runtime.clone(),
            config.state_dir(),
            workspace_availability.clone(),
        ));

        let mut registry = RpcRegistry::with_trace_diagnostics(trace_diagnostics.clone());
        crate::auth::register_rpc_handlers(&mut registry, auth);
        register_activity_rpc(
            &mut registry,
            activity_projections.clone(),
            activity_cancellation,
        );
        register_orchestration_rpc_with_delivery_and_availability(
            &mut registry,
            orchestration.clone(),
            provider_runtime.clone(),
            config.state_dir(),
            turn_delivery.clone(),
            workspace_availability.clone(),
            attachment_uploads.clone(),
        );
        crate::production::uploads_rpc::register_uploads_rpc(
            &mut registry,
            attachment_uploads.clone(),
            state_paths.terminal_pastes_dir.clone(),
        );
        crate::production::agent_sessions_rpc::register_agent_sessions_rpc(
            &mut registry,
            orchestration.clone(),
            config.state_dir(),
        );
        register_workspace_preview_rpc(&mut registry, workspace_preview);
        let git_manager = GitManagerRpcServices::with_dependencies(
            git_repository.clone(),
            status_broadcaster.clone(),
            worktree_catalog.clone(),
            repositories.clone(),
            workspace_availability.clone(),
            Arc::new(crate::git::NativeFileTrash::default()),
        );
        register_git_vcs_rpc(&mut registry, git_vcs);
        register_git_manager_rpc(&mut registry, git_manager);
        let worktree_catalog_rpc =
            WorktreeCatalogRpcServices::new(worktree_catalog.clone(), orchestration.clone())
                .with_status_broadcaster(status_broadcaster.clone())
                .with_removal_quiescer(Arc::new(worktree_runtime.clone()));
        register_pull_requests_rpc(
            &mut registry,
            pull_requests.with_worktrees(worktree_catalog_rpc.clone()),
        );
        let worktree_catalog_operations = worktree_catalog_rpc.operation_runtime();
        register_worktree_catalog_rpc(&mut registry, worktree_catalog_rpc);
        register_server_terminal_rpc(&mut registry, terminal_services.clone());
        crate::production::remote_update_rpc::register_remote_update_rpc(
            &mut registry,
            crate::remote_update::RemoteUpdateService::new(
                config.server_version.clone(),
                config.remote_update_support,
                remote_update_delegate.clone(),
            ),
            crate::production::remote_update_rpc::ActiveWorkCounter::new(
                repositories.clone(),
                terminal_services.clone(),
            ),
        );
        finalize_rpc_registry(&registry, &control)?;

        Ok(Self {
            registry,
            attachment_uploads,
            upload_sweeper,
            orchestration,
            activity_projections,
            preview_automation,
            preview,
            preview_gateway,
            preview_tunnels,
            asset_access,
            transfer_access,
            terminal_services,
            provider_runtime,
            turn_delivery,
            operational_logs,
            server_control: control,
            provider_update_checks,
            orchestration_effects,
            diagnostic_bundle,
            managed_endpoint,
            _worktree_catalog: worktree_catalog,
            worktree_catalog_operations,
            worktree_runtime,
            worktree_removal_tasks,
            clone_operations,
            status_broadcaster,
            workspace,
            _resource_sampler: resource_sampler,
            process_monitor,
            profiler,
            update_quiesced: tokio::sync::Mutex::new(false),
        })
    }

    pub async fn json(
        &self,
        operation: JsonOperation,
        payload: Option<Value>,
        _context: RouteContext,
    ) -> Result<JsonRouteResponse, HttpRouteError> {
        match operation {
            JsonOperation::OrchestrationSnapshot => {
                let snapshot = load_snapshot(&self.orchestration.repositories())
                    .await
                    .map_err(internal_error)?;
                Ok(JsonRouteResponse::ok(
                    super::orchestration_rpc::read_model_snapshot(&snapshot, &now_iso()),
                ))
            }
            JsonOperation::OrchestrationDispatch => {
                let command = super::orchestration_rpc::decode_public_orchestration_command(
                    &self.orchestration,
                    payload.ok_or_else(|| bad_request("Request body is required."))?,
                )
                .await
                .map_err(bad_request)?;
                if matches!(command, OrchestrationCommand::ThreadTurnStart { .. }) {
                    return Err(bad_request(
                        "thread.turn.start requires the durable orchestration RPC ingress",
                    ));
                }
                let result =
                    self.orchestration
                        .dispatch(command)
                        .await
                        .map_err(|error| match error {
                            crate::orchestration::OrchestrationError::ProjectPreparation {
                                detail,
                            } => bad_request(detail),
                            error => internal_error(error),
                        })?;
                Ok(JsonRouteResponse::ok(
                    serde_json::to_value(result).map_err(internal_error)?,
                ))
            }
            _ => Err(bad_request("Unsupported core HTTP operation.")),
        }
    }

    pub async fn asset(
        &self,
        token: String,
        path: String,
    ) -> Result<AssetHttpResponse, HttpRouteError> {
        let resolved = self
            .asset_access
            .resolve(&token, &path)
            .await
            .ok_or_else(|| {
                HttpRouteError::new(
                    StatusCode::NOT_FOUND,
                    json!({
                        "_tag": "AssetNotFoundError",
                        "message": "Asset was not found or its access token expired."
                    }),
                )
            })?;
        match resolved {
            ResolvedAsset::File(file) => {
                let file_handle = tokio::fs::File::open(&file).await.map_err(internal_error)?;
                let mut bytes = Vec::with_capacity(MAX_ATTACHMENT_BYTES.min(4096));
                file_handle
                    .take((MAX_ATTACHMENT_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)
                    .await
                    .map_err(internal_error)?;
                if bytes.len() > MAX_ATTACHMENT_BYTES {
                    return Err(HttpRouteError::new(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        json!({
                            "_tag": "AssetTooLargeError",
                            "message": "Asset exceeds the 10 MiB response limit."
                        }),
                    ));
                }
                let content_type = mime_guess::from_path(&file)
                    .first_or_octet_stream()
                    .essence_str()
                    .to_owned();
                Ok(AssetHttpResponse {
                    content_type,
                    bytes,
                    cache_control: "private, max-age=3600".to_owned(),
                })
            }
            ResolvedAsset::ProjectFaviconFallback => Ok(AssetHttpResponse {
                content_type: "image/svg+xml".to_owned(),
                bytes: FALLBACK_FAVICON.as_bytes().to_vec(),
                cache_control: "private, max-age=3600".to_owned(),
            }),
        }
    }

    pub async fn diagnostic_logs(
        &self,
        frontend_log: String,
        include_crash_dumps: bool,
    ) -> Result<DiagnosticLogsHttpResponse, HttpRouteError> {
        let bundle = self
            .diagnostic_bundle
            .build(frontend_log, include_crash_dumps, OffsetDateTime::now_utc())
            .await
            .map_err(internal_error)?;
        Ok(DiagnosticLogsHttpResponse {
            filename: bundle.filename,
            bytes: bundle.bytes,
        })
    }

    pub async fn quiesce_for_update(&self) -> Result<(), String> {
        let mut quiesced = self.update_quiesced.lock().await;
        if *quiesced {
            return Ok(());
        }
        // First, so a run killed while its services stop (a desktop quit bounds the stop) reads
        // as an incomplete shutdown on the next start, not as a crash.
        let profiler = self.profiler.clone();
        let _ = tokio::task::spawn_blocking(move || profiler.begin_stopping()).await;
        // Early, so diagnostics stop sampling before process ownership is frozen and cleaned up.
        self.process_monitor.shutdown().await;
        self.server_control.shutdown_capability_discovery().await;
        self.preview_gateway.shutdown().await;
        self.preview_tunnels.shutdown();
        let process_ownership = self.terminal_services.freeze_process_ownership().await;
        self.managed_endpoint.shutdown().await;
        self.workspace.shutdown().await;
        self.status_broadcaster.shutdown().await;
        self.worktree_catalog_operations.shutdown().await;
        self._worktree_catalog.shutdown().await;
        self.worktree_runtime.shutdown().await;
        let mut first_error = None;
        self.worktree_removal_tasks.close_and_drain().await;
        // Stops every live clone, including detached ones, and waits until each removed the
        // folder it created, before providers and terminals shut down.
        self.clone_operations.close_and_drain().await;
        self.provider_update_checks.shutdown().await;
        self.turn_delivery.shutdown().await;
        self.upload_sweeper.shutdown().await;
        self.attachment_uploads.shutdown().await;
        self.orchestration_effects.shutdown().await;
        if let Err(error) = self.provider_runtime.shutdown().await {
            let error = bound_diagnostic_string(&error.to_string(), 160);
            tracing::warn!(%error, "provider process-owner cleanup completed with failures");
            first_error = Some(error);
        }
        self.terminal_services
            .shutdown_with_process_ownership(process_ownership)
            .await;
        if let Err(error) = self.operational_logs.shutdown().await {
            tracing::warn!(%error, "failed to shut down operational logs cleanly");
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
        self.orchestration.shutdown().await;
        // Last, so the events and panics of every other service's shutdown are recorded, and a
        // crash anywhere in the shutdown still reads as a crash on the next start.
        self.profiler.end_session().await;
        *quiesced = true;
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub async fn shutdown(&self) {
        let _ = self.quiesce_for_update().await;
    }
}

#[derive(Clone)]
struct RuntimeEffectCallbacks {
    repositories: Repositories,
    engine: OrchestrationEngine,
    settings_root: PathBuf,
    /// Held while a revert may reopen the provider, as a delivery holds it, so a workspace being
    /// removed gets no new provider process.
    admission: WorkspaceAdmissionController,
    provider: Arc<ProviderRuntimeSupervisor>,
    terminals: ServerTerminalServices,
    workspace: WorkspaceRpc,
    preview: PreviewManager,
    preview_gateway: PreviewGateway,
}

impl OrchestrationEffectCallbacks for RuntimeEffectCallbacks {
    fn workspace_for_thread<'a>(
        &'a self,
        thread_id: &'a str,
    ) -> BoxEffectFuture<'a, Option<PathBuf>> {
        Box::pin(async move {
            let Some(thread) = self
                .repositories
                .get_thread(thread_id.to_owned())
                .await
                .map_err(|error| error.to_string())?
            else {
                return Ok(None);
            };
            if let Some(path) = thread.worktree_path {
                return Ok(Some(process_compatible_path(PathBuf::from(path))));
            }
            Ok(self
                .repositories
                .get_project(thread.project_id)
                .await
                .map_err(|error| error.to_string())?
                .map(|project| process_compatible_path(PathBuf::from(project.workspace_root))))
        })
    }

    fn rollback_provider<'a>(
        &'a self,
        thread_id: &'a str,
        turns: i64,
        rewind_key: &'a str,
    ) -> BoxEffectFuture<'a, ProviderRewind> {
        Box::pin(async move {
            let _admission = self
                .admission
                .acquire_thread(thread_id, std::iter::empty())
                .await
                .map_err(workspace_admission_detail)?;
            rewind_thread_conversation(
                &self.provider,
                &self.engine,
                &self.settings_root,
                thread_id,
                turns,
                rewind_key,
            )
            .await
        })
    }

    fn stop_provider<'a>(&'a self, thread_id: &'a str) -> BoxEffectFuture<'a, ()> {
        Box::pin(async move {
            match self
                .provider
                .handle_orchestration(OrchestrationCommand::ThreadSessionStop {
                    command_id: format!("effects:provider-stop:{}", uuid::Uuid::new_v4()),
                    thread_id: thread_id.to_owned(),
                    created_at: now_iso(),
                })
                .await
            {
                Ok(())
                | Err(
                    crate::production::provider_runtime::ProviderRuntimeError::SessionNotFound {
                        ..
                    },
                ) => Ok(()),
                Err(error) => Err(error.to_string()),
            }
        })
    }

    fn close_terminals<'a>(&'a self, thread_id: &'a str) -> BoxEffectFuture<'a, ()> {
        Box::pin(async move {
            self.terminals.close_thread_terminals(thread_id).await;
            Ok(())
        })
    }

    fn refresh_workspace<'a>(&'a self, cwd: &'a std::path::Path) -> BoxEffectFuture<'a, ()> {
        Box::pin(async move {
            self.workspace.refresh_index(cwd).await;
            Ok(())
        })
    }

    fn close_previews<'a>(&'a self, thread_id: &'a str) -> BoxEffectFuture<'a, ()> {
        Box::pin(async move {
            self.preview_gateway.close_thread(thread_id).await;
            self.preview
                .close(thread_id, None)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn setup_script_is_running<'a>(
        &'a self,
        thread_id: &'a str,
        terminal_id: &'a str,
    ) -> BoxEffectFuture<'a, bool> {
        Box::pin(async move { Ok(self.terminals.terminal_exists(thread_id, terminal_id).await) })
    }

    fn launch_setup_script<'a>(&'a self, input: SetupScriptLaunch) -> BoxEffectFuture<'a, ()> {
        Box::pin(async move { self.terminals.launch_setup_script(input).await })
    }
}

#[derive(Clone)]
struct ProjectionAssetContext {
    repositories: Repositories,
}

impl AssetContextResolver for ProjectionAssetContext {
    fn resolve_workspace_root<'a>(
        &'a self,
        thread_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<PathBuf>, String>> + Send + 'a>> {
        Box::pin(async move {
            let Some(thread) = self
                .repositories
                .get_thread(thread_id.to_owned())
                .await
                .map_err(|error| error.to_string())?
            else {
                return Ok(None);
            };
            if let Some(worktree) = thread.worktree_path {
                return Ok(Some(process_compatible_path(PathBuf::from(worktree))));
            }
            Ok(self
                .repositories
                .get_project(thread.project_id)
                .await
                .map_err(|error| error.to_string())?
                .map(|project| process_compatible_path(PathBuf::from(project.workspace_root))))
        })
    }
}

struct GitReviewBackend;

const MAX_UNTRACKED_REVIEW_FILES: usize = 500;
const MAX_UNTRACKED_REVIEW_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_UNTRACKED_REVIEW_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
/// Each review preview source is bounded like a Git Manager commit diff.
const MAX_REVIEW_SOURCE_DIFF_BYTES: usize = crate::git::manager::graph::MAX_REASONABLE_DIFF_SIZE;

/// Keeps a diff within `limit`, cut at the last complete file. A diff that was
/// captured incompletely is always treated as truncated.
fn bound_review_diff(diff: String, limit: usize, captured_truncated: bool) -> (String, bool) {
    if !captured_truncated && diff.len() <= limit {
        return (diff, false);
    }
    let mut end = limit.min(diff.len());
    while !diff.is_char_boundary(end) {
        end -= 1;
    }
    let cut = diff[..end]
        .rfind("\ndiff --git ")
        .map_or(0, |index| index + 1);
    (diff[..cut].to_owned(), true)
}

/// Reads at most `limit` bytes plus one probe byte, so an oversized diff stops
/// the capture at once instead of being read to the end.
async fn capture_review_diff<R: tokio::io::AsyncRead + Unpin>(
    reader: R,
    limit: usize,
) -> std::io::Result<(String, bool)> {
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    let truncated = bytes.len() > limit;
    bytes.truncate(limit);
    Ok(bound_review_diff(
        String::from_utf8_lossy(&bytes).into_owned(),
        limit,
        truncated,
    ))
}

impl ReviewBackend for GitReviewBackend {
    fn get_diff_preview<'a>(
        &'a self,
        input: &'a ReviewDiffPreviewInput,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<ReviewDiffPreviewResult>, ReviewError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let cancellation = CancellationToken::new();
            let status = GitRepository::default()
                .local_status(Path::new(&input.cwd), &cancellation)
                .await
                .map_err(|error| ReviewError::Backend(error.to_string()))?;
            if !status.is_repo {
                return Ok(Some(ReviewDiffPreviewResult {
                    cwd: input.cwd.clone(),
                    generated_at: now_millis(),
                    sources: Vec::new(),
                }));
            }

            let ignore_whitespace = input.ignore_whitespace.unwrap_or(false);
            let tracked_worktree = run_review_diff(
                &input.cwd,
                review_diff_args(ignore_whitespace, Some("HEAD"), false),
            )
            .await?;
            let untracked = if tracked_worktree.truncated {
                BoundedReviewDiff {
                    diff: String::new(),
                    truncated: false,
                }
            } else {
                untracked_review_diff(
                    &input.cwd,
                    MAX_REVIEW_SOURCE_DIFF_BYTES.saturating_sub(tracked_worktree.diff.len() + 1),
                )
                .await?
            };
            let (working_tree_diff, working_tree_truncated) = bound_review_diff(
                join_review_diffs(&tracked_worktree.diff, &untracked.diff),
                MAX_REVIEW_SOURCE_DIFF_BYTES,
                false,
            );

            let base_ref = input.base_ref.clone().or(status.default_ref_name);
            let branch_diff = match (&base_ref, &status.ref_name) {
                (Some(base_ref), Some(_)) => {
                    let range = format!("{base_ref}...HEAD");
                    run_review_diff(
                        &input.cwd,
                        review_diff_args(ignore_whitespace, Some(&range), true),
                    )
                    .await?
                }
                _ => BoundedReviewDiff {
                    diff: String::new(),
                    truncated: false,
                },
            };
            let sources = vec![
                review_source(
                    "working-tree",
                    "working-tree",
                    "Dirty worktree",
                    Some("HEAD".to_owned()),
                    None,
                    working_tree_diff,
                    tracked_worktree.truncated || untracked.truncated || working_tree_truncated,
                ),
                review_source(
                    "branch-range",
                    "branch-range",
                    base_ref.as_ref().map_or_else(
                        || "Against base branch".to_owned(),
                        |base| format!("Against {base}"),
                    ),
                    base_ref,
                    Some(status.ref_name.unwrap_or_else(|| "HEAD".to_owned())),
                    branch_diff.diff,
                    branch_diff.truncated,
                ),
            ];
            Ok(Some(ReviewDiffPreviewResult {
                cwd: input.cwd.clone(),
                generated_at: now_millis(),
                sources,
            }))
        })
    }
}

/// One review preview source, bounded at the patch cap; `truncated` says later files were dropped.
struct BoundedReviewDiff {
    diff: String,
    truncated: bool,
}

fn review_diff_args(ignore_whitespace: bool, target: Option<&str>, three_dot: bool) -> Vec<String> {
    let mut args = vec![
        "diff".to_owned(),
        "--no-ext-diff".to_owned(),
        "--patch".to_owned(),
        "--minimal".to_owned(),
    ];
    if ignore_whitespace {
        args.push("--ignore-all-space".to_owned());
    }
    if let Some(target) = target {
        args.push(target.to_owned());
    }
    if !three_dot {
        args.push("--".to_owned());
    }
    args
}

/// A review Git command that `posix_spawn` can start: a bare `git` with the isolated PATH
/// would fork the whole server for every review diff (#85).
fn review_git(build: impl Fn(&mut Command)) -> crate::process::SpawnPlan {
    crate::process::SpawnPlan::new(Path::new("git"), |program| {
        let mut command = Command::new(program);
        configure_background_command(&mut command);
        build(&mut command);
        crate::process::isolate_appimage_environment(&mut command);
        command
    })
}

async fn spawn_review_git(
    plan: crate::process::SpawnPlan,
) -> std::io::Result<tokio::process::Child> {
    plan.start(
        |mut command| std::future::ready(command.spawn()),
        |error| Some(error),
    )
    .await
}

async fn run_review_diff(cwd: &str, args: Vec<String>) -> Result<BoundedReviewDiff, ReviewError> {
    let mut child = spawn_review_git(review_git(|command| {
        command
            .args(["-C", cwd])
            .args(crate::git::CANONICAL_DIFF_CONFIG)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
    }))
    .await
    .map_err(|error| ReviewError::Backend(error.to_string()))?;
    let stdout = child.stdout.take().expect("piped git stdout");
    let captured = capture_review_diff(stdout, MAX_REVIEW_SOURCE_DIFF_BYTES).await;
    if !matches!(&captured, Ok((_, false))) {
        // Stop git once the capture is complete or failed; `wait` reaps it.
        let _ = child.start_kill();
    }
    let status = child
        .wait()
        .await
        .map_err(|error| ReviewError::Backend(error.to_string()))?;
    let (diff, truncated) = captured.map_err(|error| ReviewError::Backend(error.to_string()))?;
    Ok(BoundedReviewDiff {
        diff: if status.success() || truncated {
            diff
        } else {
            String::new()
        },
        truncated,
    })
}

fn join_review_diffs(left: &str, right: &str) -> String {
    [left.trim_end(), right.trim_end()]
        .into_iter()
        .filter(|diff| !diff.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn review_source(
    id: &str,
    kind: &str,
    title: impl Into<String>,
    base_ref: Option<String>,
    head_ref: Option<String>,
    diff: String,
    truncated: bool,
) -> ReviewSource {
    let diff_hash = sha256_hex(diff.as_bytes());
    ReviewSource {
        id: id.to_owned(),
        kind: kind.to_owned(),
        title: title.into(),
        base_ref,
        head_ref,
        diff,
        diff_hash,
        truncated,
    }
}

async fn untracked_review_diff(cwd: &str, limit: usize) -> Result<BoundedReviewDiff, ReviewError> {
    let output = spawn_review_git(review_git(|command| {
        command
            .args(["-C", cwd])
            .args([
                "-c",
                "core.quotePath=false",
                "ls-files",
                "-z",
                "--others",
                "--exclude-standard",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
    }))
    .await
    .map_err(|error| ReviewError::Backend(error.to_string()))?
    .wait_with_output()
    .await
    .map_err(|error| ReviewError::Backend(error.to_string()))?;
    if !output.status.success() {
        return Err(ReviewError::Backend(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }

    let root = PathBuf::from(cwd);
    let mut total_bytes = 0_u64;
    let mut diffs = Vec::new();
    let mut truncated = false;
    let mut patch_bytes = 0_usize;
    for path in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .take(MAX_UNTRACKED_REVIEW_FILES)
    {
        if patch_bytes >= limit {
            truncated = true;
            break;
        }
        let path = String::from_utf8_lossy(path).into_owned();
        let absolute = root.join(&path);
        let metadata = tokio::fs::symlink_metadata(&absolute)
            .await
            .map_err(|error| ReviewError::Backend(error.to_string()))?;
        if !metadata.file_type().is_file() {
            continue;
        }
        if metadata.len() > MAX_UNTRACKED_REVIEW_FILE_BYTES
            || total_bytes.saturating_add(metadata.len()) > MAX_UNTRACKED_REVIEW_TOTAL_BYTES
        {
            let marker = binary_untracked_diff(&path);
            let bytes = marker.len() + usize::from(!diffs.is_empty());
            if patch_bytes.saturating_add(bytes) > limit {
                truncated = true;
                break;
            }
            patch_bytes += bytes;
            diffs.push(marker);
            truncated = true;
            continue;
        }
        let file = tokio::fs::File::open(&absolute)
            .await
            .map_err(|error| ReviewError::Backend(error.to_string()))?;
        let remaining = limit.saturating_sub(patch_bytes);
        let mut contents = Vec::new();
        file.take(remaining.saturating_add(1) as u64)
            .read_to_end(&mut contents)
            .await
            .map_err(|error| ReviewError::Backend(error.to_string()))?;
        if contents.len() > remaining {
            truncated = true;
            break;
        }
        total_bytes = total_bytes.saturating_add(contents.len() as u64);
        let diff = if contents.contains(&0) {
            binary_untracked_diff(&path)
        } else {
            text_untracked_diff(&path, &String::from_utf8_lossy(&contents))
        };
        if patch_bytes.saturating_add(diff.len() + usize::from(!diffs.is_empty())) > limit {
            truncated = true;
            break;
        }
        patch_bytes += diff.len() + usize::from(!diffs.is_empty());
        diffs.push(diff);
    }
    Ok(BoundedReviewDiff {
        diff: diffs.join("\n"),
        truncated,
    })
}

fn binary_untracked_diff(path: &str) -> String {
    format!(
        "diff --git a/{path} b/{path}\nnew file mode 100644\nBinary files /dev/null and b/{path} differ\n"
    )
}

fn text_untracked_diff(path: &str, contents: &str) -> String {
    let line_count = contents.lines().count();
    let mut diff = format!(
        "diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{line_count} @@\n"
    );
    for line in contents.split_inclusive('\n') {
        diff.push('+');
        diff.push_str(line);
    }
    if !contents.is_empty() && !contents.ends_with('\n') {
        diff.push('\n');
        diff.push_str("\\ No newline at end of file\n");
    }
    diff
}

fn bad_request(error: impl std::fmt::Display) -> HttpRouteError {
    HttpRouteError::new(
        StatusCode::BAD_REQUEST,
        json!({
            "_tag": "EnvironmentHttpBadRequestError",
            "message": error.to_string(),
        }),
    )
}

fn production_provider_terminal_observer_factories() -> ProviderTerminalObserverFactories {
    ProviderTerminalObserverFactories {
        codex: Some(Arc::new(CodexTerminalObserverFactory::system())),
        claude: Some(Arc::new(ClaudeTerminalObserverFactory::system())),
        opencode: Some(Arc::new(OpenCodeTerminalObserverFactory::system())),
    }
}

fn internal_error(error: impl std::fmt::Display) -> HttpRouteError {
    HttpRouteError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        json!({
            "_tag": "EnvironmentHttpInternalServerError",
            "message": error.to_string(),
        }),
    )
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc().unix_timestamp().to_string())
}

const FALLBACK_FAVICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" rx="8" fill="#171717"/><path d="M17 19h30v8H36v22h-8V27H17z" fill="#fafafa"/></svg>"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ServerMessage, ServerRuntime,
        activity::{
            ActivityCapabilities, ActivityLifecycle, ActivityProjection, ActivityScopeRef,
            ActivityScopeSeed, ProviderActivityMutation,
        },
        assets::{AssetIssueRequest, AssetResource},
        orchestration::{TurnDeliveryState, TurnDeliveryTransition},
        persistence::run_migrations,
        terminal::{TerminalEvent, TerminalLaunchCommand, TerminalOpenInput},
    };
    use axum::http::{HeaderMap, Uri};
    use futures_util::SinkExt;
    use tempfile::TempDir;
    use tokio::time::timeout;
    use tokio_tungstenite::{connect_async, tungstenite::Message};

    use crate::test_support::hermetic_providers;
    use crate::test_support::websocket_frames::next_frame_past_heartbeat;

    fn route_context() -> RouteContext {
        RouteContext {
            headers: HeaderMap::new(),
            uri: Uri::from_static("/test"),
            cancellation: CancellationToken::new(),
        }
    }

    #[test]
    fn production_terminal_factories_enable_supported_observation() {
        let factories = production_provider_terminal_observer_factories();

        assert!(
            factories.codex.is_some(),
            "production must install the Codex terminal observer factory"
        );
        assert!(
            factories.claude.is_some(),
            "production must install the Claude terminal observer factory"
        );
        assert!(
            factories.opencode.is_some(),
            "production must install the OpenCode terminal observer factory"
        );
    }

    #[tokio::test]
    async fn the_server_repository_reads_the_host_observation_it_is_constructed_with() {
        struct WorktreeSettings;

        impl crate::git::WorktreeBaseDirectoryProvider for WorktreeSettings {
            fn worktree_base_directory<'a>(
                &'a self,
            ) -> crate::git::BoxWorktreeBaseDirectoryFuture<'a> {
                Box::pin(async { None })
            }
        }

        let checkout = TempDir::new().expect("temporary Git repository");
        for args in [
            &["init", "--quiet", "-b", "main"][..],
            &["config", "core.fsmonitor", "false"],
            &[
                "remote",
                "add",
                "origin",
                "https://git.acme.example/team/repo.git",
            ],
        ] {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(checkout.path())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .expect("Git fixture command starts");
            assert!(
                output.status.success(),
                "Git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let hosts = Arc::new(crate::source_control::ProviderHosts::default());
        let repository =
            GitRepository::with_worktree_settings(Arc::new(WorktreeSettings), hosts.clone());
        let cancellation = CancellationToken::new();
        let before = repository
            .status(checkout.path(), &cancellation)
            .await
            .expect("status before recording the host");
        assert!(before.local.is_repo);
        assert!(before.local.has_primary_remote);
        assert_eq!(before.local.source_control_provider, None);

        hosts.record(
            "git.acme.example",
            crate::source_control::ProviderKind::Gitlab,
        );

        let after = repository
            .status(checkout.path(), &cancellation)
            .await
            .expect("status after recording the host");
        assert_eq!(
            after.local.source_control_provider,
            Some(crate::git::SourceControlProviderInfo {
                kind: crate::git::ProviderKind::Gitlab,
                name: "GitLab".to_owned(),
                base_url: "https://git.acme.example".to_owned(),
            }),
            "status must read the supplied host observation after it changes"
        );
    }

    #[tokio::test]
    async fn pull_request_services_keep_the_host_observation_they_are_constructed_with() {
        let state = TempDir::new().expect("temporary state directory");
        let database = Database::open_in_memory().await.expect("database");
        let hosts = Arc::new(crate::source_control::ProviderHosts::default());
        assert_eq!(Arc::strong_count(&hosts), 1);

        let services = PullRequestsRpcServices::with_dependencies(
            state.path().to_path_buf(),
            Repositories::new(database),
            hosts.clone(),
        );

        assert_eq!(
            Arc::strong_count(&hosts),
            2,
            "the constructed services must retain the supplied host observation, not a private one"
        );
        drop(services);
        assert_eq!(
            Arc::strong_count(&hosts),
            1,
            "dropping the services must release their shared host observation"
        );
    }

    #[tokio::test]
    async fn provider_lifecycle_and_delivery_events_do_not_trigger_git_status_reads() {
        let _native_watcher_permit = crate::git::acquire_native_watcher_test_permit().await;
        const NOW: &str = "2026-08-22T00:00:00Z";
        let state = TempDir::new().expect("temporary state directory");
        let repository = TempDir::new().expect("temporary Git repository");
        let git = if cfg!(windows) { "git.exe" } else { "git" };
        let initialized = std::process::Command::new(git)
            .args(["init", "--quiet", "-b", "main"])
            .current_dir(repository.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("Git fixture command starts");
        assert!(
            initialized.status.success(),
            "Git fixture initialization failed: {}",
            String::from_utf8_lossy(&initialized.stderr)
        );
        let config = ServerConfig::new(state.path())
            .with_bind("127.0.0.1", 0)
            .with_unsafe_no_auth();
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let runtime = ProductionRuntime::start(
            &config,
            database.clone(),
            AuthService::new(&config, vec![7_u8; 32]),
            vec![9_u8; 32],
            Arc::new(crate::diagnostics::NotApplicableUiProcessObserver),
        )
        .await
        .expect("production runtime start");
        runtime
            .orchestration
            .dispatch(
                serde_json::from_value(json!({
                    "type":"project.create",
                    "commandId":"provider-no-git-project",
                    "projectId":"provider-no-git-project",
                    "title":"Provider no-Git trigger",
                    "workspaceRoot":repository.path(),
                    "defaultModelSelection":null,
                    "createdAt":NOW,
                }))
                .expect("project command"),
            )
            .await
            .expect("project created");
        let thread_id = runtime
            .orchestration
            .repositories()
            .list_threads_by_project("provider-no-git-project".to_owned())
            .await
            .expect("project threads")
            .into_iter()
            .find(|thread| thread.kind == "default")
            .expect("default thread")
            .thread_id;
        let delivery_thread_id = thread_id.clone();
        database
            .call(move |connection| {
                connection.execute(
                    "INSERT INTO orchestration_command_receipts (command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, error, payload_digest) VALUES (?, 'thread', ?, ?, 0, 'accepted', NULL, NULL)",
                    rusqlite::params!["provider-no-git-delivery", delivery_thread_id, NOW],
                )?;
                connection.execute(
                    "INSERT INTO provider_turn_outbox (command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, attempts, last_error, created_at, updated_at) VALUES ('provider-no-git-delivery', ?, 'provider-no-git-message', 'codex', 'codex', NULL, 'provider-no-git-key', '{}', 'sending', 1, NULL, ?, ?)",
                    rusqlite::params![delivery_thread_id, NOW, NOW],
                )?;
                Ok(())
            })
            .await
            .expect("delivery fixture");
        runtime
            .orchestration
            .dispatch(
                serde_json::from_value(json!({
                    "type":"thread.message.assistant.delta",
                    "commandId":"provider-no-git-assistant-seed",
                    "threadId":thread_id,
                    "messageId":"provider-no-git-assistant-message",
                    "delta":"working",
                    "turnId":null,
                    "createdAt":NOW,
                }))
                .expect("assistant seed command"),
            )
            .await
            .expect("assistant seed");

        let cancellation = CancellationToken::new();
        let mut status = runtime
            .status_broadcaster
            .subscribe(repository.path().to_path_buf(), cancellation.clone())
            .await
            .expect("status subscription");
        assert!(matches!(
            status.recv().await,
            Some(crate::git::VcsStatusStreamEvent::Snapshot { .. })
        ));
        let baseline = runtime
            .status_broadcaster
            .physical_local_read_count_for_test(repository.path())
            .await;
        assert_eq!(baseline, 1);
        let event_baseline = runtime
            .orchestration
            .read_events(0)
            .await
            .expect("baseline events")
            .last()
            .map_or(0, |event| event.sequence);

        runtime
            .orchestration
            .dispatch(
                serde_json::from_value(json!({
                    "type":"thread.message.assistant.complete",
                    "commandId":"provider-no-git-assistant",
                    "threadId":thread_id,
                    "messageId":"provider-no-git-assistant-message",
                    "turnId":null,
                    "createdAt":NOW,
                }))
                .expect("assistant completion command"),
            )
            .await
            .expect("assistant completion");
        runtime
            .orchestration
            .dispatch(
                serde_json::from_value(json!({
                    "type":"thread.session.set",
                    "commandId":"provider-no-git-session",
                    "threadId":thread_id,
                    "session":{
                        "threadId":thread_id,
                        "status":"idle",
                        "providerName":"codex",
                        "providerInstanceId":"codex",
                        "runtimeMode":"full-access",
                        "activeTurnId":null,
                        "lastError":null,
                        "lastErrorClass":null,
                        "updatedAt":NOW
                    },
                    "createdAt":NOW,
                }))
                .expect("provider completion command"),
            )
            .await
            .expect("provider completion");
        assert!(
            runtime
                .orchestration
                .transition_turn_delivery(TurnDeliveryTransition {
                    turn_id: None,
                    command_id: "provider-no-git-delivery".to_owned(),
                    expected_states: vec![TurnDeliveryState::Sending],
                    expected_attempt: 1,
                    next_state: TurnDeliveryState::Failed,
                    detail: Some("provider rejected delivery".to_owned()),
                    updated_at: NOW.to_owned(),
                })
                .await
                .expect("delivery failure transition")
        );
        let events = runtime
            .orchestration
            .read_events(event_baseline)
            .await
            .expect("provider lifecycle events");
        for (event_type, command_id) in [
            ("thread.message-sent", "provider-no-git-assistant"),
            ("thread.session-set", "provider-no-git-session"),
            (
                "thread.turn-delivery-updated",
                "server:turn-delivery:provider-no-git-delivery",
            ),
        ] {
            assert!(events.iter().any(|event| {
                event.event.event_type == event_type
                    && event.event.command_id.as_deref() == Some(command_id)
            }));
        }

        assert!(
            timeout(
                Duration::from_millis(750),
                runtime
                    .status_broadcaster
                    .wait_for_physical_local_read_after_for_test(repository.path(), baseline),
            )
            .await
            .is_err(),
            "provider/session/delivery events must not schedule a physical local status read"
        );
        assert_eq!(
            runtime
                .status_broadcaster
                .physical_local_read_count_for_test(repository.path())
                .await,
            baseline
        );
        drop(status);
        cancellation.cancel();
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn structured_terminal_process_exit_immediately_invalidates_status_under_watcher_fallback()
     {
        let _native_watcher_permit = crate::git::acquire_native_watcher_test_permit().await;
        let state = TempDir::new().expect("temporary state directory");
        let repository = TempDir::new().expect("temporary Git repository");
        let git = if cfg!(windows) { "git.exe" } else { "git" };
        for args in [
            &["init", "--quiet", "-b", "main"][..],
            &["config", "user.name", "BiBCode Test"][..],
            &["config", "user.email", "bibcode@example.invalid"][..],
        ] {
            let output = std::process::Command::new(git)
                .args(args)
                .current_dir(repository.path())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .expect("Git fixture command starts");
            assert!(
                output.status.success(),
                "Git fixture command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::fs::write(repository.path().join("tracked.txt"), "base\n")
            .expect("tracked fixture file");
        let command_cwd = repository.path().join("nested");
        std::fs::create_dir(&command_cwd).expect("nested command cwd");
        for args in [
            &["add", "--", "tracked.txt"][..],
            &["commit", "--quiet", "-m", "initial"][..],
        ] {
            let output = std::process::Command::new(git)
                .args(args)
                .current_dir(repository.path())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .expect("Git fixture command starts");
            assert!(
                output.status.success(),
                "Git fixture command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let config = ServerConfig::new(state.path())
            .with_bind("127.0.0.1", 0)
            .with_unsafe_no_auth();
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let runtime = ProductionRuntime::start(
            &config,
            database,
            AuthService::new(&config, vec![7_u8; 32]),
            vec![9_u8; 32],
            Arc::new(crate::diagnostics::NotApplicableUiProcessObserver),
        )
        .await
        .expect("production runtime start");
        let cancellation = CancellationToken::new();
        let mut status = runtime
            .status_broadcaster
            .subscribe(repository.path().to_path_buf(), cancellation.clone())
            .await
            .expect("status subscription");
        assert!(matches!(
            status.recv().await,
            Some(crate::git::VcsStatusStreamEvent::Snapshot { .. })
        ));
        runtime
            .status_broadcaster
            .shut_down_watcher_for_test()
            .await;
        let read_baseline = runtime
            .status_broadcaster
            .physical_local_read_count_for_test(repository.path())
            .await;
        let refresh_baseline = runtime
            .status_broadcaster
            .local_refresh_generation_for_test(repository.path())
            .await;

        let terminal = runtime.terminal_services.terminal_manager_for_test();
        let mut terminal_events = terminal.subscribe_events();
        let mut input = TerminalOpenInput::new(
            "terminal-status-thread",
            "terminal-status-command",
            command_cwd,
            80,
            24,
        );
        input.command = Some(if cfg!(windows) {
            TerminalLaunchCommand {
                executable: "cmd.exe".to_owned(),
                args: vec![
                    "/d".to_owned(),
                    "/s".to_owned(),
                    "/c".to_owned(),
                    "echo terminal-change>>..\\tracked.txt".to_owned(),
                ],
                label: Some("Status mutation".to_owned()),
                activity: None,
                env: None,
            }
        } else {
            TerminalLaunchCommand {
                executable: "/bin/sh".to_owned(),
                args: vec![
                    "-c".to_owned(),
                    "printf 'terminal-change\\n' >> ../tracked.txt".to_owned(),
                ],
                label: Some("Status mutation".to_owned()),
                activity: None,
                env: None,
            }
        });
        terminal
            .open(input)
            .await
            .expect("structured command starts");
        let exit_code = timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(TerminalEvent::Exited {
                    thread_id,
                    terminal_id,
                    exit_code,
                    ..
                }) = terminal_events.recv().await
                    && thread_id == "terminal-status-thread"
                    && terminal_id == "terminal-status-command"
                {
                    return exit_code;
                }
            }
        })
        .await
        .expect("structured terminal process exits");
        assert_eq!(exit_code, Some(0), "structured command succeeds");
        assert_ne!(
            std::fs::read_to_string(repository.path().join("tracked.txt"))
                .expect("tracked file remains readable"),
            "base\n",
            "structured command mutates the tracked file before Exited"
        );
        timeout(
            Duration::from_millis(750),
            runtime
                .status_broadcaster
                .wait_for_local_refresh_generation_after_for_test(
                    repository.path(),
                    refresh_baseline,
                ),
        )
        .await
        .expect("terminal exit reaches the admitted worktree invalidation seam");

        timeout(
            Duration::from_millis(750),
            runtime
                .status_broadcaster
                .wait_for_physical_local_read_after_for_test(repository.path(), read_baseline),
        )
        .await
        .expect("terminal exit immediately schedules a physical local status read");
        let dirty = timeout(Duration::from_secs(30), async {
            loop {
                if let Some(crate::git::VcsStatusStreamEvent::LocalUpdated { local }) =
                    status.recv().await
                    && local.has_working_tree_changes
                {
                    return local;
                }
            }
        })
        .await;
        let dirty = match dirty {
            Ok(dirty) => dirty,
            Err(_) => {
                let observed_reads = runtime
                    .status_broadcaster
                    .physical_local_read_count_for_test(repository.path())
                    .await;
                panic!(
                    "terminal-triggered status refresh publishes the command mutation; physical reads {read_baseline}->{observed_reads}"
                );
            }
        };
        assert!(
            dirty
                .working_tree
                .files
                .iter()
                .any(|file| file.path == "tracked.txt")
        );
        assert_eq!(
            runtime
                .status_broadcaster
                .physical_local_read_count_for_test(repository.path())
                .await,
            read_baseline + 1,
            "structured terminal exit requests exactly one immediate local read"
        );
        drop(status);
        cancellation.cancel();
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn production_activity_rpc_stream_receives_producer_projection_deltas() {
        // Mutation caught: registering RPC with fresh projections that do not share producer buses.
        let state = TempDir::new().expect("temporary state directory");
        let config = ServerConfig::new(state.path())
            .with_bind("127.0.0.1", 0)
            .with_unsafe_no_auth();
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let runtime = ProductionRuntime::start(
            &config,
            database,
            AuthService::new(&config, vec![7_u8; 32]),
            vec![9_u8; 32],
            Arc::new(crate::diagnostics::NotApplicableUiProcessObserver),
        )
        .await
        .expect("production runtime start");
        let producer_projection = runtime.activity_projections.chat();
        producer_projection
            .ensure_scope(
                ActivityScopeSeed::thread(
                    "thread:production-rpc",
                    "production-rpc",
                    "codex",
                    Some("codex"),
                    ActivityCapabilities::structured_full(false),
                )
                .expect("thread scope"),
            )
            .await
            .expect("scope persistence");
        let handle = ServerRuntime::start_with_registry(config, runtime.registry.clone())
            .await
            .expect("RPC server");
        let mut socket = connect_async(format!("ws://{}/ws", handle.local_addr()))
            .await
            .expect("WebSocket")
            .0;
        socket
            .send(Message::Text(
                json!({
                    "_tag": "Request",
                    "id": "701",
                    "tag": "subscribeActivity",
                    "payload": { "_tag": "thread", "threadId": "production-rpc" },
                    "headers": []
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("subscribe to activity");
        let initial = next_activity_message(&mut socket).await;
        assert!(matches!(
            initial,
            ServerMessage::Chunk { ref values, .. }
                if values[0]["kind"] == "snapshot"
                    && values[0]["snapshot"]["scopeId"] == "thread:production-rpc"
        ));
        socket
            .send(Message::Text(
                json!({ "_tag": "Ack", "requestId": "701" })
                    .to_string()
                    .into(),
            ))
            .await
            .expect("acknowledge initial snapshot");

        producer_projection
            .apply(
                "thread:production-rpc",
                "event:production-rpc".to_owned(),
                vec![
                    ProviderActivityMutation::upsert_actor(
                        "actor:production-rpc",
                        None,
                        "Production RPC actor",
                        "running",
                    )
                    .expect("actor mutation"),
                ],
                "2026-08-04T12:00:00Z".to_owned(),
            )
            .await
            .expect("producer mutation");
        let delta = next_activity_message(&mut socket).await;
        assert!(matches!(
            delta,
            ServerMessage::Chunk { ref values, .. }
                if values[0]["kind"] == "delta"
                    && values[0]["delta"]["scopeId"] == "thread:production-rpc"
        ));

        socket.close(None).await.expect("close WebSocket");
        handle.shutdown();
        handle.join().await.expect("RPC server joins");
        runtime.shutdown().await;
    }

    async fn next_activity_message<S>(
        socket: &mut tokio_tungstenite::WebSocketStream<S>,
    ) -> ServerMessage
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let frame = timeout(Duration::from_secs(2), next_frame_past_heartbeat(socket))
            .await
            .expect("activity message timeout")
            .expect("WebSocket remains open")
            .expect("WebSocket frame");
        let Message::Text(text) = frame else {
            panic!("unexpected WebSocket frame: {frame:?}");
        };
        serde_json::from_str(&text).expect("server message")
    }

    async fn request_settings_update<S>(
        socket: &mut tokio_tungstenite::WebSocketStream<S>,
        request_id: &str,
        patch: Value,
    ) -> Value
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        socket
            .send(Message::Text(
                json!({
                    "_tag": "Request",
                    "id": request_id,
                    "tag": "server.updateSettings",
                    "payload": { "patch": patch },
                    "headers": []
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send settings update");
        let message = next_activity_message(socket).await;
        match message {
            ServerMessage::Exit {
                exit: crate::RpcExit::Success { value: Some(value) },
                ..
            } => value,
            other => panic!("unexpected settings response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn workspace_changes_start_terminal_input_readmission() {
        async fn readmission_after(terminal: &crate::terminal::TerminalManager, from: u64) -> u64 {
            timeout(std::time::Duration::from_secs(2), async {
                loop {
                    let epoch = terminal.input_readmission_epoch();
                    if epoch != from {
                        return epoch;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("the runtime feeds workspace changes to terminal input")
        }

        let state = TempDir::new().expect("state");
        let config = ServerConfig::new(state.path()).with_bind("127.0.0.1", 0);
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let runtime = ProductionRuntime::start(
            &config,
            database,
            AuthService::new(&config, vec![7_u8; 32]),
            vec![9_u8; 32],
            Arc::new(crate::diagnostics::NotApplicableUiProcessObserver),
        )
        .await
        .expect("runtime");
        let terminal = runtime.terminal_services.terminal_manager_for_test();
        let workspace_root = state.path().join("input-project");
        let worktree = state.path().join("input-worktree");
        std::fs::create_dir(&workspace_root).expect("workspace");
        std::fs::create_dir(&worktree).expect("worktree");
        let before = terminal.input_readmission_epoch();
        // The project's default thread is created: `thread.created`.
        runtime
            .orchestration
            .dispatch(
                serde_json::from_value(json!({
                    "type":"project.create","commandId":"input-project","projectId":"input-project",
                    "title":"Project","workspaceRoot":workspace_root,"defaultModelSelection":null,
                    "createdAt":"2026-08-01T00:00:00Z"
                }))
                .expect("project command"),
            )
            .await
            .expect("project");
        let created = readmission_after(&terminal, before).await;
        let thread_id = load_snapshot(&runtime.orchestration.repositories())
            .await
            .expect("snapshot")
            .threads
            .into_iter()
            .find(|thread| thread.kind == "default")
            .expect("default thread")
            .thread_id;
        // The thread moves to a worktree: `thread.meta-updated` carrying `worktreePath`.
        runtime
            .orchestration
            .dispatch(
                serde_json::from_value(json!({
                    "type":"thread.meta.update","commandId":"input-worktree",
                    "threadId":thread_id,"worktreePath":worktree
                }))
                .expect("thread command"),
            )
            .await
            .expect("thread moved");
        readmission_after(&terminal, created).await;
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn http_dispatch_cannot_bypass_durable_or_worktree_authority() {
        let state = TempDir::new().expect("state");
        let config = ServerConfig::new(state.path()).with_bind("127.0.0.1", 0);
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let runtime = ProductionRuntime::start(
            &config,
            database,
            AuthService::new(&config, vec![7_u8; 32]),
            vec![9_u8; 32],
            Arc::new(crate::diagnostics::NotApplicableUiProcessObserver),
        )
        .await
        .expect("runtime");
        let workspace_root = state.path().join("http-project");
        std::fs::create_dir(&workspace_root).expect("workspace");
        runtime
            .orchestration
            .dispatch(
                serde_json::from_value(json!({
                    "type":"project.create","commandId":"http-project","projectId":"http-project",
                    "title":"Project","workspaceRoot":workspace_root,"defaultModelSelection":null,
                    "createdAt":"2026-08-01T00:00:00Z"
                }))
                .expect("project command"),
            )
            .await
            .expect("project");
        let thread_id = load_snapshot(&runtime.orchestration.repositories())
            .await
            .expect("snapshot")
            .threads
            .into_iter()
            .find(|thread| thread.kind == "default")
            .expect("default thread")
            .thread_id;

        let result = runtime.json(
            JsonOperation::OrchestrationDispatch,
            Some(json!({
                "type":"thread.turn.start","commandId":"http-turn","threadId":thread_id,
                "message":{"messageId":"http-message","role":"user","text":"hello","attachments":[]},
                "createdAt":"2026-08-01T00:00:01Z"
            })),
            route_context(),
        ).await;

        assert!(result.is_err());
        assert!(
            runtime
                .orchestration
                .repositories()
                .get_command_receipt("http-turn".to_owned())
                .await
                .expect("receipt")
                .is_none()
        );
        assert!(
            runtime
                .orchestration
                .repositories()
                .get_provider_turn_delivery("http-turn".to_owned())
                .await
                .expect("delivery")
                .is_none()
        );

        runtime
            .orchestration
            .dispatch(
                serde_json::from_value(json!({
                    "type":"thread.create","commandId":"http-owner-create",
                    "threadId":"http-adopted-owner","projectId":"http-project",
                    "title":"Adopted owner","modelSelection":{"instanceId":"codex","model":"gpt-5"},
                    "runtimeMode":"full-access","interactionMode":"default","branch":"feature/http",
                    "worktreePath":workspace_root.join("external-worktree"),"createdAt":"2026-08-01T00:00:02Z"
                }))
                .expect("internal owner command"),
            )
            .await
            .expect("internal owner created");
        for payload in [
            json!({
                "type":"worktree.adopt-resolved","commandId":"http-internal-adopt",
                "projectId":"http-project","worktreeKey":"client-key","path":"/client/path",
                "branch":"main","head":"abcdef1","modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access","interactionMode":"default"
            }),
            json!({
                "type":"project.meta.update","commandId":"http-raw-policy","projectId":"http-project",
                "worktreeDiscovery":{"visibility":"shown","initialPromptDismissedAt":null,"baselinePaths":["/client/baseline"]}
            }),
            json!({
                "type":"thread.create","commandId":"http-raw-path","threadId":"http-raw-owner",
                "projectId":"http-project","title":"Raw owner","modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access","interactionMode":"default","branch":"raw",
                "worktreePath":"/client/worktree","createdAt":"2026-08-01T00:00:03Z"
            }),
            json!({
                "type":"thread.delete","commandId":"http-raw-owner-delete","threadId":"http-adopted-owner"
            }),
            json!({
                "type":"project.delete","commandId":"http-raw-project-delete","projectId":"http-project","force":true
            }),
        ] {
            let command_id = payload["commandId"]
                .as_str()
                .expect("command id")
                .to_owned();
            let rejected = runtime
                .json(
                    JsonOperation::OrchestrationDispatch,
                    Some(payload),
                    route_context(),
                )
                .await;
            assert!(
                rejected.is_err(),
                "HTTP generic dispatch must reject worktree authority"
            );
            assert!(
                runtime
                    .orchestration
                    .repositories()
                    .get_command_receipt(command_id)
                    .await
                    .expect("receipt")
                    .is_none()
            );
        }
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn startup_interrupts_only_unresolved_terminal_activity() {
        let state = TempDir::new().expect("temporary state directory");
        let config = ServerConfig::new(state.path()).with_bind("127.0.0.1", 0);
        hermetic_providers::write_hermetic_settings(
            &config.state_dir(),
            json!({"enableChatAgentActivity": true, "enableTerminalAgentActivity": true}),
        );
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let projection = ActivityProjection::new(ActivityRepository::new(database.clone()));
        let terminal_scope = ActivityScopeSeed::terminal(
            "terminal:restart-active",
            "generation-restart-active",
            "thread-restart",
            "terminal-restart",
            "codex",
            Some("codex"),
            ActivityCapabilities::structured_full(true),
        )
        .expect("terminal scope");
        let thread_scope = ActivityScopeSeed::thread(
            "thread:restart-active",
            "thread-restart",
            "codex",
            Some("codex"),
            ActivityCapabilities::structured_full(false),
        )
        .expect("thread scope");
        projection
            .ensure_scope(terminal_scope.clone())
            .await
            .expect("terminal scope persistence");
        projection
            .ensure_scope(thread_scope.clone())
            .await
            .expect("thread scope persistence");
        let fixture_timestamp = now_iso();
        projection
            .apply(
                &terminal_scope.scope_id,
                "terminal:restart-fixture".to_owned(),
                vec![
                    ProviderActivityMutation::upsert_actor(
                        "terminal-active",
                        None,
                        "Terminal active",
                        "running",
                    )
                    .expect("terminal active actor"),
                    ProviderActivityMutation::upsert_actor(
                        "terminal-completed",
                        None,
                        "Terminal completed",
                        "completed",
                    )
                    .expect("terminal completed actor"),
                ],
                fixture_timestamp.clone(),
            )
            .await
            .expect("terminal activity persistence");
        projection
            .apply(
                &thread_scope.scope_id,
                "thread:restart-fixture".to_owned(),
                vec![
                    ProviderActivityMutation::upsert_actor(
                        "thread-active",
                        None,
                        "Thread active",
                        "running",
                    )
                    .expect("thread active actor"),
                ],
                fixture_timestamp,
            )
            .await
            .expect("thread activity persistence");

        let runtime = ProductionRuntime::start(
            &config,
            database,
            AuthService::new(&config, vec![7_u8; 32]),
            vec![9_u8; 32],
            Arc::new(crate::diagnostics::NotApplicableUiProcessObserver),
        )
        .await
        .expect("production runtime start");

        let terminal_snapshot = runtime
            .activity_projections
            .terminal()
            .snapshot(&ActivityScopeRef::Terminal {
                thread_id: "thread-restart".to_owned(),
                terminal_id: "terminal-restart".to_owned(),
            })
            .await
            .expect("terminal snapshot");
        assert_eq!(
            terminal_snapshot
                .actors
                .iter()
                .find(|actor| actor.id == "terminal-active")
                .expect("terminal active actor")
                .status,
            ActivityLifecycle::Interrupted
        );
        assert_eq!(
            terminal_snapshot
                .actors
                .iter()
                .find(|actor| actor.id == "terminal-completed")
                .expect("terminal completed actor")
                .status,
            ActivityLifecycle::Completed
        );
        let thread_snapshot = runtime
            .activity_projections
            .chat()
            .snapshot(&ActivityScopeRef::Thread {
                thread_id: "thread-restart".to_owned(),
            })
            .await
            .expect("thread snapshot");
        assert_eq!(
            thread_snapshot
                .actors
                .iter()
                .find(|actor| actor.id == "thread-active")
                .expect("thread active actor")
                .status,
            ActivityLifecycle::Running
        );
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn agent_activity_startup_migrates_legacy_true_to_chat_enabled_and_terminal_disabled() {
        let state = TempDir::new().expect("temporary state directory");
        let config = ServerConfig::new(state.path()).with_bind("127.0.0.1", 0);
        hermetic_providers::write_hermetic_settings(
            &config.state_dir(),
            json!({"enableAgentActivity": true}),
        );
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");

        let runtime = ProductionRuntime::start(
            &config,
            database,
            AuthService::new(&config, vec![7_u8; 32]),
            vec![9_u8; 32],
            Arc::new(crate::diagnostics::NotApplicableUiProcessObserver),
        )
        .await
        .expect("production runtime start");

        let chat_state = runtime
            .activity_projections
            .chat()
            .agent_activity_controller_for_integration_test()
            .snapshot();
        let terminal_state = runtime
            .activity_projections
            .terminal()
            .agent_activity_controller_for_integration_test()
            .snapshot();
        assert!(chat_state.enabled);
        assert!(!terminal_state.enabled);
        assert_eq!(chat_state.generation, 0);
        assert_eq!(terminal_state.generation, 0);
        let trace_path = config.state_dir().join("logs/server.trace.ndjson");
        let records = std::fs::read_to_string(trace_path).expect("startup trace");
        let records = records
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("valid trace record"))
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 2);
        assert!(records.iter().any(|record| {
            record["name"] == "agent_activity_enabled"
                && record["events"][0]["attributes"]["cause"] == "startup"
                && record["events"][0]["attributes"]["source"] == "chat"
                && record["events"][0]["attributes"]["enabled"] == true
                && record["events"][0]["attributes"]["observationGeneration"] == 0
        }));
        assert!(records.iter().any(|record| {
            record["name"] == "agent_activity_disabled"
                && record["events"][0]["attributes"]["cause"] == "startup"
                && record["events"][0]["attributes"]["source"] == "terminal"
                && record["events"][0]["attributes"]["enabled"] == false
                && record["events"][0]["attributes"]["observationGeneration"] == 0
        }));
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn agent_activity_runtime_settings_updates_change_only_the_selected_source_controller() {
        let state = TempDir::new().expect("temporary state directory");
        let config = ServerConfig::new(state.path())
            .with_bind("127.0.0.1", 0)
            .with_unsafe_no_auth();
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let runtime = ProductionRuntime::start(
            &config,
            database,
            AuthService::new(&config, vec![7_u8; 32]),
            vec![9_u8; 32],
            Arc::new(crate::diagnostics::NotApplicableUiProcessObserver),
        )
        .await
        .expect("production runtime start");
        let chat_controller = runtime
            .activity_projections
            .chat()
            .agent_activity_controller_for_integration_test();
        let terminal_controller = runtime
            .activity_projections
            .terminal()
            .agent_activity_controller_for_integration_test();
        let initial_chat = chat_controller.snapshot();
        let initial_terminal = terminal_controller.snapshot();
        assert!(initial_chat.enabled);
        assert!(!initial_terminal.enabled);

        let handle = ServerRuntime::start_with_registry(config, runtime.registry.clone())
            .await
            .expect("RPC server");
        let mut socket = connect_async(format!("ws://{}/ws", handle.local_addr()))
            .await
            .expect("WebSocket")
            .0;
        let terminal_settings = request_settings_update(
            &mut socket,
            "801",
            json!({ "enableTerminalAgentActivity": true }),
        )
        .await;
        assert_eq!(terminal_settings["enableTerminalAgentActivity"], true);
        assert_eq!(chat_controller.snapshot(), initial_chat);
        let terminal_enabled = terminal_controller.snapshot();
        assert!(terminal_enabled.enabled);
        assert_eq!(terminal_enabled.generation, initial_terminal.generation + 1);

        let chat_settings = request_settings_update(
            &mut socket,
            "802",
            json!({ "enableChatAgentActivity": false }),
        )
        .await;
        assert_eq!(chat_settings["enableChatAgentActivity"], false);
        assert_eq!(terminal_controller.snapshot(), terminal_enabled);
        let chat_disabled = chat_controller.snapshot();
        assert!(!chat_disabled.enabled);
        assert_eq!(chat_disabled.generation, initial_chat.generation + 1);

        socket.close(None).await.expect("close WebSocket");
        handle.shutdown();
        handle.join().await.expect("RPC server joins");
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn hardening_bootstrap_provider_observation_failure_does_not_abort_production_runtime() {
        let state = TempDir::new().expect("temporary state directory");
        let config = ServerConfig::new(state.path()).with_bind("127.0.0.1", 0);
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        std::fs::write(
            config.state_dir().join("runtime"),
            b"blocks private observer directory",
        )
        .expect("blocking file");
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let auth = AuthService::new(&config, vec![7_u8; 32]);

        let runtime = ProductionRuntime::start(
            &config,
            database,
            auth,
            vec![9_u8; 32],
            Arc::new(crate::diagnostics::NotApplicableUiProcessObserver),
        )
        .await
        .expect("optional provider observation must fail open");

        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn production_runtime_covers_core_routes_assets_diagnostics_and_shutdown() {
        let state = TempDir::new().expect("temporary state directory");
        let config = ServerConfig::new(state.path()).with_bind("127.0.0.1", 0);
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let database = Database::open_in_memory()
            .await
            .expect("in-memory database should open");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("database should migrate");
        let auth = AuthService::new(&config, vec![7_u8; 32]);
        let runtime = ProductionRuntime::start(
            &config,
            database,
            auth,
            vec![9_u8; 32],
            Arc::new(crate::diagnostics::NotApplicableUiProcessObserver),
        )
        .await
        .expect("production runtime should start");
        let resources =
            crate::diagnostics::ResourceSampler::sample(runtime._resource_sampler.as_ref())
                .await
                .expect("attributed resources should sample");
        assert_eq!(
            resources.ui_coverage.status,
            crate::diagnostics::UiCoverageStatus::NotApplicable
        );

        let snapshot = runtime
            .json(JsonOperation::OrchestrationSnapshot, None, route_context())
            .await
            .expect("snapshot route should succeed");
        assert_eq!(snapshot.status, StatusCode::OK);
        assert_eq!(snapshot.body["snapshotSequence"], json!(0));
        assert!(snapshot.body["updatedAt"].is_string());
        assert_eq!(snapshot.body["projects"], json!([]));
        assert_eq!(snapshot.body["threads"], json!([]));
        assert!(snapshot.body.get("states").is_none());

        assert!(
            runtime
                .json(JsonOperation::OrchestrationDispatch, None, route_context())
                .await
                .is_err(),
            "missing dispatch payload should fail",
        );

        assert!(
            runtime
                .json(
                    JsonOperation::OrchestrationDispatch,
                    Some(json!({"_tag":"UnknownCommand"})),
                    route_context(),
                )
                .await
                .is_err(),
            "malformed dispatch payload should fail",
        );

        let callback_workspace = state.path().join("callback-workspace");
        tokio::fs::create_dir_all(&callback_workspace)
            .await
            .expect("callback workspace should create");
        runtime
            .json(
                JsonOperation::OrchestrationDispatch,
                Some(json!({
                    "type":"project.create",
                    "commandId":"runtime-project-create",
                    "projectId":"runtime-project",
                    "title":"Runtime project",
                    "workspaceRoot":callback_workspace,
                    "createdAt":"2026-01-01T00:00:00Z"
                })),
                route_context(),
            )
            .await
            .expect("project dispatch should succeed");
        runtime
            .json(
                JsonOperation::OrchestrationDispatch,
                Some(json!({
                    "type":"thread.create",
                    "commandId":"runtime-thread-create",
                    "threadId":"runtime-thread",
                    "projectId":"runtime-project",
                    "title":"Runtime thread",
                    "modelSelection":{"provider":"codex","model":"auto"},
                    "runtimeMode":"approval-required",
                    "interactionMode":"default",
                    "branch":null,
                    "worktreePath":null,
                    "createdAt":"2026-01-01T00:00:00Z"
                })),
                route_context(),
            )
            .await
            .expect("thread dispatch should succeed");

        let callbacks = RuntimeEffectCallbacks {
            repositories: runtime.orchestration.repositories(),
            engine: runtime.orchestration.clone(),
            settings_root: config.state_dir(),
            admission: WorkspaceAdmissionController::new(
                WorkspaceAvailabilityRegistry::new(),
                runtime.orchestration.repositories(),
            ),
            provider: runtime.provider_runtime.clone(),
            terminals: runtime.terminal_services.clone(),
            workspace: WorkspaceRpc::new(WorkspaceService::default()),
            preview: runtime.preview.clone(),
            preview_gateway: runtime.preview_gateway.clone(),
        };
        let canonical_callback_workspace =
            std::fs::canonicalize(&callback_workspace).expect("callback workspace canonical path");
        assert_eq!(
            callbacks
                .workspace_for_thread("runtime-thread")
                .await
                .expect("thread workspace should resolve"),
            Some(process_compatible_path(
                canonical_callback_workspace.clone()
            )),
        );
        assert_eq!(
            callbacks
                .workspace_for_thread("missing-thread")
                .await
                .expect("missing thread should resolve"),
            None,
        );
        assert!(
            callbacks
                .rollback_provider("missing-thread", 1, "rewind-key")
                .await
                .is_err()
        );
        callbacks
            .stop_provider("missing-thread")
            .await
            .expect("missing provider session should already be stopped");
        callbacks
            .refresh_workspace(&callback_workspace)
            .await
            .expect("workspace index should refresh");
        callbacks
            .close_terminals("runtime-thread")
            .await
            .expect("thread terminals should close");
        callbacks
            .launch_setup_script(SetupScriptLaunch {
                thread_id: "runtime-thread".to_owned(),
                terminal_id: "runtime-setup-terminal".to_owned(),
                script_id: "runtime-setup".to_owned(),
                script_name: "Runtime setup".to_owned(),
                command: "echo coverage".to_owned(),
                cwd: callback_workspace.clone(),
                worktree_path: callback_workspace.clone(),
                env: Default::default(),
            })
            .await
            .expect("setup script should launch");
        callbacks
            .close_terminals("runtime-thread")
            .await
            .expect("setup terminal should close");

        let asset_context = ProjectionAssetContext {
            repositories: runtime.orchestration.repositories(),
        };
        assert_eq!(
            asset_context
                .resolve_workspace_root("runtime-thread")
                .await
                .expect("asset workspace should resolve"),
            Some(process_compatible_path(canonical_callback_workspace)),
        );
        assert_eq!(
            asset_context
                .resolve_workspace_root("missing-thread")
                .await
                .expect("missing asset workspace should resolve"),
            None,
        );

        assert!(
            runtime
                .json(JsonOperation::ConnectLinkState, None, route_context())
                .await
                .is_err(),
            "connect operation should be owned by its route adapter",
        );
        assert!(
            runtime
                .asset("invalid-token".to_string(), "missing.png".to_string())
                .await
                .is_err(),
            "invalid asset token should fail",
        );

        let attachment_id = "runtime-test-image";
        let attachment_path = config.state_dir().join("attachments").join(attachment_id);
        tokio::fs::create_dir_all(attachment_path.parent().expect("attachment parent"))
            .await
            .expect("attachment directory should create");
        tokio::fs::write(&attachment_path, b"png-bytes")
            .await
            .expect("attachment should write");
        let issued = runtime
            .asset_access
            .issue(AssetIssueRequest {
                resource: AssetResource::Attachment {
                    attachment_id: attachment_id.to_string(),
                },
                workspace_root: None,
            })
            .await
            .expect("attachment URL should issue");
        let mut asset_parts = issued.relative_url.rsplitn(2, '/');
        let asset_path = asset_parts.next().expect("asset filename");
        let asset_token = asset_parts.next().expect("asset token path");
        let asset_token = asset_token.rsplit('/').next().expect("asset token");
        let asset = runtime
            .asset(asset_token.to_string(), asset_path.to_string())
            .await
            .expect("attachment asset should resolve");
        assert_eq!(asset.content_type, "application/octet-stream");
        assert_eq!(asset.bytes, b"png-bytes");

        let oversized_id = "runtime-oversized";
        let oversized_path = config.state_dir().join("attachments").join(oversized_id);
        let oversized_file = std::fs::File::create(&oversized_path).expect("oversized attachment");
        oversized_file
            .set_len((10 * 1024 * 1024 + 1) as u64)
            .expect("oversized attachment length");
        let issued = runtime
            .asset_access
            .issue(AssetIssueRequest {
                resource: AssetResource::Attachment {
                    attachment_id: oversized_id.to_owned(),
                },
                workspace_root: None,
            })
            .await
            .expect("oversized attachment may receive a capability");
        let mut asset_parts = issued.relative_url.rsplitn(2, '/');
        let asset_path = asset_parts.next().expect("asset filename");
        let asset_token = asset_parts.next().expect("asset token path");
        let asset_token = asset_token.rsplit('/').next().expect("asset token");
        assert!(
            runtime
                .asset(asset_token.to_owned(), asset_path.to_owned())
                .await
                .is_err(),
            "the HTTP boundary must not return an oversized attachment body"
        );

        let favicon_root = state.path().join("favicon-project");
        tokio::fs::create_dir_all(&favicon_root)
            .await
            .expect("favicon project should create");
        let issued = runtime
            .asset_access
            .issue(AssetIssueRequest {
                resource: AssetResource::ProjectFavicon {
                    cwd: favicon_root.to_string_lossy().into_owned(),
                },
                workspace_root: None,
            })
            .await
            .expect("fallback favicon URL should issue");
        let mut asset_parts = issued.relative_url.rsplitn(2, '/');
        let asset_path = asset_parts.next().expect("favicon filename");
        let asset_token = asset_parts.next().expect("favicon token path");
        let asset_token = asset_token.rsplit('/').next().expect("favicon token");
        let favicon = runtime
            .asset(asset_token.to_string(), asset_path.to_string())
            .await
            .expect("fallback favicon should resolve");
        assert_eq!(favicon.content_type, "image/svg+xml");
        assert!(favicon.bytes.starts_with(b"<svg"));

        let diagnostics = runtime
            .diagnostic_logs("frontend runtime test".to_string(), false)
            .await
            .expect("diagnostic bundle should build");
        assert!(diagnostics.filename.ends_with(".zip"));
        assert!(diagnostics.bytes.starts_with(b"PK"));

        let callback_database = callbacks.repositories.database().clone();
        callback_database
            .call(|connection| {
                connection.execute_batch(
                    "ALTER TABLE projection_projects RENAME TO projection_projects_unavailable",
                )?;
                Ok(())
            })
            .await
            .expect("project table should become unavailable");
        assert!(
            callbacks
                .workspace_for_thread("runtime-thread")
                .await
                .is_err()
        );
        assert!(
            asset_context
                .resolve_workspace_root("runtime-thread")
                .await
                .is_err()
        );
        callback_database
            .call(|connection| {
                connection.execute_batch(
                    "ALTER TABLE projection_projects_unavailable RENAME TO projection_projects;\
                     ALTER TABLE projection_threads RENAME TO projection_threads_unavailable",
                )?;
                Ok(())
            })
            .await
            .expect("thread table should become unavailable");
        assert!(
            callbacks
                .workspace_for_thread("runtime-thread")
                .await
                .is_err()
        );
        assert!(
            asset_context
                .resolve_workspace_root("runtime-thread")
                .await
                .is_err()
        );
        callback_database
            .call(|connection| {
                connection.execute_batch(
                    "ALTER TABLE projection_threads_unavailable RENAME TO projection_threads",
                )?;
                Ok(())
            })
            .await
            .expect("thread table should restore");

        runtime.shutdown().await;
        let discovery = crate::production::server_terminal::ProductionServerControl::call(
            runtime.server_control.as_ref(),
            "server.getProviderCapabilities",
            json!({"instanceId":"cursor","cwd":state.path()}),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(discovery["_tag"], "ServerProviderCapabilitiesError");
        assert!(
            discovery["reason"]
                .as_str()
                .unwrap()
                .contains("shutting down")
        );
    }

    #[tokio::test]
    async fn production_runtime_shutdown_cancels_owned_worktree_catalog_pollers() {
        let state = TempDir::new().expect("temporary state directory");
        let repository = state.path().join("catalog-runtime-project");
        std::fs::create_dir(&repository).expect("repository directory");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&repository)
                .env("GIT_AUTHOR_NAME", "BiBCode Test")
                .env("GIT_AUTHOR_EMAIL", "bibcode@example.invalid")
                .env("GIT_COMMITTER_NAME", "BiBCode Test")
                .env("GIT_COMMITTER_EMAIL", "bibcode@example.invalid")
                .output()
                .expect("Git command");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "--quiet", "-b", "main"]);
        std::fs::write(repository.join("README.md"), "runtime catalog\n")
            .expect("repository fixture");
        git(&["add", "README.md"]);
        git(&["commit", "--quiet", "-m", "initial"]);

        let config = ServerConfig::new(state.path())
            .with_bind("127.0.0.1", 0)
            .with_unsafe_no_auth();
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let runtime = ProductionRuntime::start(
            &config,
            database,
            AuthService::new(&config, vec![7_u8; 32]),
            vec![9_u8; 32],
            Arc::new(crate::diagnostics::NotApplicableUiProcessObserver),
        )
        .await
        .expect("production runtime");
        runtime
            .orchestration
            .dispatch(
                serde_json::from_value(json!({
                    "type": "project.create",
                    "commandId": "catalog-runtime-project",
                    "projectId": "catalog-runtime-project",
                    "title": "Catalog runtime project",
                    "workspaceRoot": repository,
                    "defaultModelSelection": null,
                    "createdAt": "2026-08-09T00:00:00Z"
                }))
                .expect("project command"),
            )
            .await
            .expect("project creation");
        let subscription = runtime
            ._worktree_catalog
            .subscribe("catalog-runtime-project")
            .await
            .expect("catalog subscription");
        assert_eq!(runtime._worktree_catalog.active_poller_count_for_test(), 1);

        runtime.shutdown().await;

        assert_eq!(runtime._worktree_catalog.active_poller_count_for_test(), 0);
        drop(subscription);
    }

    #[tokio::test]
    async fn git_review_backend_includes_untracked_files() {
        let repository = TempDir::new().expect("temporary repository");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(repository.path())
                .status()
                .expect("git starts")
                .success()
        );
        std::fs::write(repository.path().join("untracked.txt"), "first\nsecond\n")
            .expect("write untracked fixture");

        let input = ReviewDiffPreviewInput {
            cwd: repository.path().to_string_lossy().into_owned(),
            base_ref: None,
            ignore_whitespace: Some(false),
        };
        let preview = GitReviewBackend
            .get_diff_preview(&input)
            .await
            .expect("review succeeds")
            .expect("review preview");

        assert_eq!(preview.sources.len(), 2);
        assert_eq!(preview.sources[0].kind, "working-tree");
        assert!(preview.sources[0].diff.contains("+++ b/untracked.txt"));
        assert!(preview.sources[0].diff.contains("+first"));
        assert!(preview.sources[0].diff.contains("+second"));
        assert_eq!(preview.sources[1].kind, "branch-range");
    }

    #[tokio::test]
    async fn git_review_backend_includes_staged_changes_and_branch_range() {
        let repository = TempDir::new().expect("temporary repository");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(repository.path())
                .output()
                .expect("git starts");
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "--quiet"]);
        git(&["checkout", "--quiet", "-b", "main"]);
        git(&["config", "user.email", "test@bibcode.local"]);
        git(&["config", "user.name", "BiBCode Test"]);
        std::fs::write(repository.path().join("tracked.txt"), "base\n").expect("write base");
        git(&["add", "tracked.txt"]);
        git(&["commit", "--quiet", "-m", "base"]);
        git(&["switch", "--quiet", "-c", "feature"]);
        std::fs::write(repository.path().join("branch.txt"), "feature\n").expect("write branch");
        git(&["add", "branch.txt"]);
        git(&["commit", "--quiet", "-m", "feature"]);
        std::fs::write(repository.path().join("tracked.txt"), "staged\n").expect("write staged");
        git(&["add", "tracked.txt"]);

        let input = ReviewDiffPreviewInput {
            cwd: repository.path().to_string_lossy().into_owned(),
            base_ref: None,
            ignore_whitespace: Some(false),
        };
        for (mnemonic, no_prefix, source, destination) in [
            ("true", "false", "a/", "b/"),
            ("false", "true", "a/", "b/"),
            ("false", "false", "before/", "after/"),
        ] {
            git(&["config", "diff.mnemonicPrefix", mnemonic]);
            git(&["config", "diff.noprefix", no_prefix]);
            git(&["config", "diff.srcPrefix", source]);
            git(&["config", "diff.dstPrefix", destination]);
            let config_path = repository.path().join(".git/config");
            let config_before = std::fs::read(&config_path).expect("repository config");

            let preview = GitReviewBackend
                .get_diff_preview(&input)
                .await
                .expect("review succeeds")
                .expect("review preview");

            assert!(preview.sources[0].diff.contains("--- a/tracked.txt"));
            assert!(preview.sources[0].diff.contains("+++ b/tracked.txt"));
            assert!(preview.sources[0].diff.contains("+staged"));
            assert_eq!(preview.sources[1].base_ref.as_deref(), Some("main"));
            assert!(preview.sources[1].diff.contains("+++ b/branch.txt"));
            assert!(preview.sources[1].diff.contains("+feature"));
            assert_eq!(
                std::fs::read(config_path).expect("config after review"),
                config_before,
                "review must not rewrite the repository's display settings"
            );
        }
    }

    #[test]
    fn review_diff_helpers_cover_empty_binary_text_and_whitespace_options() {
        assert_eq!(
            review_diff_args(true, Some("main...HEAD"), true),
            vec![
                "diff",
                "--no-ext-diff",
                "--patch",
                "--minimal",
                "--ignore-all-space",
                "main...HEAD",
            ]
        );
        assert_eq!(
            review_diff_args(false, None, false),
            vec!["diff", "--no-ext-diff", "--patch", "--minimal", "--"]
        );
        assert_eq!(join_review_diffs("", ""), "");
        assert_eq!(join_review_diffs("left\n", "right\n"), "left\nright");
        assert!(binary_untracked_diff("image.bin").contains("Binary files"));
        assert!(text_untracked_diff("empty.txt", "").contains("+1,0"));
        assert!(text_untracked_diff("lines.txt", "first\nsecond\n").contains("+second\n"));
        assert!(
            text_untracked_diff("unterminated.txt", "last")
                .contains("\\ No newline at end of file")
        );

        let source = review_source(
            "working-tree",
            "working-tree",
            "Dirty worktree",
            Some("HEAD".to_owned()),
            None,
            "diff".to_owned(),
            true,
        );
        assert_eq!(source.id, "working-tree");
        assert_eq!(source.diff_hash.len(), 64);
        assert!(source.truncated);
        assert!(now_millis() > 0);
        assert!(now_iso().contains('T'));
    }

    #[tokio::test]
    async fn git_review_backend_returns_an_empty_preview_outside_a_repository() {
        let directory = TempDir::new().expect("temporary directory");
        let preview = GitReviewBackend
            .get_diff_preview(&ReviewDiffPreviewInput {
                cwd: directory.path().to_string_lossy().into_owned(),
                base_ref: None,
                ignore_whitespace: None,
            })
            .await
            .expect("review succeeds")
            .expect("review preview");

        assert!(preview.sources.is_empty());
        assert!(preview.generated_at > 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn review_diff_commands_ignore_appimage_environment() {
        use crate::test_support::appimage_environment::{
            ENVIRONMENT_CASES, assert_child_environment, check_inherited_environment,
        };
        use crate::test_support::reexec;

        const FIXTURE: &str = "BIBCODE_REVIEW_ENVIRONMENT_FIXTURE";
        const TEST: &str =
            "production::runtime::tests::review_diff_commands_ignore_appimage_environment";
        if let Some(child) = reexec::enter(TEST, "review-diff") {
            let directory = PathBuf::from(std::env::var_os(FIXTURE).expect("review fixture path"));
            crate::test_support::run_on_current_thread(async {
                assert_eq!(
                    run_review_diff(
                        &directory.to_string_lossy(),
                        review_diff_args(false, Some("HEAD"), false),
                    )
                    .await
                    .expect("tracked review diff")
                    .diff,
                    "tracked review fixture\n"
                );
                let untracked = untracked_review_diff(
                    &directory.to_string_lossy(),
                    MAX_REVIEW_SOURCE_DIFF_BYTES,
                )
                .await
                .expect("untracked review diff");
                assert!(untracked.diff.contains("+++ b/untracked.txt"));
                assert!(untracked.diff.contains("+untracked review fixture"));
                assert!(!untracked.truncated);
            });
            child.complete();
            return;
        }

        check_inherited_environment(TEST, ENVIRONMENT_CASES, |expected| {
            let directory = tempfile::tempdir().expect("review fixture directory");
            let executable = directory.path().join("git");
            crate::test_support::executable_fixture::write_executable(
                &executable,
                concat!(
                    "#!/bin/sh\n",
                    "[ \"$1\" = -C ] || exit 1\n",
                    "review_cwd=\"$2\"\n",
                    "shift 2\n",
                    "while [ \"$1\" = -c ]; do shift 2; done\n",
                    "case \"$1\" in\n",
                    "  diff) /usr/bin/env -0 > \"$review_cwd/tracked.environment\"; printf 'tracked review fixture\\n' ;;\n",
                    "  ls-files) /usr/bin/env -0 > \"$review_cwd/untracked.environment\"; printf 'untracked.txt\\0' ;;\n",
                    "  *) exit 1 ;;\n",
                    "esac\n",
                ),
            );
            std::fs::write(
                directory.path().join("untracked.txt"),
                "untracked review fixture\n",
            )
            .expect("untracked file fixture");
            reexec::run(TEST, "review-diff", None, |command| {
                command
                    .env(FIXTURE, directory.path())
                    .env("PATH", directory.path());
            });
            let mut expected = expected.clone();
            expected.insert("PATH", Some(directory.path().as_os_str().to_owned()));
            for capture in ["tracked.environment", "untracked.environment"] {
                assert_child_environment(
                    &std::fs::read(directory.path().join(capture)).expect("review environment"),
                    &expected,
                );
            }
        });
    }

    #[tokio::test]
    async fn untracked_review_diff_marks_binary_and_oversized_files() {
        let repository = TempDir::new().expect("temporary repository");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(repository.path())
                .status()
                .expect("git starts")
                .success()
        );
        std::fs::write(repository.path().join("binary.dat"), b"binary\0payload")
            .expect("binary fixture");
        std::fs::write(
            repository.path().join("oversized.dat"),
            vec![b'x'; MAX_UNTRACKED_REVIEW_FILE_BYTES as usize + 1],
        )
        .expect("oversized fixture");

        let diff = untracked_review_diff(
            &repository.path().to_string_lossy(),
            MAX_REVIEW_SOURCE_DIFF_BYTES,
        )
        .await
        .expect("untracked diff");
        assert!(diff.diff.contains("binary.dat"));
        assert!(diff.diff.contains("oversized.dat"));
        assert!(diff.truncated);
    }

    #[cfg(target_os = "linux")]
    const REVIEW_SPAWN_TEST: &str =
        "production::runtime::tests::review_diffs_spawn_git_without_copying_the_server";
    #[cfg(target_os = "linux")]
    const REVIEW_BENCH_TEST: &str = "production::runtime::tests::review_diff_timing_by_host_size";

    /// A repository with a modified tracked file and an untracked one, and an AppImage
    /// launch environment for re-executing `test` against it.
    #[cfg(target_os = "linux")]
    fn run_review_case_as_appimage(
        test: &str,
        phase: &str,
        configure: impl FnOnce(&mut std::process::Command),
    ) {
        use crate::test_support::{fork_probe, isolated_git_config::IsolatedGitConfig, reexec};

        let config = IsolatedGitConfig::new();
        let root = TempDir::new().expect("fixture root");
        let repository = root.path().join("repository");
        std::fs::create_dir_all(&repository).expect("repository directory");
        let git = |args: &[&str]| {
            let mut command = std::process::Command::new("git");
            config.apply_to_command(&mut command);
            let status = command
                .args(args)
                .current_dir(&repository)
                .status()
                .expect("git starts");
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "--quiet"]);
        std::fs::write(repository.join("tracked.txt"), "one\n").expect("tracked file");
        git(&["add", "tracked.txt"]);
        git(&[
            "-c",
            "user.name=Review",
            "-c",
            "user.email=review@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ]);
        std::fs::write(repository.join("tracked.txt"), "one\ntwo\n").expect("modified file");
        std::fs::write(repository.join("untracked.txt"), "new\n").expect("untracked file");
        let environment = fork_probe::appimage_environment(root.path(), &[]);
        reexec::run(test, phase, None, |command| {
            config.apply_to_command(command);
            configure(command);
            command
                .envs(environment)
                .env("BIBCODE_TEST_REVIEW_REPOSITORY", &repository);
        });
    }

    #[cfg(target_os = "linux")]
    fn review_repository() -> String {
        std::env::var("BIBCODE_TEST_REVIEW_REPOSITORY").expect("review repository")
    }

    #[cfg(target_os = "linux")]
    async fn review_both_diffs(repository: &str) {
        let tracked = run_review_diff(repository, review_diff_args(false, None, false))
            .await
            .expect("tracked review diff");
        assert!(tracked.diff.contains("+two"), "{}", tracked.diff);
        let untracked = untracked_review_diff(repository, MAX_REVIEW_SOURCE_DIFF_BYTES)
            .await
            .expect("untracked review diff");
        assert!(
            untracked.diff.contains("untracked.txt"),
            "{}",
            untracked.diff
        );
    }

    /// Review Git commands must not copy the server (#85): inside the AppImage they get a
    /// PATH of their own, which made Rust fork for the bare `git`.
    #[cfg(target_os = "linux")]
    #[test]
    fn review_diffs_spawn_git_without_copying_the_server() {
        use crate::test_support::{fork_probe, reexec};

        let Some(child) = reexec::enter(REVIEW_SPAWN_TEST, "appimage") else {
            run_review_case_as_appimage(REVIEW_SPAWN_TEST, "appimage", |_| {});
            return;
        };
        let region = fork_probe::TouchedRegion::new();
        region.assert_detects_a_fork();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Tokio runtime")
            .block_on(review_both_diffs(&review_repository()));
        let faults = region.rewrite_faults();
        assert!(
            faults < fork_probe::NO_COPY_FAULTS,
            "a review Git child copied the server: {faults} write faults on {} pages",
            region.pages()
        );
        child.complete();
    }

    /// `cargo test -p bibcode-server --lib -- --ignored --nocapture review_diff_timing`
    /// prints the median time of one tracked and one untracked review diff with an idle
    /// host and with a host that holds 4 GiB of touched heap.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "benchmark"]
    fn review_diff_timing_by_host_size() {
        use crate::test_support::reexec;

        for (phase, heap_gib) in [("small", 0), ("large", 4)] {
            if let Some(child) = reexec::enter(REVIEW_BENCH_TEST, phase) {
                let heap = (0..heap_gib * 1024)
                    .map(|_| vec![1_u8; 1 << 20])
                    .collect::<Vec<_>>();
                let repository = review_repository();
                let mut samples = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("Tokio runtime")
                    .block_on(async {
                        let mut samples = Vec::new();
                        for _ in 0..15 {
                            let started = std::time::Instant::now();
                            review_both_diffs(&repository).await;
                            samples.push(started.elapsed());
                        }
                        samples
                    });
                samples.sort();
                let results = std::env::var_os("BIBCODE_BENCH_RESULTS").expect("results path");
                let mut existing = std::fs::read_to_string(&results).unwrap_or_default();
                existing.push_str(&format!(
                    "heap={heap_gib}GiB review diffs median={:?} min={:?} max={:?}\n",
                    samples[samples.len() / 2],
                    samples[0],
                    samples[samples.len() - 1],
                ));
                std::fs::write(results, existing).expect("benchmark results");
                std::hint::black_box(&heap);
                child.complete();
                return;
            }
        }
        let results = TempDir::new().expect("results directory");
        let path = results.path().join("results");
        for phase in ["small", "large"] {
            run_review_case_as_appimage(REVIEW_BENCH_TEST, phase, |command| {
                command
                    .arg("--include-ignored")
                    .env("BIBCODE_BENCH_RESULTS", &path);
            });
        }
        print!(
            "{}",
            std::fs::read_to_string(path).expect("benchmark results")
        );
    }

    #[tokio::test]
    async fn git_review_maps_process_detached_head_and_unreadable_file_edges() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        assert!(
            run_review_diff("\0", review_diff_args(false, Some("HEAD"), false))
                .await
                .is_err()
        );
        assert!(
            untracked_review_diff("\0", MAX_REVIEW_SOURCE_DIFF_BYTES)
                .await
                .is_err()
        );
        assert!(
            format!("{:?}", internal_error("injected review error"))
                .contains("injected review error")
        );

        let repository = TempDir::new().expect("temporary repository");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(repository.path())
                .output()
                .expect("git starts");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "--quiet"]);
        git(&["checkout", "--quiet", "-b", "main"]);
        git(&["config", "user.email", "test@bibcode.local"]);
        git(&["config", "user.name", "BiBCode Test"]);
        std::fs::write(repository.path().join("tracked.txt"), "base\n").expect("tracked file");
        git(&["add", "tracked.txt"]);
        git(&["commit", "--quiet", "-m", "base"]);
        git(&["checkout", "--quiet", "--detach", "HEAD"]);

        let detached = GitReviewBackend
            .get_diff_preview(&ReviewDiffPreviewInput {
                cwd: repository.path().to_string_lossy().into_owned(),
                base_ref: Some("main".to_owned()),
                ignore_whitespace: None,
            })
            .await
            .expect("detached review")
            .expect("detached preview");
        assert_eq!(detached.sources[1].head_ref.as_deref(), Some("HEAD"));

        #[cfg(unix)]
        {
            let unreadable = repository.path().join("unreadable.txt");
            std::fs::write(&unreadable, "secret\n").expect("unreadable fixture");
            std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000))
                .expect("remove read permission");
            let result = untracked_review_diff(
                &repository.path().to_string_lossy(),
                MAX_REVIEW_SOURCE_DIFF_BYTES,
            )
            .await;
            std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o600))
                .expect("restore read permission");
            assert!(result.is_err());
        }
    }

    #[test]
    fn review_diffs_over_the_bound_are_cut_at_a_file_boundary() {
        let first = format!("diff --git a/one b/one\n+{}\n", "1".repeat(40));
        let second = format!("diff --git a/two b/two\n+{}\n", "2".repeat(40));
        let diff = format!("{first}{second}");
        assert_eq!(
            bound_review_diff(diff.clone(), diff.len(), false),
            (diff.clone(), false)
        );
        let (cut, truncated) = bound_review_diff(diff.clone(), first.len() + 16, false);
        assert!(truncated);
        assert_eq!(cut, first);
        let (empty, truncated) = bound_review_diff(diff, 10, false);
        assert!(truncated);
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn review_capture_stops_reading_at_the_cap_plus_one_probe_byte() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Endless(Arc<AtomicUsize>);
        impl tokio::io::AsyncRead for Endless {
            fn poll_read(
                self: Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                let bytes = vec![b'x'; buf.remaining()];
                self.0.fetch_add(bytes.len(), Ordering::Relaxed);
                buf.put_slice(&bytes);
                std::task::Poll::Ready(Ok(()))
            }
        }
        let read = Arc::new(AtomicUsize::new(0));
        let (diff, truncated) = tokio::time::timeout(
            Duration::from_secs(1),
            capture_review_diff(Endless(Arc::clone(&read)), 128),
        )
        .await
        .expect("never wait for EOF on an oversized diff")
        .expect("capture");
        assert_eq!(read.load(Ordering::Relaxed), 129);
        assert!(truncated);
        assert!(diff.len() <= 128);

        let first = "diff --git a/one b/one\n+one\n";
        let second = format!("diff --git a/two b/two\n+{}\n", "x".repeat(1000));
        let input = format!("{first}{second}");
        let (diff, truncated) = capture_review_diff(input.as_bytes(), first.len() + 32)
            .await
            .expect("capture bounded diff");
        assert!(truncated);
        assert_eq!(diff, first, "the incomplete last file is discarded");
    }
}
