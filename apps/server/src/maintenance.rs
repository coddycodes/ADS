use std::{collections::BTreeMap, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{Mutex, Notify};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    ServerConfig, ServerMode,
    persistence::{
        BackupTrigger, Database, PreparedStore, StatePaths, StorageInstanceId, StoreClassification,
        StoreOperationGuard, create_verified_backup,
    },
    production::runtime::ProductionRuntime,
    rpc::{MethodMutability, method_mutability},
};

pub const MAINTENANCE_UPDATE_PREPARE_PATH: &str = "/api/maintenance/update/prepare";
pub const MAINTENANCE_UPDATE_COMMIT_PATH: &str = "/api/maintenance/update/commit";
pub const MAINTENANCE_UPDATE_CANCEL_PATH: &str = "/api/maintenance/update/cancel";
pub const MAINTENANCE_UPDATE_STATUS_PATH: &str = "/api/maintenance/update/status";
pub const DESKTOP_MAINTENANCE_TOKEN_HEADER: &str = "x-coddy-studio-desktop-bootstrap-token";

const SHUTDOWN_AFTER_RESPONSE_DELAY: Duration = Duration::from_millis(25);
const MAX_ADMISSION_BLOCKERS: usize = 16;
const MAX_OPERATION_LABEL_CHARS: usize = 160;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RpcMutability {
    Read,
    Mutation,
}

/// Central classification for the typed RPC inventory. Unknown methods fail safe as mutations.
#[must_use]
pub fn rpc_mutability(method: &str) -> RpcMutability {
    match method_mutability(method) {
        Some(MethodMutability::Read) => RpcMutability::Read,
        Some(MethodMutability::Mutation) | None => RpcMutability::Mutation,
    }
}

/// Central HTTP classification. Maintenance controls and shutdown bypass mutation admission.
#[must_use]
pub fn http_mutability(method: &str, path: &str) -> RpcMutability {
    if matches!(
        path,
        MAINTENANCE_UPDATE_PREPARE_PATH
            | MAINTENANCE_UPDATE_COMMIT_PATH
            | MAINTENANCE_UPDATE_CANCEL_PATH
            | MAINTENANCE_UPDATE_STATUS_PATH
            | crate::DESKTOP_SHUTDOWN_PATH
    ) {
        return RpcMutability::Read;
    }
    match method {
        "GET" | "HEAD" | "OPTIONS" => RpcMutability::Read,
        _ => RpcMutability::Mutation,
    }
}

#[derive(Debug, Error)]
pub enum MaintenanceError {
    #[error("persistent mutations are closed for desktop update maintenance")]
    AdmissionClosed,
    #[error(
        "timed out while draining {in_flight} admitted mutations; blocking operations: {blockers}"
    )]
    DrainTimeout { in_flight: u64, blockers: String },
    #[error("update preparation failed: {0}")]
    Preparation(String),
    #[error("the update maintenance operation does not match the active operation")]
    OperationMismatch,
    #[error("no prepared update maintenance operation is active")]
    NoPreparedOperation,
}

#[derive(Clone)]
pub struct RpcAdmissionGate {
    inner: Arc<AdmissionInner>,
}

struct AdmissionInner {
    state: std::sync::Mutex<AdmissionState>,
    drained: Notify,
}

#[derive(Debug)]
struct AdmissionState {
    closed: bool,
    in_flight: u64,
    drain_count: u64,
    next_permit_id: u64,
    active: BTreeMap<u64, ActiveMutation>,
}

#[derive(Debug)]
struct ActiveMutation {
    operation: String,
    admitted_at: Instant,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionBlocker {
    pub operation: String,
    pub age_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionSnapshot {
    pub in_flight: u64,
    pub blockers: Vec<AdmissionBlocker>,
}

impl Default for RpcAdmissionGate {
    fn default() -> Self {
        Self::new()
    }
}

impl RpcAdmissionGate {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AdmissionInner {
                state: std::sync::Mutex::new(AdmissionState {
                    closed: false,
                    in_flight: 0,
                    drain_count: 0,
                    next_permit_id: 0,
                    active: BTreeMap::new(),
                }),
                drained: Notify::new(),
            }),
        }
    }

    pub fn admit(&self, mutability: RpcMutability) -> Result<RpcPermit, MaintenanceError> {
        self.admit_named(mutability, "unnamed mutation")
    }

    pub fn admit_named(
        &self,
        mutability: RpcMutability,
        operation: impl Into<String>,
    ) -> Result<RpcPermit, MaintenanceError> {
        if mutability == RpcMutability::Read {
            return Ok(RpcPermit { _lease: None });
        }
        let mut state = self
            .inner
            .state
            .lock()
            .expect("RPC admission mutex poisoned");
        if state.closed {
            return Err(MaintenanceError::AdmissionClosed);
        }
        state.next_permit_id = state.next_permit_id.wrapping_add(1).max(1);
        let permit_id = state.next_permit_id;
        state.in_flight = state.in_flight.saturating_add(1);
        state.active.insert(
            permit_id,
            ActiveMutation {
                operation: normalized_operation_label(operation.into()),
                admitted_at: Instant::now(),
            },
        );
        Ok(RpcPermit {
            _lease: Some(Arc::new(RpcPermitLease {
                gate: self.clone(),
                permit_id,
            })),
        })
    }

    #[must_use]
    pub fn snapshot(&self) -> AdmissionSnapshot {
        let state = self
            .inner
            .state
            .lock()
            .expect("RPC admission mutex poisoned");
        admission_snapshot(&state)
    }

    pub async fn close_and_drain(&self, deadline: Instant) -> Result<u64, MaintenanceError> {
        {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("RPC admission mutex poisoned");
            if !state.closed {
                state.closed = true;
                state.drain_count = state.in_flight;
            }
        }
        loop {
            let notified = self.inner.drained.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let (in_flight, drain_count) = {
                let state = self
                    .inner
                    .state
                    .lock()
                    .expect("RPC admission mutex poisoned");
                (state.in_flight, state.drain_count)
            };
            if in_flight == 0 {
                return Ok(drain_count);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                let snapshot = self.snapshot();
                let blockers = snapshot
                    .blockers
                    .iter()
                    .map(|blocker| format!("{} ({}ms)", blocker.operation, blocker.age_ms))
                    .collect::<Vec<_>>()
                    .join(", ");
                tracing::warn!(
                    in_flight = snapshot.in_flight,
                    blockers,
                    "desktop update maintenance mutation drain timed out"
                );
                return Err(MaintenanceError::DrainTimeout {
                    in_flight: snapshot.in_flight,
                    blockers,
                });
            }
        }
    }

    pub fn release(&self) -> Result<(), MaintenanceError> {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("RPC admission mutex poisoned");
        state.closed = false;
        state.drain_count = 0;
        Ok(())
    }

    fn permit_released(&self, permit_id: u64) {
        let notify = {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("RPC admission mutex poisoned");
            if state.active.remove(&permit_id).is_some() {
                debug_assert!(state.in_flight > 0);
                state.in_flight = state.in_flight.saturating_sub(1);
            }
            state.in_flight == 0
        };
        if notify {
            self.inner.drained.notify_waiters();
        }
    }
}

fn normalized_operation_label(operation: String) -> String {
    let single_line = operation.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.is_empty() {
        return "unknown mutation".to_owned();
    }
    if single_line.chars().count() <= MAX_OPERATION_LABEL_CHARS {
        return single_line;
    }
    let mut bounded = single_line
        .chars()
        .take(MAX_OPERATION_LABEL_CHARS - 1)
        .collect::<String>();
    bounded.push('…');
    bounded
}

fn admission_snapshot(state: &AdmissionState) -> AdmissionSnapshot {
    AdmissionSnapshot {
        in_flight: state.in_flight,
        blockers: state
            .active
            .values()
            .take(MAX_ADMISSION_BLOCKERS)
            .map(|mutation| AdmissionBlocker {
                operation: mutation.operation.clone(),
                age_ms: u64::try_from(mutation.admitted_at.elapsed().as_millis())
                    .unwrap_or(u64::MAX),
            })
            .collect(),
    }
}

#[derive(Clone)]
pub struct RpcPermit {
    _lease: Option<Arc<RpcPermitLease>>,
}

struct RpcPermitLease {
    gate: RpcAdmissionGate,
    permit_id: u64,
}

impl Drop for RpcPermitLease {
    fn drop(&mut self) {
        self.gate.permit_released(self.permit_id);
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PrepareForUpdateResult {
    pub operation_id: String,
    pub storage_instance_id: StorageInstanceId,
    pub backup_id: String,
    pub drained_operations: u64,
    pub expires_at: String,
}

#[derive(Clone, Debug)]
enum UpdatePhase {
    Idle,
    Preparing(PreparingUpdate),
    Prepared(PrepareForUpdateResult),
    Committed(Uuid),
    Cancelled(Uuid),
    Failed,
    Expired(Uuid),
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
enum UpdatePreparationStage {
    WaitingForMutations,
    QuiescingRuntime,
    AcquiringStoreLock,
    CheckpointingDatabase,
    CreatingVerifiedBackup,
}

#[derive(Clone, Debug)]
struct PreparingUpdate {
    operation_id: Uuid,
    stage: UpdatePreparationStage,
    started_at: Instant,
    deadline: Instant,
}

pub struct UpdateMaintenance {
    admission: RpcAdmissionGate,
    state: Mutex<UpdatePhase>,
    changed: Notify,
    runtime: Arc<ProductionRuntime>,
    database: Database,
    paths: StatePaths,
    storage_instance_id: StorageInstanceId,
    classification: StoreClassification,
    app_version: String,
    shutdown: CancellationToken,
    drain_timeout: Duration,
    lease: Duration,
}

impl UpdateMaintenance {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub(crate) fn new(
        admission: RpcAdmissionGate,
        runtime: Arc<ProductionRuntime>,
        database: Database,
        paths: StatePaths,
        storage_instance_id: StorageInstanceId,
        classification: StoreClassification,
        app_version: String,
        shutdown: CancellationToken,
        drain_timeout: Duration,
        lease: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            admission,
            state: Mutex::new(UpdatePhase::Idle),
            changed: Notify::new(),
            runtime,
            database,
            paths,
            storage_instance_id,
            classification,
            app_version,
            shutdown,
            drain_timeout,
            lease,
        })
    }

    pub async fn prepare(self: &Arc<Self>) -> Result<PrepareForUpdateResult, MaintenanceError> {
        let preparing = loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let mut state = self.state.lock().await;
            match &*state {
                UpdatePhase::Prepared(result) => return Ok(result.clone()),
                UpdatePhase::Preparing(_) => {
                    drop(state);
                    notified.await;
                }
                UpdatePhase::Idle => {
                    let operation_id = Uuid::new_v4();
                    let started_at = Instant::now();
                    let deadline = started_at.checked_add(self.drain_timeout).ok_or_else(|| {
                        MaintenanceError::Preparation("deadline overflow".to_owned())
                    })?;
                    let preparing = PreparingUpdate {
                        operation_id,
                        stage: UpdatePreparationStage::WaitingForMutations,
                        started_at,
                        deadline,
                    };
                    *state = UpdatePhase::Preparing(preparing.clone());
                    break preparing;
                }
                UpdatePhase::Committed(_)
                | UpdatePhase::Cancelled(_)
                | UpdatePhase::Failed
                | UpdatePhase::Expired(_) => return Err(MaintenanceError::NoPreparedOperation),
            }
        };

        let prepared = self.prepare_once(&preparing).await;
        match prepared {
            Ok(result) => {
                *self.state.lock().await = UpdatePhase::Prepared(result.clone());
                self.changed.notify_waiters();
                self.spawn_lease_expiry(preparing.operation_id);
                Ok(result)
            }
            Err(error) => {
                *self.state.lock().await = UpdatePhase::Failed;
                self.changed.notify_waiters();
                self.shutdown.cancel();
                Err(error)
            }
        }
    }

    async fn prepare_once(
        &self,
        preparing: &PreparingUpdate,
    ) -> Result<PrepareForUpdateResult, MaintenanceError> {
        let deadline = preparing.deadline;
        let drained_operations = self.admission.close_and_drain(deadline).await?;
        self.set_preparing_stage(
            preparing.operation_id,
            UpdatePreparationStage::QuiescingRuntime,
        )
        .await;
        tokio::time::timeout_at(deadline, self.runtime.quiesce_for_update())
            .await
            .map_err(|_| MaintenanceError::Preparation("runtime quiesce timed out".to_owned()))?
            .map_err(MaintenanceError::Preparation)?;

        self.set_preparing_stage(
            preparing.operation_id,
            UpdatePreparationStage::AcquiringStoreLock,
        )
        .await;
        let cancellation = CancellationToken::new();
        let remaining = deadline.saturating_duration_since(Instant::now());
        let _operation_guard =
            StoreOperationGuard::acquire(&self.paths.base_dir, cancellation.clone(), remaining)
                .await
                .map_err(|error| MaintenanceError::Preparation(error.to_string()))?;
        self.set_preparing_stage(
            preparing.operation_id,
            UpdatePreparationStage::CheckpointingDatabase,
        )
        .await;
        self.database
            .checkpoint_wal()
            .await
            .map_err(|error| MaintenanceError::Preparation(error.to_string()))?;
        self.set_preparing_stage(
            preparing.operation_id,
            UpdatePreparationStage::CreatingVerifiedBackup,
        )
        .await;
        let context = PreparedStore {
            database: self.database.clone(),
            storage_instance_id: self.storage_instance_id,
            classification: self.classification,
            paths: self.paths.clone(),
        };
        let backup = tokio::time::timeout_at(
            deadline,
            create_verified_backup(
                &self.database,
                &context,
                BackupTrigger::PreUpdate,
                &self.app_version,
            ),
        )
        .await
        .map_err(|_| MaintenanceError::Preparation("verified backup timed out".to_owned()))?
        .map_err(|error| MaintenanceError::Preparation(error.to_string()))?;
        let expires_at = (OffsetDateTime::now_utc()
            + time::Duration::try_from(self.lease)
                .map_err(|error| MaintenanceError::Preparation(error.to_string()))?)
        .format(&Rfc3339)
        .map_err(|error| MaintenanceError::Preparation(error.to_string()))?;
        Ok(PrepareForUpdateResult {
            operation_id: preparing.operation_id.to_string(),
            storage_instance_id: self.storage_instance_id,
            backup_id: backup.manifest.backup_id.to_string(),
            drained_operations,
            expires_at,
        })
    }

    async fn set_preparing_stage(&self, operation_id: Uuid, stage: UpdatePreparationStage) {
        let mut state = self.state.lock().await;
        if let UpdatePhase::Preparing(preparing) = &mut *state
            && preparing.operation_id == operation_id
        {
            preparing.stage = stage;
        }
    }

    fn spawn_lease_expiry(self: &Arc<Self>, operation_id: Uuid) {
        let maintenance = Arc::downgrade(self);
        let lease = self.lease;
        tokio::spawn(async move {
            tokio::time::sleep(lease).await;
            let Some(maintenance) = maintenance.upgrade() else {
                return;
            };
            let mut state = maintenance.state.lock().await;
            if matches!(&*state, UpdatePhase::Prepared(result) if result.operation_id == operation_id.to_string())
            {
                *state = UpdatePhase::Expired(operation_id);
                drop(state);
                maintenance.changed.notify_waiters();
                maintenance.shutdown.cancel();
            }
        });
    }

    pub async fn commit(&self, operation_id: Uuid) -> Result<(), MaintenanceError> {
        let mut state = self.state.lock().await;
        match &*state {
            UpdatePhase::Prepared(result) if result.operation_id == operation_id.to_string() => {
                *state = UpdatePhase::Committed(operation_id);
                Ok(())
            }
            UpdatePhase::Prepared(_) => Err(MaintenanceError::OperationMismatch),
            _ => Err(MaintenanceError::NoPreparedOperation),
        }
    }

    pub async fn cancel(&self, operation_id: Uuid) -> Result<(), MaintenanceError> {
        let mut state = self.state.lock().await;
        match &*state {
            UpdatePhase::Prepared(result) if result.operation_id == operation_id.to_string() => {
                *state = UpdatePhase::Cancelled(operation_id);
                Ok(())
            }
            UpdatePhase::Prepared(_) => Err(MaintenanceError::OperationMismatch),
            _ => Err(MaintenanceError::NoPreparedOperation),
        }
    }

    pub fn shutdown_after_response(&self) {
        let shutdown = self.shutdown.clone();
        tokio::spawn(async move {
            tokio::time::sleep(SHUTDOWN_AFTER_RESPONSE_DELAY).await;
            shutdown.cancel();
        });
    }

    pub async fn status(&self) -> Value {
        let phase = self.state.lock().await.clone();
        update_status_value(&phase, &self.admission.snapshot(), Instant::now())
    }
}

fn update_status_value(phase: &UpdatePhase, admission: &AdmissionSnapshot, now: Instant) -> Value {
    match phase {
        UpdatePhase::Idle => json!({"phase":"idle","result":null}),
        UpdatePhase::Preparing(preparing) => json!({
            "phase":"preparing",
            "operationId":preparing.operation_id.to_string(),
            "stage":preparing.stage,
            "elapsedMs":duration_millis(now.saturating_duration_since(preparing.started_at)),
            "remainingMs":duration_millis(preparing.deadline.saturating_duration_since(now)),
            "inFlightMutations":admission.in_flight,
            "blockers":admission.blockers,
            "result":null,
        }),
        UpdatePhase::Prepared(result) => json!({"phase":"prepared","result":result}),
        UpdatePhase::Committed(operation_id) => {
            json!({"phase":"committed","operationId":operation_id.to_string(),"result":null})
        }
        UpdatePhase::Cancelled(operation_id) => {
            json!({"phase":"cancelled","operationId":operation_id.to_string(),"result":null})
        }
        UpdatePhase::Failed => json!({
            "phase":"failed",
            "inFlightMutations":admission.in_flight,
            "blockers":admission.blockers,
            "result":null,
        }),
        UpdatePhase::Expired(operation_id) => {
            json!({"phase":"expired","operationId":operation_id.to_string(),"result":null})
        }
    }
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Every desktop-mode runtime with a bootstrap token owns an update-maintenance
/// coordinator, whatever its bind; the desktop calls it in process.
#[must_use]
pub(crate) fn update_maintenance_owner_enabled(config: &ServerConfig) -> bool {
    config.mode == ServerMode::Desktop && config.desktop_bootstrap_token.is_some()
}

/// The HTTP maintenance routes stay loopback-or-WSL only (overview "Desktop update
/// protection"); a wildcard native bind answers 404.
#[must_use]
pub(crate) fn maintenance_routes_enabled(config: &ServerConfig) -> bool {
    if !update_maintenance_owner_enabled(config) {
        return false;
    }
    let local_bind = config
        .host
        .parse::<std::net::IpAddr>()
        .is_ok_and(|address| address.is_loopback())
        || config.host.eq_ignore_ascii_case("localhost");
    let desktop_owned_wsl_bind =
        config.desktop_wsl_transport && matches!(config.host.as_str(), "0.0.0.0" | "::");
    local_bind || desktop_owned_wsl_bind
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preparing_status_reports_stage_timing_and_bounded_blockers() {
        let now = Instant::now();
        let phase = UpdatePhase::Preparing(PreparingUpdate {
            operation_id: Uuid::nil(),
            stage: UpdatePreparationStage::WaitingForMutations,
            started_at: now - Duration::from_millis(250),
            deadline: now + Duration::from_secs(1),
        });
        let admission = AdmissionSnapshot {
            in_flight: 1,
            blockers: vec![AdmissionBlocker {
                operation: "server.updateSettings".to_owned(),
                age_ms: 500,
            }],
        };

        let status = update_status_value(&phase, &admission, now);

        assert_eq!(status["phase"], "preparing");
        assert_eq!(status["stage"], "waiting-for-mutations");
        assert_eq!(status["elapsedMs"], 250);
        assert_eq!(status["remainingMs"], 1_000);
        assert_eq!(status["inFlightMutations"], 1);
        assert_eq!(status["blockers"][0]["operation"], "server.updateSettings");
        assert_eq!(status["blockers"][0]["ageMs"], 500);
        assert!(status["blockers"][0].get("payload").is_none());
    }
}
