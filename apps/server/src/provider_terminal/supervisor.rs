use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

#[cfg(unix)]
use std::io::Write;

use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;

#[cfg(unix)]
use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};

use super::{
    NativeTerminalCommand, PreparedTerminalLaunch, TerminalGenerationActivityPublisher,
    TerminalLaunchPreparation, TerminalLaunchPreparationInput, TerminalLaunchPreparer,
    TerminalObserverCancellationReason,
};
use crate::{
    activity::{ActivityProjection, AgentActivityController},
    diagnostics::ProcessAttributionRegistry,
    process::{Platform, launch_executable_extensions, locate_executable},
    server_settings::{ProviderInstanceState, ProviderSettingsState, ProviderSettingsStore},
};

const MAX_OPERATIONAL_WARNINGS: usize = 64;
const PUBLICATION_LOCK_PRUNE_THRESHOLD: usize = 256;
const RACED_PREPARATION_WORKER_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);
const RACED_PREPARATION_WORKER_ABORT_TIMEOUT: Duration = Duration::from_millis(50);
// Four slots per process admitted by the 16-helper supervisor bound absorb
// normal cleanup overlap while keeping the persistent namespace finite.
#[cfg(unix)]
const MANAGED_GENERATION_SLOT_COUNT: usize = 64;
#[cfg(unix)]
const OWNERSHIP_MARKER_NAME: &str = ".bibcode-provider-terminal-owner";
#[cfg(unix)]
const OWNERSHIP_MARKER_CONTENT: &[u8] = b"bibcode-provider-terminal-v1\n";

#[cfg(unix)]
static MANAGED_GENERATION_SLOT_OPERATION_LOCK: Mutex<()> = Mutex::new(());

#[cfg(unix)]
#[derive(Clone, Copy, Eq, PartialEq)]
enum ManagedGenerationSlotPhase {
    ClaimBeforeLock,
    ClaimMarkerWritten,
    CleanupAfterValidation,
    CleanupAfterContentsRemoved,
}

#[cfg(unix)]
fn lock_managed_generation_slot_operations() -> std::sync::MutexGuard<'static, ()> {
    MANAGED_GENERATION_SLOT_OPERATION_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderTerminalInventoryEntry {
    pub instance_id: String,
    pub driver_kind: String,
    pub enabled: bool,
    pub configured_binary: String,
    pub built_in_binary: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProviderTerminalInventory {
    entries: BTreeMap<String, ProviderTerminalInventoryEntry>,
}

pub trait ProviderTerminalInventoryAuthority: Send + Sync {
    fn current(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderTerminalInventory, ()>> + Send + '_>>;
}

#[derive(Clone)]
pub struct ProviderSettingsInventoryAuthority {
    store: ProviderSettingsStore,
}

impl fmt::Debug for ProviderSettingsInventoryAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderSettingsInventoryAuthority")
            .finish_non_exhaustive()
    }
}

impl ProviderSettingsInventoryAuthority {
    pub fn new(store: ProviderSettingsStore) -> Self {
        Self { store }
    }
}

impl ProviderTerminalInventoryAuthority for ProviderSettingsInventoryAuthority {
    fn current(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderTerminalInventory, ()>> + Send + '_>> {
        Box::pin(async move {
            self.store
                .get()
                .await
                .map(|settings| ProviderTerminalInventory::from_settings(&settings))
                .map_err(|_| ())
        })
    }
}

#[derive(Clone, Debug)]
struct StaticProviderTerminalInventoryAuthority {
    inventory: ProviderTerminalInventory,
}

impl ProviderTerminalInventoryAuthority for StaticProviderTerminalInventoryAuthority {
    fn current(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderTerminalInventory, ()>> + Send + '_>> {
        Box::pin(async move { Ok(self.inventory.clone()) })
    }
}

impl ProviderTerminalInventory {
    pub fn from_settings(settings: &ProviderSettingsState) -> Self {
        let mut entries = BTreeMap::new();
        for (instance_id, driver_kind, enabled, configured_binary, built_in_binary) in [
            (
                "codex",
                "codex",
                settings.providers.codex.enabled,
                settings.providers.codex.binary_path.as_str(),
                "codex",
            ),
            (
                "claudeAgent",
                "claudeAgent",
                settings.providers.claude_agent.enabled,
                settings.providers.claude_agent.binary_path.as_str(),
                "claude",
            ),
            (
                "opencode",
                "opencode",
                settings.providers.opencode.enabled,
                settings.providers.opencode.binary_path.as_str(),
                "opencode",
            ),
        ] {
            entries.insert(
                instance_id.to_owned(),
                ProviderTerminalInventoryEntry {
                    instance_id: instance_id.to_owned(),
                    driver_kind: driver_kind.to_owned(),
                    enabled,
                    configured_binary: configured_binary.to_owned(),
                    built_in_binary: Some(built_in_binary.to_owned()),
                },
            );
        }
        for (instance_id, instance) in &settings.provider_instances {
            let configured_binary =
                configured_instance_binary(settings, instance).unwrap_or_default();
            entries.insert(
                instance_id.clone(),
                ProviderTerminalInventoryEntry {
                    instance_id: instance_id.clone(),
                    driver_kind: instance.driver.clone(),
                    enabled: instance.enabled,
                    configured_binary,
                    built_in_binary: built_in_binary(&instance.driver).map(str::to_owned),
                },
            );
        }
        Self { entries }
    }

    pub fn get(&self, instance_id: &str) -> Option<&ProviderTerminalInventoryEntry> {
        self.entries.get(instance_id)
    }
}

fn configured_instance_binary(
    settings: &ProviderSettingsState,
    instance: &ProviderInstanceState,
) -> Option<String> {
    let configured = instance
        .config
        .get("binaryPath")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    configured.map(str::to_owned).or_else(|| {
        let binary = match instance.driver.as_str() {
            "codex" => &settings.providers.codex.binary_path,
            "claudeAgent" => &settings.providers.claude_agent.binary_path,
            "opencode" => &settings.providers.opencode.binary_path,
            _ => return None,
        };
        (!binary.trim().is_empty()).then(|| binary.clone())
    })
}

fn built_in_binary(driver_kind: &str) -> Option<&'static str> {
    match driver_kind {
        "codex" => Some("codex"),
        "claudeAgent" => Some("claude"),
        "opencode" => Some("opencode"),
        _ => None,
    }
}

#[derive(Clone)]
pub struct ProviderTerminalObserverFactoryInput {
    pub launch: TerminalLaunchPreparationInput,
    pub activity_publisher: TerminalGenerationActivityPublisher,
    pub process_attribution: ProcessAttributionRegistry,
    pub runtime_dir: PathBuf,
}

impl fmt::Debug for ProviderTerminalObserverFactoryInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderTerminalObserverFactoryInput")
            .field("launch", &self.launch)
            .field("runtime_dir", &self.runtime_dir)
            .finish_non_exhaustive()
    }
}

pub trait ProviderTerminalObserverFactory: Send + Sync {
    fn requires_private_executable_pin(&self) -> bool {
        false
    }

    fn prepare(
        &self,
        input: ProviderTerminalObserverFactoryInput,
    ) -> Pin<Box<dyn Future<Output = Option<PreparedTerminalLaunch>> + Send + '_>>;

    /// See `TerminalLaunchPreparer::native_command`; `executable` is the validated provider
    /// executable.
    fn native_command(
        &self,
        _executable: &Path,
        _args: &[String],
        _launch_env: &BTreeMap<String, String>,
    ) -> Pin<Box<dyn Future<Output = Option<NativeTerminalCommand>> + Send + '_>> {
        Box::pin(std::future::ready(None))
    }

    #[doc(hidden)]
    fn shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::ready(()))
    }
}

#[derive(Clone, Default)]
pub struct ProviderTerminalObserverFactories {
    pub codex: Option<Arc<dyn ProviderTerminalObserverFactory>>,
    pub claude: Option<Arc<dyn ProviderTerminalObserverFactory>>,
    pub opencode: Option<Arc<dyn ProviderTerminalObserverFactory>>,
}

impl fmt::Debug for ProviderTerminalObserverFactories {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderTerminalObserverFactories")
            .field("codex", &self.codex.is_some())
            .field("claude", &self.claude.is_some())
            .field("opencode", &self.opencode.is_some())
            .finish()
    }
}

#[derive(Debug, Default)]
struct WarningState {
    emitted: HashSet<String>,
    saturated: bool,
}

#[derive(Clone)]
pub struct ProviderTerminalActivitySupervisor {
    authority: Arc<dyn ProviderTerminalInventoryAuthority>,
    activity_controller: AgentActivityController,
    activity_projection: ActivityProjection,
    process_attribution: ProcessAttributionRegistry,
    runtime_dir: PathBuf,
    factories: ProviderTerminalObserverFactories,
    warnings: Arc<Mutex<WarningState>>,
    publication_locks: PublicationLocks,
}

type PublicationLocks = Arc<Mutex<BTreeMap<(String, String), Weak<AsyncMutex<()>>>>>;

impl fmt::Debug for ProviderTerminalActivitySupervisor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderTerminalActivitySupervisor")
            .field("runtime_dir", &self.runtime_dir)
            .field("factories", &self.factories)
            .finish_non_exhaustive()
    }
}

impl ProviderTerminalActivitySupervisor {
    pub fn new(
        settings: ProviderSettingsState,
        inventory: ProviderTerminalInventory,
        activity_projection: ActivityProjection,
        process_attribution: ProcessAttributionRegistry,
        runtime_dir: PathBuf,
        factories: ProviderTerminalObserverFactories,
    ) -> Result<Self, String> {
        if inventory != ProviderTerminalInventory::from_settings(&settings) {
            return Err("provider terminal inventory does not match validated settings".to_owned());
        }
        Self::new_with_authority(
            Arc::new(StaticProviderTerminalInventoryAuthority { inventory }),
            activity_projection.agent_activity_controller(),
            activity_projection,
            process_attribution,
            runtime_dir,
            factories,
        )
    }

    pub fn new_with_authority(
        authority: Arc<dyn ProviderTerminalInventoryAuthority>,
        activity_controller: AgentActivityController,
        activity_projection: ActivityProjection,
        process_attribution: ProcessAttributionRegistry,
        runtime_dir: PathBuf,
        factories: ProviderTerminalObserverFactories,
    ) -> Result<Self, String> {
        if !runtime_dir.is_absolute() {
            return Err("provider terminal runtime directory must be absolute".to_owned());
        }
        std::fs::create_dir_all(&runtime_dir).map_err(|error| {
            format!("failed to create provider terminal runtime directory: {error}")
        })?;
        if !std::fs::metadata(&runtime_dir)
            .map_err(|error| {
                format!("failed to inspect provider terminal runtime directory: {error}")
            })?
            .is_dir()
        {
            return Err("provider terminal runtime path is not a directory".to_owned());
        }
        restrict_runtime_directory(&runtime_dir)?;
        let runtime_dir = std::fs::canonicalize(&runtime_dir).unwrap_or(runtime_dir);
        cleanup_stale_owned_generation_directories(&runtime_dir);
        Ok(Self {
            authority,
            activity_controller,
            activity_projection,
            process_attribution,
            runtime_dir,
            factories,
            warnings: Arc::new(Mutex::new(WarningState::default())),
            publication_locks: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    fn factory_for(&self, driver_kind: &str) -> Option<Arc<dyn ProviderTerminalObserverFactory>> {
        match driver_kind {
            "codex" => self.factories.codex.clone(),
            "claudeAgent" => self.factories.claude.clone(),
            "opencode" => self.factories.opencode.clone(),
            _ => None,
        }
    }

    fn publication_lock(&self, input: &TerminalLaunchPreparationInput) -> Arc<AsyncMutex<()>> {
        let key = (
            input.generation.thread_id().to_owned(),
            input.generation.terminal_id().to_owned(),
        );
        let mut locks = self
            .publication_locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
            return lock;
        }
        if locks.len() >= PUBLICATION_LOCK_PRUNE_THRESHOLD {
            locks.retain(|_, lock| lock.strong_count() > 0);
        }
        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(key, Arc::downgrade(&lock));
        lock
    }

    fn warn_once(&self, reason: &'static str, input: &TerminalLaunchPreparationInput) {
        let warning_key = format!(
            "{reason}:{}:{}",
            input.activity.driver_kind, input.activity.provider_instance_id
        );
        let mut warnings = self
            .warnings
            .lock()
            .expect("provider terminal warnings lock");
        if warnings.emitted.contains(&warning_key) {
            return;
        }
        if warnings.emitted.len() >= MAX_OPERATIONAL_WARNINGS {
            if !warnings.saturated {
                warnings.saturated = true;
                tracing::warn!(
                    limit = MAX_OPERATIONAL_WARNINGS,
                    "provider terminal observation warnings reached their bounded limit"
                );
            }
            return;
        }
        warnings.emitted.insert(warning_key);
        tracing::warn!(
            provider = %input.activity.driver_kind,
            strategy = observer_strategy(&input.activity.driver_kind),
            status = reason,
            "provider terminal activity hint was not accepted"
        );
    }

    async fn prepare_inner(
        &self,
        input: TerminalLaunchPreparationInput,
    ) -> TerminalLaunchPreparation {
        let activity_state = self.activity_controller.snapshot();
        if !activity_state.enabled {
            return TerminalLaunchPreparation::PassThrough;
        }
        let observer_generation = input.generation.clone();
        let inventory = match self.authority.current().await {
            Ok(inventory) => inventory,
            Err(()) => {
                self.warn_once("inventory_unavailable", &input);
                return TerminalLaunchPreparation::PassThrough;
            }
        };
        let Some(instance) = inventory.get(&input.activity.provider_instance_id) else {
            self.warn_once("unknown_instance", &input);
            return TerminalLaunchPreparation::PassThrough;
        };
        if !instance.enabled {
            self.warn_once("disabled_instance", &input);
            return TerminalLaunchPreparation::PassThrough;
        }
        if instance.driver_kind != input.activity.driver_kind {
            self.warn_once("driver_mismatch", &input);
            return TerminalLaunchPreparation::PassThrough;
        }
        let Some(validated_executable) = validated_executable(&input, instance) else {
            self.warn_once("executable_mismatch", &input);
            return TerminalLaunchPreparation::PassThrough;
        };
        let Some(factory) = self.factory_for(&instance.driver_kind) else {
            self.warn_once("observer_unavailable", &input);
            return TerminalLaunchPreparation::PassThrough;
        };
        let mut input = input;
        input.executable = validated_executable.to_string_lossy().into_owned();
        let activity_publisher = TerminalGenerationActivityPublisher::new(
            input.generation.clone(),
            self.activity_projection.clone(),
            self.publication_lock(&input),
        );
        let factory_input = ProviderTerminalObserverFactoryInput {
            launch: input,
            activity_publisher,
            process_attribution: self.process_attribution.clone(),
            runtime_dir: self.runtime_dir.clone(),
        };
        let requires_private_executable_pin = factory.requires_private_executable_pin();
        let preparation = match factory.prepare(factory_input).await {
            None => return TerminalLaunchPreparation::PassThrough,
            Some(prepared)
                if requires_private_executable_pin
                    && private_pinned_executable(
                        Path::new(&prepared.executable),
                        &self.runtime_dir,
                    ) =>
            {
                prepared
            }
            Some(_) if requires_private_executable_pin => {
                return TerminalLaunchPreparation::PassThrough;
            }
            Some(mut prepared) => {
                prepared.executable = validated_executable.to_string_lossy().into_owned();
                prepared
            }
        };
        let activity_admission = self.activity_controller.admit().filter(|admission| {
            self.activity_controller.snapshot() == activity_state && admission.is_current()
        });
        let Some(activity_admission) = activity_admission else {
            drop(preparation);
            observer_generation
                .request_cancellation(TerminalObserverCancellationReason::PreparationRejected);
            observer_generation
                .shutdown_workers(
                    RACED_PREPARATION_WORKER_SHUTDOWN_TIMEOUT,
                    RACED_PREPARATION_WORKER_ABORT_TIMEOUT,
                )
                .await;
            return TerminalLaunchPreparation::PassThrough;
        };
        TerminalLaunchPreparation::Admitted(preparation, activity_admission)
    }
}

fn observer_strategy(driver_kind: &str) -> &'static str {
    match driver_kind {
        "codex" => "remote-app-server",
        "claudeAgent" => "authenticated-http-hooks",
        "opencode" => "authenticated-serve-attach",
        _ => "unsupported",
    }
}

#[cfg(unix)]
pub(crate) fn create_owned_generation_directory(
    runtime_dir: &Path,
    generation_name: &str,
) -> std::io::Result<PathBuf> {
    create_owned_generation_directory_unix(runtime_dir, generation_name)
}

#[cfg(unix)]
fn create_owned_generation_directory_unix(
    runtime_dir: &Path,
    generation_name: &str,
) -> std::io::Result<PathBuf> {
    let family = managed_generation_family(generation_name)?;
    let runtime = open_owned_directory(runtime_dir)?;
    if !is_private_current_user_directory(&runtime) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "provider terminal runtime directory is not private",
        ));
    }

    // Prefer a slot name that has never existed. This preserves immediate
    // restart path identity while the finite namespace still has capacity.
    for slot in 0..MANAGED_GENERATION_SLOT_COUNT {
        let slot_name = managed_generation_slot_name(family, slot);
        let slot_name_c =
            std::ffi::CString::new(slot_name.as_bytes()).map_err(std::io::Error::other)?;
        // SAFETY: `runtime` is an anchored private directory, `slot_name_c` is
        // one exact direct managed name, and mode 0700 grants no group/other
        // access. EEXIST is handled without following the existing entry.
        let created = unsafe { libc::mkdirat(runtime.as_raw_fd(), slot_name_c.as_ptr(), 0o700) };
        if created != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EEXIST) {
                continue;
            }
            return Err(error);
        }
        let Ok(slot_directory) = open_owned_directory_at(&runtime, &slot_name_c) else {
            continue;
        };
        use std::os::unix::fs::PermissionsExt as _;
        if slot_directory
            .set_permissions(std::fs::Permissions::from_mode(0o700))
            .is_ok()
            && is_private_current_user_directory(&slot_directory)
            && claim_empty_generation_slot(&runtime, &slot_name_c, &slot_directory)
        {
            return Ok(runtime_dir.join(slot_name));
        }
    }

    // Every slot name exists. Inspect exactly this finite namespace; unrelated
    // runtime entries can neither consume scan budget nor force UUID growth.
    let mut retired = Vec::new();
    for slot in 0..MANAGED_GENERATION_SLOT_COUNT {
        let slot_name = managed_generation_slot_name(family, slot);
        let Ok(slot_name_c) = std::ffi::CString::new(slot_name.as_bytes()) else {
            continue;
        };
        let Ok(slot_directory) = open_owned_directory_at(&runtime, &slot_name_c) else {
            continue;
        };
        if !is_private_current_user_directory(&slot_directory)
            || !list_owned_directory_entries(&slot_directory)
                .is_ok_and(|entries| entries.is_completely_empty())
        {
            continue;
        }
        let modified = slot_directory
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        retired.push((modified, slot_name, slot_name_c, slot_directory));
    }
    retired.sort_by_key(|(modified, ..)| *modified);
    for (_, slot_name, slot_name_c, slot_directory) in retired {
        if claim_empty_generation_slot(&runtime, &slot_name_c, &slot_directory) {
            return Ok(runtime_dir.join(slot_name));
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        "all provider terminal generation slots are active or ambiguous",
    ))
}

#[cfg(unix)]
fn is_managed_generation_name(name: &[u8]) -> bool {
    name.len() == 17
        && matches!(name.first(), Some(b'c' | b'h'))
        && name[1..]
            .iter()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(unix)]
fn managed_generation_family(generation_name: &str) -> std::io::Result<u8> {
    let name = generation_name.as_bytes();
    if !is_managed_generation_name(name) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "provider terminal generation name is invalid",
        ));
    }
    Ok(name[0])
}

#[cfg(unix)]
fn managed_generation_slot_name(family: u8, slot: usize) -> String {
    format!("{}{slot:016x}", char::from(family))
}

#[cfg(unix)]
fn claim_empty_generation_slot(
    runtime: &std::fs::File,
    slot_name: &std::ffi::CStr,
    slot_directory: &std::fs::File,
) -> bool {
    claim_empty_generation_slot_with_phase(runtime, slot_name, slot_directory, |_| {})
}

#[cfg(unix)]
fn claim_empty_generation_slot_with_phase(
    runtime: &std::fs::File,
    slot_name: &std::ffi::CStr,
    slot_directory: &std::fs::File,
    mut phase: impl FnMut(ManagedGenerationSlotPhase),
) -> bool {
    phase(ManagedGenerationSlotPhase::ClaimBeforeLock);
    let _operation_guard = lock_managed_generation_slot_operations();
    if !list_owned_directory_entries(slot_directory)
        .is_ok_and(|entries| entries.is_completely_empty())
        || write_ownership_marker_at(slot_directory).is_err()
    {
        return false;
    }
    phase(ManagedGenerationSlotPhase::ClaimMarkerWritten);

    has_valid_ownership_marker_at(slot_directory)
        && list_owned_directory_entries(slot_directory).is_ok_and(|entries| {
            !entries.inspection_failed
                && !entries.has_nested_directories
                && entries.non_directories.len() == 1
                && entries.non_directories[0].as_bytes() == OWNERSHIP_MARKER_NAME.as_bytes()
        })
        && open_name_still_references_directory(runtime, slot_name, slot_directory)
}

#[cfg(unix)]
fn write_ownership_marker_at(generation: &std::fs::File) -> std::io::Result<()> {
    use std::os::fd::FromRawFd;

    let marker_name =
        std::ffi::CString::new(OWNERSHIP_MARKER_NAME).map_err(std::io::Error::other)?;
    // SAFETY: `generation` is a live no-follow directory descriptor and the
    // static marker name is a direct child. O_EXCL is the atomic claim that
    // prevents two creators from reusing the same retired directory.
    let descriptor = unsafe {
        libc::openat(
            generation.as_raw_fd(),
            marker_name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `descriptor` is newly returned by `openat` and uniquely owned.
    let mut marker = unsafe { std::fs::File::from_raw_fd(descriptor) };
    marker
        .write_all(OWNERSHIP_MARKER_CONTENT)
        .and_then(|()| marker.sync_all())?;
    Ok(())
}

#[cfg(all(test, unix))]
fn remove_ownership_marker_at(generation: &std::fs::File) {
    let Ok(marker_name) = std::ffi::CString::new(OWNERSHIP_MARKER_NAME) else {
        return;
    };
    // SAFETY: the marker name is a direct child of the anchored generation
    // descriptor. `unlinkat` removes the entry itself without following it.
    unsafe {
        libc::unlinkat(generation.as_raw_fd(), marker_name.as_ptr(), 0);
    }
}

#[cfg(unix)]
fn open_name_still_references_directory(
    runtime: &std::fs::File,
    name: &std::ffi::CStr,
    directory: &std::fs::File,
) -> bool {
    use std::os::unix::fs::MetadataExt;

    let Ok(open_metadata) = directory.metadata() else {
        return false;
    };
    let mut named_metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `runtime` is live, `name` is a direct NUL-terminated child, and
    // `named_metadata` is valid output storage. Symlinks are never followed.
    let status = unsafe {
        libc::fstatat(
            runtime.as_raw_fd(),
            name.as_ptr(),
            named_metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if status != 0 {
        return false;
    }
    // SAFETY: successful `fstatat` initialized `named_metadata`.
    let named_metadata = unsafe { named_metadata.assume_init() };
    // `dev_t` is narrower than `u64` on macOS but already `u64` on Linux.
    #[allow(clippy::unnecessary_cast)]
    let same_device = open_metadata.dev() == named_metadata.st_dev as u64;
    named_metadata.st_mode & libc::S_IFMT == libc::S_IFDIR
        && same_device
        && open_metadata.ino() == named_metadata.st_ino
}

pub(crate) fn cleanup_owned_generation_directory(
    runtime_dir: &Path,
    generation_dir: &Path,
) -> bool {
    #[cfg(unix)]
    {
        cleanup_owned_generation_directory_unix(runtime_dir, generation_dir, |_| {})
    }
    #[cfg(not(unix))]
    {
        let _ = (runtime_dir, generation_dir);
        false
    }
}

#[cfg(unix)]
fn cleanup_owned_generation_directory_unix(
    runtime_dir: &Path,
    generation_dir: &Path,
    mut phase: impl FnMut(ManagedGenerationSlotPhase),
) -> bool {
    let _operation_guard = lock_managed_generation_slot_operations();
    if generation_dir.parent() != Some(runtime_dir)
        || !std::fs::canonicalize(runtime_dir).is_ok_and(|canonical| canonical == runtime_dir)
    {
        return false;
    }
    let Some(generation_name) = generation_dir.file_name() else {
        return false;
    };
    let Ok(generation_name) = std::ffi::CString::new(generation_name.as_bytes()) else {
        return false;
    };
    let Ok(runtime) = open_owned_directory(runtime_dir) else {
        return false;
    };
    if !is_private_current_user_directory(&runtime) {
        return false;
    }
    let Ok(generation) = open_owned_directory_at(&runtime, &generation_name) else {
        return false;
    };
    if !is_private_current_user_directory(&generation)
        || !has_valid_ownership_marker_at(&generation)
    {
        return false;
    }
    phase(ManagedGenerationSlotPhase::CleanupAfterValidation);
    let Ok(entries) = list_owned_directory_entries(&generation) else {
        return false;
    };
    let mut removal_failed = false;
    for entry in &entries.non_directories {
        // SAFETY: `generation` is an open descriptor for the validated
        // generation inode and `entry` is a NUL-terminated direct child name
        // returned by that descriptor. `unlinkat` removes the entry itself and
        // never follows a symlink child.
        let result = unsafe { libc::unlinkat(generation.as_raw_fd(), entry.as_ptr(), 0) };
        if result != 0 {
            removal_failed = true;
        }
    }
    phase(ManagedGenerationSlotPhase::CleanupAfterContentsRemoved);
    // POSIX has no portable operation that removes an open directory by
    // descriptor. Removing `generation_name` here would race a same-name
    // replacement after validation, so cleanup deliberately retains the empty
    // validated inode. The finite managed slot allocator reclaims only slots
    // whose descriptor-relative post-scan proves them completely empty.
    !removal_failed
        && list_owned_directory_entries(&generation).is_ok_and(|remaining| {
            !remaining.inspection_failed && remaining.non_directories.is_empty()
        })
}

#[cfg(all(test, unix))]
fn cleanup_owned_generation_directory_after_validation(
    runtime_dir: &Path,
    generation_dir: &Path,
    mut after_validation: impl FnMut(),
) -> bool {
    cleanup_owned_generation_directory_unix(runtime_dir, generation_dir, |phase| {
        if phase == ManagedGenerationSlotPhase::CleanupAfterValidation {
            after_validation();
        }
    })
}

#[cfg(all(test, unix))]
fn cleanup_owned_generation_directory_after_contents_removed(
    runtime_dir: &Path,
    generation_dir: &Path,
    mut after_contents_removed: impl FnMut(),
) -> bool {
    cleanup_owned_generation_directory_unix(runtime_dir, generation_dir, |phase| {
        if phase == ManagedGenerationSlotPhase::CleanupAfterContentsRemoved {
            after_contents_removed();
        }
    })
}

#[cfg(unix)]
fn open_owned_directory(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(unix)]
fn open_owned_directory_at(
    parent: &std::fs::File,
    name: &std::ffi::CStr,
) -> std::io::Result<std::fs::File> {
    use std::os::fd::FromRawFd;

    // SAFETY: `parent` is a live directory descriptor and `name` is a
    // NUL-terminated direct child name. The flags reject symlinks and require a
    // directory; ownership of a successful descriptor transfers to `File`.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `descriptor` is newly returned by `openat` and uniquely owned.
    Ok(unsafe { std::fs::File::from_raw_fd(descriptor) })
}

#[cfg(unix)]
fn is_private_current_user_directory(directory: &std::fs::File) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let Ok(metadata) = directory.metadata() else {
        return false;
    };
    metadata.is_dir()
        && metadata.uid() == unsafe { libc::geteuid() }
        && metadata.permissions().mode() & 0o777 == 0o700
}

#[cfg(unix)]
fn has_valid_ownership_marker_at(generation: &std::fs::File) -> bool {
    use std::{io::Read as _, os::fd::FromRawFd};

    let Ok(marker_name) = std::ffi::CString::new(OWNERSHIP_MARKER_NAME) else {
        return false;
    };
    // SAFETY: `generation` is a live validated directory descriptor and the
    // static marker name is NUL-terminated. `O_NOFOLLOW` rejects a marker
    // symlink and ownership of a successful descriptor transfers to `File`.
    let descriptor = unsafe {
        libc::openat(
            generation.as_raw_fd(),
            marker_name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if descriptor < 0 {
        return false;
    }
    // SAFETY: `descriptor` is newly returned by `openat` and uniquely owned.
    let marker = unsafe { std::fs::File::from_raw_fd(descriptor) };
    let Ok(metadata) = marker.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    let mut content = Vec::with_capacity(64);
    marker.take(65).read_to_end(&mut content).is_ok() && content == OWNERSHIP_MARKER_CONTENT
}

#[cfg(unix)]
struct OwnedDirectoryEntries {
    non_directories: Vec<std::ffi::CString>,
    has_nested_directories: bool,
    inspection_failed: bool,
}

#[cfg(unix)]
impl OwnedDirectoryEntries {
    fn is_completely_empty(&self) -> bool {
        self.non_directories.is_empty() && !self.has_nested_directories && !self.inspection_failed
    }
}

#[cfg(unix)]
fn list_owned_directory_entries(
    directory: &std::fs::File,
) -> std::io::Result<OwnedDirectoryEntries> {
    use std::os::fd::FromRawFd;

    // SAFETY: `directory` is a live anchored descriptor and `.` is a
    // NUL-terminated self-reference. A fresh open file description keeps each
    // enumeration's directory offset independent; `dup` would share it.
    let duplicate = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        )
    };
    if duplicate < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `duplicate` is a live directory descriptor. `fdopendir` assumes
    // ownership on success.
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        // SAFETY: ownership was not transferred because `fdopendir` failed.
        unsafe {
            drop(std::fs::File::from_raw_fd(duplicate));
        }
        return Err(std::io::Error::last_os_error());
    }
    let stream = OwnedDirectoryStream(stream);
    let mut non_directories = Vec::new();
    let mut has_nested_directories = false;
    let mut inspection_failed = false;
    loop {
        let mut entry = std::mem::MaybeUninit::<libc::dirent>::uninit();
        let mut result = std::ptr::null_mut();
        // SAFETY: the stream is live, `entry` has enough storage for one
        // `dirent`, and `result` is a valid output pointer.
        #[allow(deprecated)]
        let error = unsafe { libc::readdir_r(stream.0, entry.as_mut_ptr(), &mut result) };
        if error != 0 {
            return Err(std::io::Error::from_raw_os_error(error));
        }
        if result.is_null() {
            break;
        }
        // SAFETY: a non-null `result` means `readdir_r` initialized `entry`.
        let entry = unsafe { entry.assume_init() };
        // SAFETY: `d_name` is guaranteed to be NUL-terminated by `readdir_r`.
        let name = unsafe { std::ffi::CStr::from_ptr(entry.d_name.as_ptr()) };
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `directory` is live, `name` is a direct child returned by
        // this descriptor, and `metadata` is valid output storage. No symlink
        // is followed while checking for nested directories.
        let status = unsafe {
            libc::fstatat(
                directory.as_raw_fd(),
                name.as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if status != 0 {
            inspection_failed = true;
            non_directories.push(name.to_owned());
            continue;
        }
        // SAFETY: successful `fstatat` initialized `metadata`.
        let metadata = unsafe { metadata.assume_init() };
        if metadata.st_mode & libc::S_IFMT == libc::S_IFDIR {
            has_nested_directories = true;
            continue;
        }
        non_directories.push(name.to_owned());
    }
    Ok(OwnedDirectoryEntries {
        non_directories,
        has_nested_directories,
        inspection_failed,
    })
}

#[cfg(unix)]
struct OwnedDirectoryStream(*mut libc::DIR);

#[cfg(unix)]
impl Drop for OwnedDirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this wrapper uniquely owns the live stream returned by
        // `fdopendir`.
        unsafe {
            libc::closedir(self.0);
        }
    }
}

fn cleanup_stale_owned_generation_directories(runtime_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(runtime_dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry
            .file_type()
            .is_ok_and(|file_type| file_type.is_dir() && !file_type.is_symlink())
        {
            continue;
        }
        let candidate = entry.path();
        if candidate.parent() != Some(runtime_dir)
            || std::fs::canonicalize(&candidate)
                .ok()
                .and_then(|path| path.parent().map(Path::to_path_buf))
                .as_deref()
                != Some(runtime_dir)
        {
            continue;
        }
        cleanup_owned_generation_directory(runtime_dir, &candidate);
    }
}

#[cfg(unix)]
fn private_pinned_executable(path: &Path, runtime_dir: &Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let Ok(canonical) = std::fs::canonicalize(path) else {
        return false;
    };
    if canonical != path {
        return false;
    }
    let Some(generation_dir) = canonical.parent() else {
        return false;
    };
    if generation_dir.parent() != Some(runtime_dir) {
        return false;
    }
    let Ok(runtime_metadata) = std::fs::metadata(runtime_dir) else {
        return false;
    };
    let Ok(generation_metadata) = std::fs::metadata(generation_dir) else {
        return false;
    };
    let Ok(metadata) = std::fs::metadata(&canonical) else {
        return false;
    };
    // SAFETY: `geteuid` has no preconditions and does not dereference memory.
    let effective_user_id = unsafe { libc::geteuid() };
    runtime_metadata.is_dir()
        && runtime_metadata.permissions().mode() & 0o777 == 0o700
        && runtime_metadata.uid() == effective_user_id
        && generation_metadata.is_dir()
        && generation_metadata.permissions().mode() & 0o777 == 0o700
        && generation_metadata.uid() == effective_user_id
        && metadata.is_file()
        && metadata.permissions().mode() & 0o777 == 0o500
        && metadata.uid() == effective_user_id
        && metadata.nlink() == 1
}

#[cfg(not(unix))]
fn private_pinned_executable(_path: &Path, _runtime_dir: &Path) -> bool {
    false
}

impl TerminalLaunchPreparer for ProviderTerminalActivitySupervisor {
    fn preparation_execution_budget(
        &self,
        input: &TerminalLaunchPreparationInput,
    ) -> Pin<Box<dyn Future<Output = Duration> + Send + '_>> {
        let input = input.clone();
        Box::pin(async move {
            let default = Duration::from_millis(500);
            if !self.activity_controller.snapshot().enabled {
                return default;
            }
            if input.activity.driver_kind != "opencode" {
                return default;
            }
            let Ok(inventory) = self.authority.current().await else {
                return default;
            };
            let Some(instance) = inventory.get(&input.activity.provider_instance_id) else {
                return default;
            };
            if !instance.enabled
                || instance.driver_kind != input.activity.driver_kind
                || validated_executable(&input, instance).is_none()
                || self.factory_for(&instance.driver_kind).is_none()
            {
                return default;
            }
            Duration::from_secs(1)
        })
    }

    fn prepare(
        &self,
        input: TerminalLaunchPreparationInput,
    ) -> Pin<Box<dyn Future<Output = TerminalLaunchPreparation> + Send + '_>> {
        Box::pin(self.prepare_inner(input))
    }

    fn native_command(
        &self,
        input: &TerminalLaunchPreparationInput,
    ) -> Pin<Box<dyn Future<Output = Option<NativeTerminalCommand>> + Send + '_>> {
        let input = input.clone();
        Box::pin(async move {
            // Only OpenCode's command differs by installed version.
            if input.activity.driver_kind != "opencode" {
                return None;
            }
            // Only the configured provider executable is probed, as `prepare` would.
            let inventory = self.authority.current().await.ok()?;
            let instance = inventory.get(&input.activity.provider_instance_id)?;
            if !instance.enabled || instance.driver_kind != input.activity.driver_kind {
                return None;
            }
            let executable = validated_executable(&input, instance)?;
            self.factory_for(&instance.driver_kind)?
                .native_command(&executable, &input.args, &input.launch_env)
                .await
        })
    }

    fn shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            for factory in [
                self.factories.codex.as_ref(),
                self.factories.claude.as_ref(),
                self.factories.opencode.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                factory.shutdown().await;
            }
        })
    }
}

fn validated_executable(
    input: &TerminalLaunchPreparationInput,
    instance: &ProviderTerminalInventoryEntry,
) -> Option<PathBuf> {
    let allowed = if instance.configured_binary.trim().is_empty() {
        instance.built_in_binary.as_deref()?
    } else {
        instance.configured_binary.as_str()
    };
    let requested = resolve_executable(
        &input.executable,
        &input.cwd,
        Some(&input.launch_env),
        false,
    )?;
    let allowed = resolve_executable(allowed, &input.cwd, None, true)?;
    paths_equal(&requested, &allowed).then_some(allowed)
}

fn resolve_executable(
    executable: &str,
    cwd: &Path,
    launch_env: Option<&BTreeMap<String, String>>,
    configured_provider: bool,
) -> Option<PathBuf> {
    let search_path = launch_env
        .and_then(|environment| environment_value(environment, "PATH").map(Into::into))
        .or_else(|| std::env::var_os("PATH"));
    let path_extensions = (Platform::current() == Platform::Windows)
        .then(|| {
            launch_env
                .and_then(|environment| environment_value(environment, "PATHEXT"))
                .map(str::to_owned)
                .or_else(|| std::env::var("PATHEXT").ok())
        })
        .flatten();
    let extensions = launch_executable_extensions(Platform::current(), path_extensions.as_deref());
    #[cfg(feature = "hermetic-test-guard")]
    if configured_provider || crate::hermetic_guard::is_guarded_program(Path::new(executable)) {
        return crate::hermetic_guard::resolve_guarded_executable(
            Path::new(executable),
            Some(cwd),
            search_path.as_deref(),
            &extensions,
            false,
        )
        .and_then(|path| std::fs::canonicalize(path).ok());
    }
    #[cfg(not(feature = "hermetic-test-guard"))]
    let _ = configured_provider;
    let path = locate_executable(executable, Some(cwd), search_path.as_deref(), &extensions)?;
    std::fs::canonicalize(path).ok()
}

fn environment_value<'a>(environment: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    environment
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.as_str())
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    match Platform::current() {
        Platform::Windows => {
            executable_text_equal(&left.to_string_lossy(), &right.to_string_lossy())
        }
        Platform::Unix => left == right,
    }
}

fn executable_text_equal(left: &str, right: &str) -> bool {
    match Platform::current() {
        Platform::Windows => left.eq_ignore_ascii_case(right),
        Platform::Unix => left == right,
    }
}

#[cfg(unix)]
fn restrict_runtime_directory(runtime_dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(runtime_dir)
        .map_err(|error| format!("failed to inspect provider terminal runtime directory: {error}"))?
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(runtime_dir, permissions)
        .map_err(|error| format!("failed to protect provider terminal runtime directory: {error}"))
}

#[cfg(not(unix))]
fn restrict_runtime_directory(_runtime_dir: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {

    #[cfg(feature = "hermetic-test-guard")]
    #[test]
    fn hermetic_guard_refuses_terminal_resolution_before_using_the_candidate() {
        let Some(executable) = crate::hermetic_guard::refusal_fixture(
            "provider_terminal::supervisor::tests::hermetic_guard_refuses_terminal_resolution_before_using_the_candidate",
            "codex",
        ) else {
            return;
        };
        assert!(
            super::resolve_executable(
                executable.to_str().unwrap(),
                executable.parent().unwrap(),
                None,
                true
            )
            .is_none()
        );
        let environment = std::collections::BTreeMap::from([(
            "PATH".to_owned(),
            executable.parent().unwrap().display().to_string(),
        )]);
        assert!(
            super::resolve_executable(
                "codex",
                executable.parent().unwrap(),
                Some(&environment),
                true
            )
            .is_none()
        );
    }

    #[cfg(feature = "hermetic-test-guard")]
    #[test]
    fn hermetic_guard_preserves_ordinary_shells_and_checks_configured_provider_aliases() {
        let Some(executable) = crate::hermetic_guard::refusal_fixture(
            "provider_terminal::supervisor::tests::hermetic_guard_preserves_ordinary_shells_and_checks_configured_provider_aliases",
            "2.1.220",
        ) else {
            return;
        };
        assert!(
            super::resolve_executable("/bin/sh", executable.parent().unwrap(), None, false)
                .is_some()
        );
        assert!(
            super::resolve_executable(
                executable.to_str().unwrap(),
                executable.parent().unwrap(),
                None,
                true
            )
            .is_none()
        );
    }
    use std::os::unix::{ffi::OsStrExt as _, fs::symlink};

    use super::{
        ManagedGenerationSlotPhase, claim_empty_generation_slot_with_phase,
        cleanup_owned_generation_directory_after_contents_removed,
        cleanup_owned_generation_directory_after_validation, create_owned_generation_directory,
        open_owned_directory, open_owned_directory_at, remove_ownership_marker_at,
        write_ownership_marker_at,
    };

    #[test]
    fn cleanup_serializes_a_successor_claim_until_marker_removal_finishes() {
        let fixture = tempfile::tempdir().expect("cleanup claim race fixture");
        let runtime_dir = fixture.path().join("runtime");
        std::fs::create_dir(&runtime_dir).expect("runtime directory");
        let runtime_dir = std::fs::canonicalize(runtime_dir).expect("canonical runtime directory");
        super::restrict_runtime_directory(&runtime_dir).expect("private runtime directory");
        let generation_dir = create_owned_generation_directory(&runtime_dir, "cffffffffffffffff")
            .expect("generation");
        let runtime = open_owned_directory(&runtime_dir).expect("runtime descriptor");
        let generation_name = std::ffi::CString::new(
            generation_dir
                .file_name()
                .expect("generation file name")
                .as_bytes(),
        )
        .expect("generation name");
        let generation =
            open_owned_directory_at(&runtime, &generation_name).expect("generation descriptor");
        let (start_claim, claim_started) = std::sync::mpsc::sync_channel(0);
        let (claim_attempted, cleanup_may_continue) = std::sync::mpsc::sync_channel(0);
        let claim_thread = std::thread::spawn(move || {
            claim_started.recv().expect("start successor claim");
            claim_empty_generation_slot_with_phase(
                &runtime,
                &generation_name,
                &generation,
                |phase| {
                    if phase == ManagedGenerationSlotPhase::ClaimBeforeLock {
                        claim_attempted
                            .send(())
                            .expect("report successor lock attempt");
                    }
                },
            )
        });

        cleanup_owned_generation_directory_after_validation(&runtime_dir, &generation_dir, || {
            start_claim.send(()).expect("release successor claim");
            cleanup_may_continue
                .recv()
                .expect("successor reached operation lock");
        });

        assert!(
            claim_thread.join().expect("successor claim thread"),
            "the successor claim returns only after cleanup finishes"
        );
        assert!(
            generation_dir.join(super::OWNERSHIP_MARKER_NAME).exists(),
            "cleanup cannot delete the serialized successor marker"
        );
        assert_eq!(
            std::fs::read(generation_dir.join(super::OWNERSHIP_MARKER_NAME))
                .expect("successor marker"),
            super::OWNERSHIP_MARKER_CONTENT
        );
    }

    #[test]
    fn ambiguous_claim_does_not_unlink_a_successor_marker() {
        let fixture = tempfile::tempdir().expect("claim rollback race fixture");
        let runtime_dir = fixture.path().join("runtime");
        std::fs::create_dir(&runtime_dir).expect("runtime directory");
        let runtime_dir = std::fs::canonicalize(runtime_dir).expect("canonical runtime directory");
        super::restrict_runtime_directory(&runtime_dir).expect("private runtime directory");
        let generation_dir = create_owned_generation_directory(&runtime_dir, "cffffffffffffffff")
            .expect("generation");
        assert!(super::cleanup_owned_generation_directory(
            &runtime_dir,
            &generation_dir
        ));
        let runtime = open_owned_directory(&runtime_dir).expect("runtime descriptor");
        let generation_name = std::ffi::CString::new(
            generation_dir
                .file_name()
                .expect("generation file name")
                .as_bytes(),
        )
        .expect("generation name");
        let generation =
            open_owned_directory_at(&runtime, &generation_name).expect("generation descriptor");

        let claimed = claim_empty_generation_slot_with_phase(
            &runtime,
            &generation_name,
            &generation,
            |phase| {
                if phase == ManagedGenerationSlotPhase::ClaimMarkerWritten {
                    remove_ownership_marker_at(&generation);
                    write_ownership_marker_at(&generation).expect("successor marker");
                    std::fs::write(generation_dir.join("ambiguous"), b"ambiguous")
                        .expect("force ambiguous claim result");
                }
            },
        );

        assert!(!claimed, "ambiguous claim must fail closed");
        assert_eq!(
            std::fs::read(generation_dir.join(super::OWNERSHIP_MARKER_NAME))
                .expect("successor marker survives"),
            super::OWNERSHIP_MARKER_CONTENT
        );
    }

    #[test]
    fn cleanup_never_follows_a_generation_parent_replaced_after_validation() {
        let fixture = tempfile::tempdir().expect("cleanup race fixture");
        let runtime_dir = fixture.path().join("runtime");
        std::fs::create_dir(&runtime_dir).expect("runtime directory");
        let runtime_dir = std::fs::canonicalize(runtime_dir).expect("canonical runtime directory");
        super::restrict_runtime_directory(&runtime_dir).expect("private runtime directory");
        let generation_dir = create_owned_generation_directory(&runtime_dir, "cffffffffffffffff")
            .expect("generation");
        std::fs::write(generation_dir.join("owned"), b"owned").expect("owned artifact");
        let displaced = runtime_dir.join("displaced");
        let outside = fixture.path().join("outside");
        std::fs::create_dir(&outside).expect("outside directory");
        let outside_file = outside.join("must-survive");
        std::fs::write(&outside_file, b"outside").expect("outside artifact");

        cleanup_owned_generation_directory_after_validation(&runtime_dir, &generation_dir, || {
            std::fs::rename(&generation_dir, &displaced)
                .expect("replace validated generation path");
            symlink(&outside, &generation_dir).expect("outside replacement symlink");
        });

        assert!(
            outside_file.exists(),
            "cleanup must stay anchored to the validated generation inode"
        );
    }

    #[test]
    fn cleanup_never_removes_a_real_directory_replacement_at_final_removal() {
        let fixture = tempfile::tempdir().expect("cleanup final-removal fixture");
        let runtime_dir = fixture.path().join("runtime");
        std::fs::create_dir(&runtime_dir).expect("runtime directory");
        let runtime_dir = std::fs::canonicalize(runtime_dir).expect("canonical runtime directory");
        super::restrict_runtime_directory(&runtime_dir).expect("private runtime directory");
        let generation_dir = create_owned_generation_directory(&runtime_dir, "cffffffffffffffff")
            .expect("generation");
        std::fs::write(generation_dir.join("owned"), b"owned").expect("owned artifact");
        let displaced = runtime_dir.join("validated-generation");

        cleanup_owned_generation_directory_after_contents_removed(
            &runtime_dir,
            &generation_dir,
            || {
                std::fs::rename(&generation_dir, &displaced)
                    .expect("move validated generation inode");
                std::fs::create_dir(&generation_dir)
                    .expect("same-name unvalidated real directory replacement");
            },
        );

        assert!(
            generation_dir.exists(),
            "final cleanup must preserve a same-name inode that was never validated"
        );
        assert!(
            displaced.exists(),
            "the opened validated generation inode remains distinct"
        );
    }

    #[test]
    fn cleaned_generation_directory_is_reclaimed_for_a_later_generation() {
        let fixture = tempfile::tempdir().expect("cleanup reuse fixture");
        let runtime_dir = fixture.path().join("runtime");
        std::fs::create_dir(&runtime_dir).expect("runtime directory");
        let runtime_dir = std::fs::canonicalize(runtime_dir).expect("canonical runtime directory");
        super::restrict_runtime_directory(&runtime_dir).expect("private runtime directory");
        let mut observed_slots = std::collections::HashSet::new();
        for generation_index in 0..(super::MANAGED_GENERATION_SLOT_COUNT + 8) {
            let generation = create_owned_generation_directory(
                &runtime_dir,
                &format!("c{generation_index:016x}"),
            )
            .expect("sequential generation");
            observed_slots.insert(generation.clone());
            assert_eq!(
                std::fs::read(generation.join(super::OWNERSHIP_MARKER_NAME))
                    .expect("claimed ownership marker"),
                super::OWNERSHIP_MARKER_CONTENT
            );
            assert!(
                super::cleanup_owned_generation_directory(&runtime_dir, &generation),
                "sequential cleanup removes every direct artifact"
            );
        }

        assert_eq!(
            observed_slots.len(),
            super::MANAGED_GENERATION_SLOT_COUNT,
            "sequential generations use only the finite managed slot namespace"
        );
        assert_eq!(
            std::fs::read_dir(&runtime_dir)
                .expect("bounded runtime directory")
                .count(),
            super::MANAGED_GENERATION_SLOT_COUNT,
            "sequential generations never grow beyond the managed slot cap"
        );
    }

    #[test]
    fn reclamation_preserves_nonempty_ambiguous_generation_directories() {
        let fixture = tempfile::tempdir().expect("cleanup ambiguity fixture");
        let runtime_dir = fixture.path().join("runtime");
        std::fs::create_dir(&runtime_dir).expect("runtime directory");
        let runtime_dir = std::fs::canonicalize(runtime_dir).expect("canonical runtime directory");
        super::restrict_runtime_directory(&runtime_dir).expect("private runtime directory");
        let ambiguous = runtime_dir.join("c0000000000000001");
        std::fs::create_dir(&ambiguous).expect("ambiguous generation");
        super::restrict_runtime_directory(&ambiguous).expect("private ambiguous generation");
        std::fs::write(ambiguous.join("preserve"), b"not retired").expect("ambiguous payload");

        let claimed = create_owned_generation_directory(&runtime_dir, "c0000000000000002")
            .expect("new generation");

        assert_ne!(
            claimed, ambiguous,
            "nonempty unmarked directories are never reclaimed"
        );
        assert_eq!(
            std::fs::read(ambiguous.join("preserve")).expect("preserved ambiguous payload"),
            b"not retired"
        );
    }

    #[test]
    fn unrelated_entries_cannot_starve_bounded_generation_slot_reuse() {
        let fixture = tempfile::tempdir().expect("bounded slot fixture");
        let runtime_dir = fixture.path().join("runtime");
        std::fs::create_dir(&runtime_dir).expect("runtime directory");
        let runtime_dir = std::fs::canonicalize(runtime_dir).expect("canonical runtime directory");
        super::restrict_runtime_directory(&runtime_dir).expect("private runtime directory");
        for index in 0..1024 {
            std::fs::write(
                runtime_dir.join(format!("unrelated-{index:04}")),
                b"preserve",
            )
            .expect("unrelated entry");
        }

        for index in 0..(super::MANAGED_GENERATION_SLOT_COUNT + 8) {
            let generation =
                create_owned_generation_directory(&runtime_dir, &format!("c{index:016x}"))
                    .expect("bounded sequential generation");
            super::cleanup_owned_generation_directory(&runtime_dir, &generation);
        }

        let managed_count = std::fs::read_dir(&runtime_dir)
            .expect("runtime entries")
            .flatten()
            .filter(|entry| super::is_managed_generation_name(entry.file_name().as_encoded_bytes()))
            .count();
        assert!(
            managed_count <= super::MANAGED_GENERATION_SLOT_COUNT,
            "unrelated entries must not cause sequential launches to grow beyond the managed cap: \
             {managed_count}"
        );
    }

    #[test]
    fn cleanup_preserves_nested_directories_but_removes_direct_sensitive_artifacts() {
        let fixture = tempfile::tempdir().expect("nested cleanup fixture");
        let runtime_dir = fixture.path().join("runtime");
        std::fs::create_dir(&runtime_dir).expect("runtime directory");
        let runtime_dir = std::fs::canonicalize(runtime_dir).expect("canonical runtime directory");
        super::restrict_runtime_directory(&runtime_dir).expect("private runtime directory");
        let generation_dir = create_owned_generation_directory(&runtime_dir, "c0000000000000001")
            .expect("generation");
        let secret = generation_dir.join("credentials.json");
        std::fs::write(&secret, b"secret").expect("direct secret");
        let nested = generation_dir.join("preserve-nested");
        std::fs::create_dir(&nested).expect("nested directory");
        std::fs::write(nested.join("preserve"), b"nested").expect("nested payload");

        let cleaned = super::cleanup_owned_generation_directory(&runtime_dir, &generation_dir);

        assert!(cleaned, "all direct artifacts were removed");
        assert!(!secret.exists(), "direct secret must be removed");
        assert!(
            !generation_dir.join(super::OWNERSHIP_MARKER_NAME).exists(),
            "direct ownership marker must be removed"
        );
        assert!(nested.exists(), "nested real directory is preserved");
        assert_eq!(
            std::fs::read(nested.join("preserve")).expect("nested payload"),
            b"nested"
        );
    }

    #[test]
    fn cleanup_reports_failure_while_direct_artifacts_remain() {
        use std::os::unix::fs::PermissionsExt as _;

        let fixture = tempfile::tempdir().expect("failed cleanup fixture");
        let runtime_dir = fixture.path().join("runtime");
        std::fs::create_dir(&runtime_dir).expect("runtime directory");
        let runtime_dir = std::fs::canonicalize(runtime_dir).expect("canonical runtime directory");
        super::restrict_runtime_directory(&runtime_dir).expect("private runtime directory");
        let generation_dir = create_owned_generation_directory(&runtime_dir, "c0000000000000001")
            .expect("generation");
        let secret = generation_dir.join("credentials.json");
        std::fs::write(&secret, b"secret").expect("direct secret");

        let cleaned = cleanup_owned_generation_directory_after_validation(
            &runtime_dir,
            &generation_dir,
            || {
                std::fs::set_permissions(&generation_dir, std::fs::Permissions::from_mode(0o500))
                    .expect("make direct unlink fail");
            },
        );

        assert!(
            !cleaned,
            "remaining direct artifacts make cleanup incomplete"
        );
        assert!(secret.exists(), "failed direct unlink remains observable");
        assert!(
            generation_dir.join(super::OWNERSHIP_MARKER_NAME).exists(),
            "failed marker unlink remains observable"
        );
        std::fs::set_permissions(&generation_dir, std::fs::Permissions::from_mode(0o700))
            .expect("restore generation permissions");
        assert!(
            super::cleanup_owned_generation_directory(&runtime_dir, &generation_dir),
            "retry after permissions recover removes every direct artifact"
        );
    }

    #[test]
    fn exhausted_ambiguous_generation_slots_fail_closed_to_observation() {
        let fixture = tempfile::tempdir().expect("slot exhaustion fixture");
        let runtime_dir = fixture.path().join("runtime");
        std::fs::create_dir(&runtime_dir).expect("runtime directory");
        let runtime_dir = std::fs::canonicalize(runtime_dir).expect("canonical runtime directory");
        super::restrict_runtime_directory(&runtime_dir).expect("private runtime directory");
        for slot in 0..super::MANAGED_GENERATION_SLOT_COUNT {
            let ambiguous = runtime_dir.join(format!("c{slot:016x}"));
            std::fs::create_dir(&ambiguous).expect("ambiguous slot");
            super::restrict_runtime_directory(&ambiguous).expect("private ambiguous slot");
            std::fs::write(ambiguous.join("preserve"), b"ambiguous").expect("ambiguous payload");
        }

        let result = create_owned_generation_directory(&runtime_dir, "cffffffffffffffff");

        assert!(
            result.is_err(),
            "slot exhaustion must fail observation instead of creating an unbounded slot"
        );
    }
}
