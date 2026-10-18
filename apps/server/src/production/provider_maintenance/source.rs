use std::{
    ffi::OsStr,
    io::ErrorKind,
    path::{Path, PathBuf},
    time::Duration,
};

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::time::timeout;

use crate::provider::environment::{claude_config_directory, effective_environment_value};

use super::{
    ProviderMaintenanceTarget,
    latest::{ClaudeReleaseChannel, LatestVersionSource, VersionScheme},
};

const CLAUDE_LOCAL_FILE_LIMIT: u64 = 64 * 1024;
const CLAUDE_MANAGED_ENTRY_LIMIT: usize = 64;
const CLAUDE_MANAGED_AGGREGATE_LIMIT: usize = CLAUDE_LOCAL_FILE_LIMIT as usize * 4;
const CLAUDE_LOCAL_IO_TIMEOUT: Duration = Duration::from_secs(1);

enum LocalEvidenceRead {
    Missing,
    Document(Vec<u8>),
    Invalid,
}

enum ManagedDirectoryRead {
    Missing,
    Documents(Vec<Vec<u8>>),
    Invalid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LinuxPackageManager {
    Apt,
    Dnf,
    Apk,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ClaudeSourceHints {
    channel: Option<ClaudeReleaseChannel>,
    updates_disabled: bool,
    linux_package: Option<LinuxPackageManager>,
    invalid_managed_channel: bool,
}

impl Default for ClaudeSourceHints {
    fn default() -> Self {
        Self {
            channel: Some(ClaudeReleaseChannel::Latest),
            updates_disabled: false,
            linux_package: None,
            invalid_managed_channel: false,
        }
    }
}

fn claude_hints_from_documents<'a>(
    user: Option<&'a [u8]>,
    managed: impl IntoIterator<Item = &'a [u8]>,
) -> ClaudeSourceHints {
    let mut hints = ClaudeSourceHints::default();
    if let Some(document) = user
        && let Ok(document) = serde_json::from_slice::<serde_json::Value>(document)
    {
        merge_claude_document(&mut hints, &document, false);
    }
    for document in managed {
        if let Ok(document) = serde_json::from_slice::<serde_json::Value>(document) {
            merge_claude_document(&mut hints, &document, true);
        }
    }
    hints
}

fn merge_claude_document(
    hints: &mut ClaudeSourceHints,
    document: &serde_json::Value,
    managed: bool,
) {
    if let Some(value) = document.get("autoUpdatesChannel") {
        match value.as_str() {
            Some("stable") => {
                hints.channel = Some(ClaudeReleaseChannel::Stable);
                if managed {
                    hints.invalid_managed_channel = false;
                }
            }
            Some("latest") => {
                hints.channel = Some(ClaudeReleaseChannel::Latest);
                if managed {
                    hints.invalid_managed_channel = false;
                }
            }
            _ if managed => {
                hints.channel = None;
                hints.invalid_managed_channel = true;
            }
            _ => {}
        }
    }
    if let Some(value) = document
        .get("env")
        .and_then(|environment| environment.get("DISABLE_UPDATES"))
        .filter(|value| !value.is_array() && !value.is_object() && !value.is_null())
    {
        hints.updates_disabled = value.as_str() == Some("1");
    }
}

fn parse_claude_repository_marker(
    document: &str,
) -> Option<(LinuxPackageManager, ClaudeReleaseChannel)> {
    let mut marker = None;
    for token in document
        .lines()
        .map(|line| line.split('#').next().unwrap_or_default())
        .flat_map(str::split_whitespace)
    {
        let token = token
            .strip_prefix("baseurl=")
            .unwrap_or(token)
            .trim_matches(['\'', '"']);
        let candidate = match token {
            "https://downloads.claude.ai/claude-code/apt/stable" => {
                Some((LinuxPackageManager::Apt, ClaudeReleaseChannel::Stable))
            }
            "https://downloads.claude.ai/claude-code/apt/latest" => {
                Some((LinuxPackageManager::Apt, ClaudeReleaseChannel::Latest))
            }
            "https://downloads.claude.ai/claude-code/rpm/stable" => {
                Some((LinuxPackageManager::Dnf, ClaudeReleaseChannel::Stable))
            }
            "https://downloads.claude.ai/claude-code/rpm/latest" => {
                Some((LinuxPackageManager::Dnf, ClaudeReleaseChannel::Latest))
            }
            "https://downloads.claude.ai/claude-code/apk/stable" => {
                Some((LinuxPackageManager::Apk, ClaudeReleaseChannel::Stable))
            }
            "https://downloads.claude.ai/claude-code/apk/latest" => {
                Some((LinuxPackageManager::Apk, ClaudeReleaseChannel::Latest))
            }
            _ => None,
        };
        if let Some(candidate) = candidate {
            match marker {
                Some(previous) if previous != candidate => return None,
                Some(_) => {}
                None => marker = Some(candidate),
            }
        }
    }
    marker
}

pub(crate) async fn discover_claude_hints(
    target: &ProviderMaintenanceTarget,
    resolved: Option<&Path>,
) -> ClaudeSourceHints {
    let managed_settings = claude_managed_settings_path(target);
    let managed_directory = managed_settings
        .as_deref()
        .map(|path| path.with_file_name("managed-settings.d"));
    let repository_paths = [
        PathBuf::from("/etc/apt/sources.list.d/claude-code.list"),
        PathBuf::from("/etc/yum.repos.d/claude-code.repo"),
        PathBuf::from("/etc/apk/repositories"),
    ];
    discover_claude_hints_from_paths(
        target,
        resolved,
        managed_settings.as_deref(),
        managed_directory.as_deref(),
        &repository_paths,
    )
    .await
}

async fn discover_claude_hints_from_paths(
    target: &ProviderMaintenanceTarget,
    resolved: Option<&Path>,
    managed_settings: Option<&Path>,
    managed_directory: Option<&Path>,
    repository_paths: &[PathBuf],
) -> ClaudeSourceHints {
    let user_settings =
        claude_config_directory(&target.environment).map(|path| path.join("settings.json"));
    let (user_document, mut invalid_local_evidence) = match user_settings.as_deref() {
        Some(path) => match read_bounded_local_file(path).await {
            LocalEvidenceRead::Document(document) => (Some(document), false),
            LocalEvidenceRead::Missing => (None, false),
            LocalEvidenceRead::Invalid => (None, true),
        },
        None => (None, false),
    };
    let mut managed_documents = Vec::new();
    if let Some(path) = managed_settings {
        match read_bounded_local_file(path).await {
            LocalEvidenceRead::Document(document) => managed_documents.push(document),
            LocalEvidenceRead::Missing => {}
            LocalEvidenceRead::Invalid => invalid_local_evidence = true,
        }
    }
    if let Some(directory) = managed_directory {
        match read_managed_directory(directory).await {
            ManagedDirectoryRead::Documents(documents) => managed_documents.extend(documents),
            ManagedDirectoryRead::Missing => {}
            ManagedDirectoryRead::Invalid => invalid_local_evidence = true,
        }
    }
    if managed_documents
        .iter()
        .try_fold(0usize, |total, document| total.checked_add(document.len()))
        .is_none_or(|total| total > CLAUDE_MANAGED_AGGREGATE_LIMIT)
    {
        managed_documents.clear();
        invalid_local_evidence = true;
    }
    let mut hints = claude_hints_from_documents(
        user_document.as_deref(),
        managed_documents.iter().map(Vec::as_slice),
    );
    if invalid_local_evidence {
        hints.channel = None;
        hints.invalid_managed_channel = true;
    }
    hints.updates_disabled = effective_environment_value(&target.environment, "DISABLE_UPDATES")
        .as_deref()
        .is_some_and(|value| value == OsStr::new("1"))
        || hints.updates_disabled;

    if resolved.map(normalized_path).as_deref() == Some("/usr/bin/claude") {
        let mut repository_documents = String::new();
        let mut invalid_repository_evidence = false;
        for path in repository_paths {
            match read_bounded_local_file(path).await {
                LocalEvidenceRead::Document(document) => {
                    if let Ok(document) = std::str::from_utf8(&document) {
                        repository_documents.push_str(document);
                        repository_documents.push('\n');
                    }
                }
                LocalEvidenceRead::Missing => {}
                LocalEvidenceRead::Invalid => invalid_repository_evidence = true,
            }
        }
        if !invalid_repository_evidence
            && let Some((manager, channel)) = parse_claude_repository_marker(&repository_documents)
        {
            hints.linux_package = Some(manager);
            hints.channel = Some(channel);
        } else {
            hints.linux_package = None;
            hints.channel = None;
        }
    }
    hints
}

async fn read_managed_directory(path: &Path) -> ManagedDirectoryRead {
    match timeout(CLAUDE_LOCAL_IO_TIMEOUT, read_managed_directory_inner(path)).await {
        Ok(result) => result,
        Err(_) => ManagedDirectoryRead::Invalid,
    }
}

async fn read_managed_directory_inner(path: &Path) -> ManagedDirectoryRead {
    let initial_metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_dir() => metadata,
        Ok(_) => return ManagedDirectoryRead::Invalid,
        Err(error) if error.kind() == ErrorKind::NotFound => return ManagedDirectoryRead::Missing,
        Err(_) => return ManagedDirectoryRead::Invalid,
    };
    let mut entries = match tokio::fs::read_dir(path).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return ManagedDirectoryRead::Invalid,
        Err(_) => return ManagedDirectoryRead::Invalid,
    };
    let mut entry_count = 0usize;
    let mut paths = Vec::new();
    loop {
        match entries.next_entry().await {
            Ok(Some(entry)) => {
                entry_count += 1;
                if entry_count > CLAUDE_MANAGED_ENTRY_LIMIT {
                    return ManagedDirectoryRead::Invalid;
                }
                let path = entry.path();
                if path.extension() == Some(OsStr::new("json")) {
                    paths.push(path);
                }
            }
            Ok(None) => break,
            Err(_) => return ManagedDirectoryRead::Invalid,
        }
    }
    let final_metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(_) => return ManagedDirectoryRead::Invalid,
    };
    if !local_metadata_matches(&initial_metadata, &final_metadata) {
        return ManagedDirectoryRead::Invalid;
    }
    paths.sort();
    let mut documents = Vec::with_capacity(paths.len());
    let mut total_bytes = 0usize;
    for path in paths {
        let document = match read_bounded_local_file_inner(&path).await {
            LocalEvidenceRead::Document(document) => document,
            LocalEvidenceRead::Missing | LocalEvidenceRead::Invalid => {
                return ManagedDirectoryRead::Invalid;
            }
        };
        total_bytes = match total_bytes.checked_add(document.len()) {
            Some(total) if total <= CLAUDE_MANAGED_AGGREGATE_LIMIT => total,
            _ => return ManagedDirectoryRead::Invalid,
        };
        documents.push(document);
    }
    ManagedDirectoryRead::Documents(documents)
}

async fn read_bounded_local_file(path: &Path) -> LocalEvidenceRead {
    match timeout(CLAUDE_LOCAL_IO_TIMEOUT, read_bounded_local_file_inner(path)).await {
        Ok(result) => result,
        Err(_) => LocalEvidenceRead::Invalid,
    }
}

async fn read_bounded_local_file_inner(path: &Path) -> LocalEvidenceRead {
    let initial_metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return LocalEvidenceRead::Invalid,
        Err(error) if error.kind() == ErrorKind::NotFound => return LocalEvidenceRead::Missing,
        Err(_) => return LocalEvidenceRead::Invalid,
    };
    if initial_metadata.len() > CLAUDE_LOCAL_FILE_LIMIT {
        return LocalEvidenceRead::Invalid;
    }
    let mut file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(_) => return LocalEvidenceRead::Invalid,
    };
    let opened_metadata = match file.metadata().await {
        Ok(metadata) => metadata,
        Err(_) => return LocalEvidenceRead::Invalid,
    };
    if !local_metadata_matches(&initial_metadata, &opened_metadata) {
        return LocalEvidenceRead::Invalid;
    }
    let bytes = match read_bounded_local_reader(&mut file).await {
        Some(bytes) => bytes,
        None => return LocalEvidenceRead::Invalid,
    };
    let final_file_metadata = match file.metadata().await {
        Ok(metadata) => metadata,
        Err(_) => return LocalEvidenceRead::Invalid,
    };
    let final_path_metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(_) => return LocalEvidenceRead::Invalid,
    };
    if !local_metadata_matches(&opened_metadata, &final_file_metadata)
        || !local_metadata_matches(&final_file_metadata, &final_path_metadata)
    {
        return LocalEvidenceRead::Invalid;
    }
    LocalEvidenceRead::Document(bytes)
}

fn local_metadata_matches(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    if left.is_file() != right.is_file()
        || left.is_dir() != right.is_dir()
        || left.len() != right.len()
        || left
            .modified()
            .ok()
            .zip(right.modified().ok())
            .is_none_or(|(left_modified, right_modified)| left_modified != right_modified)
    {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        left.dev() == right.dev() && left.ino() == right.ino()
    }
    #[cfg(not(unix))]
    {
        true
    }
}

async fn read_bounded_local_reader(reader: impl AsyncRead + Unpin) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(CLAUDE_LOCAL_FILE_LIMIT)
        .read_to_end(&mut bytes)
        .await
        .ok()?;
    Some(bytes)
}

fn claude_managed_settings_path(target: &ProviderMaintenanceTarget) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let _ = target;
        Some(PathBuf::from(
            "/Library/Application Support/ClaudeCode/managed-settings.json",
        ))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = target;
        Some(PathBuf::from("/etc/claude-code/managed-settings.json"))
    }
    #[cfg(windows)]
    {
        effective_environment_value(&target.environment, "ProgramFiles")
            .map(|directory| PathBuf::from(directory).join("ClaudeCode/managed-settings.json"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProviderUpdateCommand {
    pub(crate) display: String,
    pub(crate) executable: String,
    pub(crate) args: Vec<String>,
    pub(crate) lock_key: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProviderMaintenanceCapabilities {
    pub(crate) latest: Option<LatestVersionSource>,
    pub(crate) version_scheme: VersionScheme,
    pub(crate) display_command: Option<String>,
    pub(crate) update: Option<ProviderUpdateCommand>,
}

impl ProviderMaintenanceCapabilities {
    /// OpenCode 2 is published as `@opencode/cli`; `opencode-ai` stays on the 1.x line. Where the
    /// installation path did not name the package, the installed major decides it, for the
    /// advisory and the package-manager update action alike.
    pub(crate) fn for_installed_version(mut self, driver: &str, version: Option<&str>) -> Self {
        use crate::provider::opencode::{OpenCodeCliSupport, OpenCodeProtocol, cli_support};
        if driver != "opencode"
            || self.latest != Some(LatestVersionSource::Npm("opencode-ai"))
            || version.map(cli_support) != Some(OpenCodeCliSupport::Supported(OpenCodeProtocol::V2))
        {
            return self;
        }
        let package = OPENCODE_2_PACKAGE.package_name;
        let replace = |value: &str| {
            value
                .replace("opencode-ai@latest", &format!("{package}@latest"))
                .replace(" opencode-ai", &format!(" {package}"))
        };
        self.latest = Some(OPENCODE_2_PACKAGE.latest);
        self.display_command = self.display_command.as_deref().map(replace);
        if let Some(update) = self.update.as_mut() {
            update.display = replace(&update.display);
            for argument in &mut update.args {
                if argument == "opencode-ai" {
                    *argument = package.to_owned();
                } else if argument == "opencode-ai@latest" {
                    *argument = format!("{package}@latest");
                }
            }
        }
        self
    }

    pub(crate) fn unknown() -> Self {
        Self {
            latest: None,
            version_scheme: VersionScheme::Semver,
            display_command: None,
            update: None,
        }
    }

    fn executable<I, S>(
        latest: LatestVersionSource,
        version_scheme: VersionScheme,
        executable: impl Into<String>,
        args: I,
        lock_key: &'static str,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::executable_with_optional_latest(
            Some(latest),
            version_scheme,
            executable,
            args,
            lock_key,
        )
    }

    fn executable_with_optional_latest<I, S>(
        latest: Option<LatestVersionSource>,
        version_scheme: VersionScheme,
        executable: impl Into<String>,
        args: I,
        lock_key: &'static str,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let executable = executable.into();
        let args = args.into_iter().map(Into::into).collect::<Vec<String>>();
        let display = std::iter::once(executable.as_str())
            .chain(args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            latest,
            version_scheme,
            display_command: Some(display.clone()),
            update: Some(ProviderUpdateCommand {
                display,
                executable,
                args,
                lock_key,
            }),
        }
    }

    fn display_only(
        latest: LatestVersionSource,
        version_scheme: VersionScheme,
        display_command: impl Into<String>,
    ) -> Self {
        Self {
            latest: Some(latest),
            version_scheme,
            display_command: Some(display_command.into()),
            update: None,
        }
    }
}

#[derive(Clone, Copy)]
struct NativeUpdate {
    args: &'static [&'static str],
    lock_key: &'static str,
    path_matches: fn(&str) -> bool,
}

#[derive(Clone, Copy)]
struct PackageDefinition {
    latest: LatestVersionSource,
    package_name: &'static str,
    homebrew_args: &'static [&'static str],
    native_update: Option<NativeUpdate>,
}

pub(crate) fn capabilities_for_paths(
    target: &ProviderMaintenanceTarget,
    resolved: Option<&Path>,
    canonical: Option<&Path>,
) -> ProviderMaintenanceCapabilities {
    capabilities_for_paths_with_claude_hints(
        &target.driver,
        &target.binary_path,
        resolved,
        canonical,
        &ClaudeSourceHints::default(),
    )
}

pub(crate) fn capabilities_for_paths_with_claude_hints(
    driver: &str,
    binary_path: &str,
    resolved: Option<&Path>,
    canonical: Option<&Path>,
    claude_hints: &ClaudeSourceHints,
) -> ProviderMaintenanceCapabilities {
    match driver {
        "cursor" => cursor_capabilities(binary_path, resolved, canonical),
        "grok" => ProviderMaintenanceCapabilities::unknown(),
        _ => resolve_package_managed_capabilities(
            driver,
            binary_path,
            resolved,
            canonical,
            claude_hints,
        ),
    }
}

fn cursor_capabilities(
    binary_path: &str,
    resolved: Option<&Path>,
    canonical: Option<&Path>,
) -> ProviderMaintenanceCapabilities {
    let Some(canonical_path) = canonical.map(normalized_path) else {
        return ProviderMaintenanceCapabilities::unknown();
    };
    if !is_cursor_release_binary_path(&canonical_path) {
        return ProviderMaintenanceCapabilities::unknown();
    }
    let Some(resolved_path) = resolved.map(normalized_path) else {
        return ProviderMaintenanceCapabilities::unknown();
    };
    if !is_cursor_release_binary_path(&resolved_path) && !is_cursor_launch_link_path(&resolved_path)
    {
        return ProviderMaintenanceCapabilities::unknown();
    }
    ProviderMaintenanceCapabilities::executable(
        LatestVersionSource::CursorInstaller,
        VersionScheme::CursorRelease,
        resolved_or_configured_binary(binary_path, resolved),
        ["update"],
        "cursor-agent",
    )
}

fn resolve_package_managed_capabilities(
    driver: &str,
    binary_path: &str,
    resolved: Option<&Path>,
    canonical: Option<&Path>,
    claude_hints: &ClaudeSourceHints,
) -> ProviderMaintenanceCapabilities {
    let resolved_path = resolved.map(normalized_path);
    let canonical_path = canonical.map(normalized_path);
    let opencode_2 = driver == "opencode"
        && [&resolved_path, &canonical_path]
            .into_iter()
            .flatten()
            .any(|path| path.contains("/node_modules/@opencode/cli/"));
    let Some(definition) = (if opencode_2 {
        Some(OPENCODE_2_PACKAGE)
    } else {
        package_definition(driver)
    }) else {
        return ProviderMaintenanceCapabilities::unknown();
    };
    let ownership = match reconcile_installation_ownership(
        driver,
        resolved_path.as_deref(),
        canonical_path.as_deref(),
    ) {
        Ok(ownership) => ownership,
        Err(()) => return ProviderMaintenanceCapabilities::unknown(),
    };

    if driver == "claudeAgent"
        && let Some(capabilities) =
            claude_source_capabilities(resolved_path.as_deref(), ownership, claude_hints)
    {
        return capabilities;
    }

    if let Some(native) = definition.native_update
        && ownership == Some(InstallationOwnership::Native)
        && resolved_path.as_deref().is_some_and(native.path_matches)
    {
        let latest = if driver == "claudeAgent" {
            (!claude_hints.invalid_managed_channel)
                .then_some(claude_hints.channel)
                .flatten()
                .map(LatestVersionSource::Claude)
        } else {
            Some(definition.latest)
        };
        let mut capabilities = ProviderMaintenanceCapabilities::executable_with_optional_latest(
            latest,
            VersionScheme::Semver,
            resolved_or_configured_binary(binary_path, resolved),
            native.args.iter().copied(),
            native.lock_key,
        );
        if driver == "claudeAgent" && claude_hints.updates_disabled {
            capabilities.update = None;
        }
        return capabilities;
    }
    if driver == "codex" && ownership == Some(InstallationOwnership::CodexStandalone) {
        return ProviderMaintenanceCapabilities::executable(
            definition.latest,
            VersionScheme::Semver,
            resolved_or_configured_binary(binary_path, resolved),
            ["update"],
            "codex-standalone",
        );
    }
    if ownership == Some(InstallationOwnership::Homebrew) {
        return ProviderMaintenanceCapabilities::executable(
            definition.latest,
            VersionScheme::Semver,
            "brew",
            definition.homebrew_args.iter().copied(),
            "homebrew",
        );
    }
    if ownership == Some(InstallationOwnership::VitePlus) {
        return maybe_disable_claude_update(
            ProviderMaintenanceCapabilities::executable(
                definition.latest,
                VersionScheme::Semver,
                "vp",
                ["i", "-g", definition.package_name],
                "vite-plus",
            ),
            driver,
            claude_hints,
        );
    }
    if ownership == Some(InstallationOwnership::Bun) {
        return maybe_disable_claude_update(
            ProviderMaintenanceCapabilities::executable(
                definition.latest,
                VersionScheme::Semver,
                "bun",
                ["i", "-g", &format!("{}@latest", definition.package_name)],
                "bun",
            ),
            driver,
            claude_hints,
        );
    }
    if ownership == Some(InstallationOwnership::Pnpm) {
        return maybe_disable_claude_update(
            ProviderMaintenanceCapabilities::executable(
                definition.latest,
                VersionScheme::Semver,
                "pnpm",
                ["add", "-g", &format!("{}@latest", definition.package_name)],
                "pnpm",
            ),
            driver,
            claude_hints,
        );
    }
    if ownership == Some(InstallationOwnership::Npm) {
        return maybe_disable_claude_update(
            ProviderMaintenanceCapabilities::executable(
                definition.latest,
                VersionScheme::Semver,
                "npm",
                [
                    "install",
                    "-g",
                    &format!("{}@latest", definition.package_name),
                ],
                "npm",
            ),
            driver,
            claude_hints,
        );
    }
    ProviderMaintenanceCapabilities::unknown()
}

fn maybe_disable_claude_update(
    mut capabilities: ProviderMaintenanceCapabilities,
    driver: &str,
    hints: &ClaudeSourceHints,
) -> ProviderMaintenanceCapabilities {
    if driver == "claudeAgent" && hints.updates_disabled {
        capabilities.update = None;
    }
    capabilities
}

fn claude_source_capabilities(
    resolved_path: Option<&str>,
    ownership: Option<InstallationOwnership>,
    hints: &ClaudeSourceHints,
) -> Option<ProviderMaintenanceCapabilities> {
    let mut capabilities = if ownership == Some(InstallationOwnership::ClaudeLatestCask) {
        ProviderMaintenanceCapabilities::executable(
            LatestVersionSource::Claude(ClaudeReleaseChannel::Latest),
            VersionScheme::Semver,
            "brew",
            ["upgrade", "--cask", "claude-code@latest"],
            "homebrew",
        )
    } else if ownership == Some(InstallationOwnership::ClaudeStableCask) {
        ProviderMaintenanceCapabilities::executable(
            LatestVersionSource::Claude(ClaudeReleaseChannel::Stable),
            VersionScheme::Semver,
            "brew",
            ["upgrade", "--cask", "claude-code"],
            "homebrew",
        )
    } else if ownership == Some(InstallationOwnership::ClaudeWinget) {
        ProviderMaintenanceCapabilities::executable(
            LatestVersionSource::Claude(ClaudeReleaseChannel::Latest),
            VersionScheme::Semver,
            "winget",
            ["upgrade", "Anthropic.ClaudeCode"],
            "winget",
        )
    } else if resolved_path == Some("/usr/bin/claude") {
        match (hints.linux_package, hints.channel) {
            (Some(LinuxPackageManager::Apt), Some(channel)) => {
                ProviderMaintenanceCapabilities::display_only(
                    LatestVersionSource::Claude(channel),
                    VersionScheme::Semver,
                    "sudo apt update && sudo apt upgrade claude-code",
                )
            }
            (Some(LinuxPackageManager::Dnf), Some(channel)) => {
                ProviderMaintenanceCapabilities::display_only(
                    LatestVersionSource::Claude(channel),
                    VersionScheme::Semver,
                    "sudo dnf upgrade claude-code",
                )
            }
            (Some(LinuxPackageManager::Apk), Some(channel)) => {
                ProviderMaintenanceCapabilities::display_only(
                    LatestVersionSource::Claude(channel),
                    VersionScheme::Semver,
                    "apk update && apk upgrade claude-code",
                )
            }
            _ => ProviderMaintenanceCapabilities::unknown(),
        }
    } else {
        return None;
    };
    if hints.updates_disabled {
        capabilities.update = None;
    }
    Some(capabilities)
}

/// OpenCode 2 is published as `@opencode/cli`; `opencode-ai` stays on the 1.x line.
const OPENCODE_2_PACKAGE: PackageDefinition = PackageDefinition {
    latest: LatestVersionSource::Npm("@opencode/cli"),
    package_name: "@opencode/cli",
    homebrew_args: &["upgrade", "anomalyco/tap/opencode"],
    native_update: Some(NativeUpdate {
        args: &["upgrade"],
        lock_key: "opencode-native",
        path_matches: is_opencode_native_path,
    }),
};

fn package_definition(driver: &str) -> Option<PackageDefinition> {
    match driver {
        "codex" => Some(PackageDefinition {
            latest: LatestVersionSource::Npm("@openai/codex"),
            package_name: "@openai/codex",
            homebrew_args: &["upgrade", "--cask", "codex"],
            native_update: None,
        }),
        "claudeAgent" => Some(PackageDefinition {
            latest: LatestVersionSource::Npm("@anthropic-ai/claude-code"),
            package_name: "@anthropic-ai/claude-code",
            homebrew_args: &["upgrade", "claude-code"],
            native_update: Some(NativeUpdate {
                args: &["update"],
                lock_key: "claude-native",
                path_matches: is_claude_native_path,
            }),
        }),
        "opencode" => Some(PackageDefinition {
            latest: LatestVersionSource::Npm("opencode-ai"),
            package_name: "opencode-ai",
            homebrew_args: &["upgrade", "anomalyco/tap/opencode"],
            native_update: Some(NativeUpdate {
                args: &["upgrade"],
                lock_key: "opencode-native",
                path_matches: is_opencode_native_path,
            }),
        }),
        _ => None,
    }
}

fn normalized_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InstallationOwnership {
    Native,
    CodexStandalone,
    ClaudeStableCask,
    ClaudeLatestCask,
    ClaudeWinget,
    Homebrew,
    VitePlus,
    Bun,
    Pnpm,
    Npm,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PathOwnershipEvidence {
    None,
    Invalid,
    Owned(InstallationOwnership),
}

fn reconcile_installation_ownership(
    driver: &str,
    resolved: Option<&str>,
    canonical: Option<&str>,
) -> Result<Option<InstallationOwnership>, ()> {
    let resolved = resolved
        .map(|path| path_ownership_evidence(driver, path))
        .unwrap_or(PathOwnershipEvidence::None);
    let canonical = canonical
        .map(|path| path_ownership_evidence(driver, path))
        .unwrap_or(PathOwnershipEvidence::None);
    match (resolved, canonical) {
        (PathOwnershipEvidence::Invalid, _) | (_, PathOwnershipEvidence::Invalid) => Err(()),
        (PathOwnershipEvidence::Owned(left), PathOwnershipEvidence::Owned(right))
            if left != right =>
        {
            Err(())
        }
        (PathOwnershipEvidence::Owned(source), _) | (_, PathOwnershipEvidence::Owned(source)) => {
            Ok(Some(source))
        }
        _ => Ok(None),
    }
}

fn path_ownership_evidence(driver: &str, path: &str) -> PathOwnershipEvidence {
    let components = path_components(path);
    let expected_binary = provider_binary_name(driver);

    if contains_component(&components, "caskroom") || contains_component(&components, "cellar") {
        return match homebrew_ownership(driver, &components, expected_binary) {
            Some(ownership) => PathOwnershipEvidence::Owned(ownership),
            None => PathOwnershipEvidence::Invalid,
        };
    }
    if contains_component(&components, "node_modules") {
        return match global_package_ownership(driver, &components) {
            Some(ownership) => PathOwnershipEvidence::Owned(ownership),
            None => PathOwnershipEvidence::Invalid,
        };
    }
    if is_user_home_shim_path(&components, &[".vite-plus", "bin"]) {
        return exact_ownership_evidence(
            provider_basename(driver, path, expected_binary),
            InstallationOwnership::VitePlus,
        );
    }
    if is_user_home_shim_path(&components, &[".bun", "bin"]) {
        return exact_ownership_evidence(
            provider_basename(driver, path, expected_binary),
            InstallationOwnership::Bun,
        );
    }
    if is_pnpm_shim_path(&components) {
        return exact_ownership_evidence(
            provider_basename(driver, path, expected_binary),
            InstallationOwnership::Pnpm,
        );
    }
    if is_npm_shim_path(&components) {
        return exact_ownership_evidence(
            provider_basename(driver, path, expected_binary),
            InstallationOwnership::Npm,
        );
    }
    if driver == "claudeAgent" && is_winget_location(&components) {
        return exact_ownership_evidence(
            exact_provider_basename(path, "claude")
                && winget_package_identity_is_claude(&components),
            InstallationOwnership::ClaudeWinget,
        );
    }
    if driver == "claudeAgent" && is_claude_native_path(path) {
        return PathOwnershipEvidence::Owned(InstallationOwnership::Native);
    }
    if driver == "opencode" && is_opencode_native_path(path) {
        return PathOwnershipEvidence::Owned(InstallationOwnership::Native);
    }
    if driver == "codex" && is_codex_standalone_path(path, &components) {
        return PathOwnershipEvidence::Owned(InstallationOwnership::CodexStandalone);
    }
    PathOwnershipEvidence::None
}

fn exact_ownership_evidence(
    matches: bool,
    ownership: InstallationOwnership,
) -> PathOwnershipEvidence {
    if matches {
        PathOwnershipEvidence::Owned(ownership)
    } else {
        PathOwnershipEvidence::Invalid
    }
}

fn provider_binary_name(driver: &str) -> &str {
    match driver {
        "claudeAgent" => "claude",
        "opencode" => "opencode",
        _ => "codex",
    }
}

fn path_components(path: &str) -> Vec<&str> {
    path.split('/')
        .filter(|component| !component.is_empty())
        .collect()
}

fn contains_component(components: &[&str], expected: &str) -> bool {
    components.contains(&expected)
}

/// OpenCode 2's npm package also installs an `opencode2` shim.
fn provider_basename(driver: &str, path: &str, expected: &str) -> bool {
    exact_provider_basename(path, expected)
        || (driver == "opencode" && exact_provider_basename(path, "opencode2"))
}

fn exact_provider_basename(path: &str, expected: &str) -> bool {
    let Some(basename) = path.rsplit('/').next() else {
        return false;
    };
    let stem = [".exe", ".cmd", ".bat", ".ps1"]
        .into_iter()
        .find_map(|extension| basename.strip_suffix(extension))
        .unwrap_or(basename);
    stem == expected
}

fn is_drive_component(component: &str) -> bool {
    component.len() == 2
        && component.as_bytes()[0].is_ascii_alphabetic()
        && component.as_bytes()[1] == b':'
}

fn user_home_prefix_len(components: &[&str]) -> Option<usize> {
    match components {
        ["root", ..] => Some(1),
        ["home" | "users", user, ..] if !user.is_empty() => Some(2),
        [drive, "users", user, ..] if is_drive_component(drive) && !user.is_empty() => Some(3),
        _ => None,
    }
}

fn is_user_home_path(components: &[&str], suffix: &[&str]) -> bool {
    user_home_prefix_len(components).is_some_and(|prefix| components.get(prefix..) == Some(suffix))
}

fn is_user_home_shim_path(components: &[&str], suffix: &[&str]) -> bool {
    components
        .split_last()
        .is_some_and(|(_, root)| is_user_home_path(root, suffix))
}

fn is_pnpm_shim_path(components: &[&str]) -> bool {
    let Some((_, root)) = components.split_last() else {
        return false;
    };
    [
        &[".local", "share", "pnpm"][..],
        &["local", "share", "pnpm"][..],
        &["library", "pnpm"][..],
        &["appdata", "local", "pnpm"][..],
        &["appdata", "roaming", "pnpm"][..],
    ]
    .into_iter()
    .any(|suffix| is_user_home_path(root, suffix))
}

fn is_npm_shim_path(components: &[&str]) -> bool {
    let Some((_, root)) = components.split_last() else {
        return false;
    };
    is_user_home_path(root, &["appdata", "roaming", "npm"])
        || root.ends_with(&[".npm-global", "bin"])
}

fn global_package_ownership(driver: &str, components: &[&str]) -> Option<InstallationOwnership> {
    let node_modules = components
        .iter()
        .rposition(|component| *component == "node_modules")?;
    let package = &components[node_modules + 1..];
    let expected: &[&[&str]] = match driver {
        "codex" => &[&["@openai", "codex"]],
        "claudeAgent" => &[&["@anthropic-ai", "claude-code"]],
        "opencode" => &[&["opencode-ai"], &["@opencode", "cli"]],
        _ => return None,
    };
    if !expected
        .iter()
        .any(|expected| package.starts_with(expected))
    {
        return None;
    }
    let prefix = &components[..node_modules];
    if is_user_home_path(prefix, &[".bun", "install", "global"]) {
        return Some(InstallationOwnership::Bun);
    }
    if is_pnpm_global_root(prefix) {
        return Some(InstallationOwnership::Pnpm);
    }
    is_npm_global_root(prefix).then_some(InstallationOwnership::Npm)
}

fn is_pnpm_global_root(components: &[&str]) -> bool {
    let Some((&version, root)) = components.split_last() else {
        return false;
    };
    let Some((&"global", root)) = root.split_last() else {
        return false;
    };
    !version.is_empty()
        && [
            &[".local", "share", "pnpm"][..],
            &["local", "share", "pnpm"][..],
            &["library", "pnpm"][..],
            &["appdata", "local", "pnpm"][..],
            &["appdata", "roaming", "pnpm"][..],
        ]
        .into_iter()
        .any(|suffix| is_user_home_path(root, suffix))
}

fn is_npm_global_root(components: &[&str]) -> bool {
    matches!(
        components,
        ["usr", "local", "lib"]
            | ["usr", "lib"]
            | ["opt", "homebrew", "lib"]
            | ["home", "linuxbrew", ".linuxbrew", "lib"]
    ) || matches!(components, [drive, "npm"] if is_drive_component(drive))
        || is_user_home_path(components, &["appdata", "roaming", "npm"])
        || is_user_home_path(components, &[".npm-global", "lib"])
}

fn homebrew_ownership(
    driver: &str,
    components: &[&str],
    expected_binary: &str,
) -> Option<InstallationOwnership> {
    if !components
        .last()
        .is_some_and(|basename| exact_provider_basename(basename, expected_binary))
    {
        return None;
    }
    let brew_root = [
        (&["opt", "homebrew", "caskroom"][..], "caskroom"),
        (&["usr", "local", "caskroom"][..], "caskroom"),
        (&["opt", "homebrew", "cellar"][..], "cellar"),
        (&["usr", "local", "cellar"][..], "cellar"),
        (&["home", "linuxbrew", ".linuxbrew", "cellar"][..], "cellar"),
    ]
    .into_iter()
    .find(|(root, _)| components.starts_with(root));
    let (root, kind) = brew_root?;
    let identity = components.get(root.len()).copied();
    match driver {
        "codex" if kind == "caskroom" && identity == Some("codex") => {
            Some(InstallationOwnership::Homebrew)
        }
        "claudeAgent" if kind == "caskroom" && identity == Some("claude-code") => {
            Some(InstallationOwnership::ClaudeStableCask)
        }
        "claudeAgent" if kind == "caskroom" && identity == Some("claude-code@latest") => {
            Some(InstallationOwnership::ClaudeLatestCask)
        }
        "opencode" if kind == "cellar" && identity == Some("opencode") => {
            Some(InstallationOwnership::Homebrew)
        }
        _ => None,
    }
}

fn is_winget_location(components: &[&str]) -> bool {
    let Some(prefix) = user_home_prefix_len(components) else {
        return false;
    };
    matches!(
        components.get(prefix..),
        Some(["appdata", "local", "microsoft", "winget", "links", _])
            | Some(["appdata", "local", "microsoft", "winget", "packages", _, _,])
    )
}

fn winget_package_identity_is_claude(components: &[&str]) -> bool {
    let Some(prefix) = user_home_prefix_len(components) else {
        return false;
    };
    match components.get(prefix..) {
        Some(["appdata", "local", "microsoft", "winget", "links", _]) => true,
        Some(
            [
                "appdata",
                "local",
                "microsoft",
                "winget",
                "packages",
                identity,
                _,
            ],
        ) => identity.starts_with("anthropic.claudecode_"),
        _ => false,
    }
}

fn is_codex_standalone_path(path: &str, components: &[&str]) -> bool {
    (is_user_home_path(
        components,
        &[
            ".codex",
            "packages",
            "standalone",
            "current",
            "bin",
            "codex",
        ],
    ) || is_user_home_path(
        components,
        &[
            "appdata",
            "local",
            "programs",
            "openai",
            "codex",
            "bin",
            "codex.exe",
        ],
    )) && exact_provider_basename(path, "codex")
}

fn resolved_or_configured_binary(binary_path: &str, resolved: Option<&Path>) -> String {
    resolved
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| binary_path.to_owned())
}

fn is_claude_native_path(path: &str) -> bool {
    let components = path_components(path);
    is_user_home_path(&components, &[".local", "bin", "claude"])
        || is_user_home_path(&components, &[".local", "bin", "claude.exe"])
}

fn is_opencode_native_path(path: &str) -> bool {
    let components = path_components(path);
    is_user_home_path(&components, &[".opencode", "bin", "opencode"])
        || is_user_home_path(&components, &[".opencode", "bin", "opencode.exe"])
}

fn is_cursor_release_binary_path(path: &str) -> bool {
    path.contains("/.local/share/cursor-agent/versions/")
        && (path.ends_with("/cursor-agent") || path.ends_with("/cursor-agent.exe"))
}

fn is_cursor_launch_link_path(path: &str) -> bool {
    path.ends_with("/.local/bin/cursor-agent")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ObservedReader {
        remaining: usize,
        consumed: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl tokio::io::AsyncRead for ObservedReader {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
            buffer: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let reader = self.get_mut();
            let amount = reader.remaining.min(buffer.remaining());
            buffer.initialize_unfilled()[..amount].fill(b'x');
            buffer.advance(amount);
            reader.remaining -= amount;
            reader
                .consumed
                .fetch_add(amount, std::sync::atomic::Ordering::SeqCst);
            std::task::Poll::Ready(Ok(()))
        }
    }

    fn claude_target(environment: &[(&str, &std::ffi::OsStr)]) -> ProviderMaintenanceTarget {
        ProviderMaintenanceTarget {
            instance_id: "claude".to_owned(),
            driver: "claudeAgent".to_owned(),
            binary_path: "claude".to_owned(),
            environment: environment
                .iter()
                .map(|(name, value)| ((*name).into(), (*value).to_owned()))
                .collect(),
        }
    }

    #[test]
    fn managed_claude_channel_overrides_user_channel() {
        let hints = claude_hints_from_documents(
            Some(br#"{"autoUpdatesChannel":"latest"}"#),
            [br#"{"autoUpdatesChannel":"stable"}"#.as_slice()],
        );
        assert_eq!(hints.channel, Some(ClaudeReleaseChannel::Stable));
    }

    #[test]
    fn native_claude_defaults_to_latest_without_valid_user_settings() {
        for user in [None, Some(br#"{"#.as_slice())] {
            let hints = claude_hints_from_documents(user, std::iter::empty());
            assert_eq!(hints.channel, Some(ClaudeReleaseChannel::Latest));
            assert!(!hints.invalid_managed_channel);
        }
    }

    #[test]
    fn every_present_non_string_managed_claude_channel_fails_closed() {
        for channel in ["null", "[]", "{}", "true", "false", "1", "1.5"] {
            let managed = format!(r#"{{"autoUpdatesChannel":{channel}}}"#);
            let hints = claude_hints_from_documents(
                Some(br#"{"autoUpdatesChannel":"latest"}"#),
                [managed.as_bytes()],
            );
            assert_eq!(hints.channel, None, "{channel}");
            assert!(hints.invalid_managed_channel, "{channel}");
        }
    }

    #[test]
    fn later_managed_claude_scalars_override_earlier_values() {
        let hints = claude_hints_from_documents(
            Some(br#"{"autoUpdatesChannel":"latest","env":{"DISABLE_UPDATES":"1"}}"#),
            [
                br#"{"autoUpdatesChannel":"stable","env":{"DISABLE_UPDATES":"1"}}"#.as_slice(),
                br#"{"autoUpdatesChannel":"preview","env":{"DISABLE_UPDATES":"0"}}"#.as_slice(),
            ],
        );
        assert_eq!(hints.channel, None);
        assert!(!hints.updates_disabled);
        assert!(hints.invalid_managed_channel);
    }

    #[tokio::test]
    async fn discovers_sorted_bounded_claude_settings_from_target_environment() {
        let directory = tempfile::tempdir().expect("temp directory");
        let config = directory.path().join("target-claude");
        let managed = directory.path().join("managed-settings.json");
        let managed_directory = directory.path().join("managed-settings.d");
        std::fs::create_dir_all(&config).expect("config directory");
        std::fs::create_dir_all(&managed_directory).expect("managed directory");
        std::fs::write(
            config.join("settings.json"),
            br#"{"autoUpdatesChannel":"latest"}"#,
        )
        .expect("user settings");
        std::fs::write(&managed, br#"{"autoUpdatesChannel":"stable"}"#).expect("managed settings");
        std::fs::write(
            managed_directory.join("01-stable.json"),
            br#"{"autoUpdatesChannel":"stable"}"#,
        )
        .expect("first managed settings");
        std::fs::write(
            managed_directory.join("02-latest.json"),
            br#"{"autoUpdatesChannel":"latest"}"#,
        )
        .expect("last managed settings");
        let target = claude_target(&[
            ("CLAUDE_CONFIG_DIR", config.as_os_str()),
            ("DISABLE_UPDATES", std::ffi::OsStr::new("1")),
            ("DISABLE_AUTOUPDATER", std::ffi::OsStr::new("1")),
        ]);

        let hints = discover_claude_hints_from_paths(
            &target,
            None,
            Some(&managed),
            Some(&managed_directory),
            &[],
        )
        .await;

        assert_eq!(hints.channel, Some(ClaudeReleaseChannel::Latest));
        assert!(hints.updates_disabled);
    }

    #[tokio::test]
    async fn too_many_managed_claude_fragments_fail_closed() {
        let directory = tempfile::tempdir().expect("temp directory");
        let config = directory.path().join("target-claude");
        let managed_directory = directory.path().join("managed-settings.d");
        std::fs::create_dir_all(&config).expect("config directory");
        std::fs::create_dir_all(&managed_directory).expect("managed directory");
        for index in 0..=CLAUDE_MANAGED_ENTRY_LIMIT {
            std::fs::write(
                managed_directory.join(format!("{index:03}.json")),
                br#"{"autoUpdatesChannel":"latest"}"#,
            )
            .expect("managed fragment");
        }
        let target = claude_target(&[("CLAUDE_CONFIG_DIR", config.as_os_str())]);

        let hints =
            discover_claude_hints_from_paths(&target, None, None, Some(&managed_directory), &[])
                .await;

        assert_eq!(hints.channel, None);
        assert!(hints.invalid_managed_channel);
    }

    #[tokio::test]
    async fn aggregate_managed_claude_fragment_overflow_fails_closed() {
        let directory = tempfile::tempdir().expect("temp directory");
        let config = directory.path().join("target-claude");
        let managed_directory = directory.path().join("managed-settings.d");
        std::fs::create_dir_all(&config).expect("config directory");
        std::fs::create_dir_all(&managed_directory).expect("managed directory");
        let mut document = br#"{"autoUpdatesChannel":"latest"}"#.to_vec();
        document.resize(CLAUDE_LOCAL_FILE_LIMIT as usize, b' ');
        for index in 0..=CLAUDE_MANAGED_AGGREGATE_LIMIT.div_ceil(CLAUDE_LOCAL_FILE_LIMIT as usize) {
            std::fs::write(
                managed_directory.join(format!("{index:03}.json")),
                &document,
            )
            .expect("managed fragment");
        }
        let target = claude_target(&[("CLAUDE_CONFIG_DIR", config.as_os_str())]);

        let hints =
            discover_claude_hints_from_paths(&target, None, None, Some(&managed_directory), &[])
                .await;

        assert_eq!(hints.channel, None);
        assert!(hints.invalid_managed_channel);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn non_regular_managed_claude_fragment_fails_closed_without_hanging() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temp directory");
        let config = directory.path().join("target-claude");
        let managed_directory = directory.path().join("managed-settings.d");
        let outside = directory.path().join("outside.json");
        std::fs::create_dir_all(&config).expect("config directory");
        std::fs::create_dir_all(&managed_directory).expect("managed directory");
        std::fs::write(&outside, br#"{"autoUpdatesChannel":"stable"}"#).expect("outside settings");
        symlink(&outside, managed_directory.join("01-linked.json"))
            .expect("managed settings symlink");
        let target = claude_target(&[("CLAUDE_CONFIG_DIR", config.as_os_str())]);

        let hints = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            discover_claude_hints_from_paths(&target, None, None, Some(&managed_directory), &[]),
        )
        .await
        .expect("non-regular evidence must not block");

        assert_eq!(hints.channel, None);
        assert!(hints.invalid_managed_channel);
    }

    #[tokio::test]
    async fn disable_autoupdater_does_not_disable_the_explicit_claude_action() {
        let directory = tempfile::tempdir().expect("temp directory");
        let target = claude_target(&[
            ("CLAUDE_CONFIG_DIR", directory.path().as_os_str()),
            ("DISABLE_UPDATES", std::ffi::OsStr::new("0")),
            ("DISABLE_AUTOUPDATER", std::ffi::OsStr::new("1")),
        ]);
        let hints = discover_claude_hints_from_paths(&target, None, None, None, &[]).await;

        assert!(!hints.updates_disabled);
    }

    #[tokio::test]
    async fn local_claude_documents_are_capped_at_64_kib() {
        let directory = tempfile::tempdir().expect("temp directory");
        let exact = directory.path().join("exact.json");
        let oversized = directory.path().join("oversized.json");
        std::fs::write(&exact, vec![b' '; CLAUDE_LOCAL_FILE_LIMIT as usize])
            .expect("exact document");
        std::fs::write(&oversized, vec![b' '; CLAUDE_LOCAL_FILE_LIMIT as usize + 1])
            .expect("oversized document");

        assert!(matches!(
            read_bounded_local_file(&exact).await,
            LocalEvidenceRead::Document(bytes)
                if bytes.len() == CLAUDE_LOCAL_FILE_LIMIT as usize
        ));
        assert!(matches!(
            read_bounded_local_file(&oversized).await,
            LocalEvidenceRead::Invalid
        ));
    }

    #[tokio::test]
    async fn bounded_claude_reader_never_consumes_a_sentinel_byte() {
        let consumed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let bytes = read_bounded_local_reader(ObservedReader {
            remaining: CLAUDE_LOCAL_FILE_LIMIT as usize + 1,
            consumed: consumed.clone(),
        })
        .await
        .expect("bounded read");

        assert_eq!(bytes.len(), CLAUDE_LOCAL_FILE_LIMIT as usize);
        assert_eq!(
            consumed.load(std::sync::atomic::Ordering::SeqCst),
            CLAUDE_LOCAL_FILE_LIMIT as usize
        );
    }

    #[tokio::test]
    async fn repository_discovery_rejects_conflicting_claude_channels() {
        let directory = tempfile::tempdir().expect("temp directory");
        let config = directory.path().join("claude-config");
        let apt = directory.path().join("claude-code.list");
        let rpm = directory.path().join("claude-code.repo");
        std::fs::create_dir_all(&config).expect("config directory");
        std::fs::write(
            &apt,
            "deb https://downloads.claude.ai/claude-code/apt/stable stable main",
        )
        .expect("apt repository");
        std::fs::write(
            &rpm,
            "baseurl=https://downloads.claude.ai/claude-code/rpm/latest",
        )
        .expect("rpm repository");
        let target = claude_target(&[("CLAUDE_CONFIG_DIR", config.as_os_str())]);
        let repositories = vec![apt.clone(), rpm.clone()];

        let conflicting = discover_claude_hints_from_paths(
            &target,
            Some(Path::new("/usr/bin/claude")),
            None,
            None,
            &repositories,
        )
        .await;
        assert_eq!(conflicting.channel, None);
        assert_eq!(conflicting.linux_package, None);

        std::fs::write(&rpm, "[unrelated]").expect("remove rpm marker");
        let marked = discover_claude_hints_from_paths(
            &target,
            Some(Path::new("/usr/bin/claude")),
            None,
            None,
            &repositories,
        )
        .await;
        assert_eq!(marked.channel, Some(ClaudeReleaseChannel::Stable));
        assert_eq!(marked.linux_package, Some(LinuxPackageManager::Apt));
    }

    #[test]
    fn claude_disable_updates_removes_only_the_executable_action() {
        let hints = ClaudeSourceHints {
            channel: Some(ClaudeReleaseChannel::Latest),
            updates_disabled: true,
            linux_package: None,
            invalid_managed_channel: false,
        };
        let capabilities = capabilities_for_paths_with_claude_hints(
            "claudeAgent",
            "claude",
            Some(Path::new("/home/me/.local/bin/claude")),
            Some(Path::new("/home/me/.local/share/claude/versions/2.1.224")),
            &hints,
        );
        assert_eq!(
            capabilities.latest,
            Some(LatestVersionSource::Claude(ClaudeReleaseChannel::Latest))
        );
        assert_eq!(
            capabilities.display_command,
            Some("/home/me/.local/bin/claude update".to_owned())
        );
        assert!(capabilities.update.is_none());
    }

    #[test]
    fn claude_disable_updates_also_preserves_package_manager_guidance() {
        let hints = ClaudeSourceHints {
            updates_disabled: true,
            ..ClaudeSourceHints::default()
        };
        let capabilities = capabilities_for_paths_with_claude_hints(
            "claudeAgent",
            "claude",
            Some(Path::new("/usr/local/bin/claude")),
            Some(Path::new(
                "/usr/local/lib/node_modules/@anthropic-ai/claude-code/cli.js",
            )),
            &hints,
        );
        assert_eq!(
            capabilities.latest,
            Some(LatestVersionSource::Npm("@anthropic-ai/claude-code"))
        );
        assert_eq!(
            capabilities.display_command.as_deref(),
            Some("npm install -g @anthropic-ai/claude-code@latest")
        );
        assert!(capabilities.update.is_none());
    }

    #[test]
    fn parses_claude_linux_repository_channels() {
        assert_eq!(
            parse_claude_repository_marker(
                "deb https://downloads.claude.ai/claude-code/apt/stable stable main"
            ),
            Some((LinuxPackageManager::Apt, ClaudeReleaseChannel::Stable))
        );
        assert_eq!(
            parse_claude_repository_marker(
                "baseurl=https://downloads.claude.ai/claude-code/rpm/latest"
            ),
            Some((LinuxPackageManager::Dnf, ClaudeReleaseChannel::Latest))
        );
        assert_eq!(
            parse_claude_repository_marker(
                "https://downloads.claude.ai/claude-code/apt/stable-spoof"
            ),
            None
        );
    }

    #[test]
    fn resolves_claude_installation_sources() {
        let stable = ClaudeSourceHints {
            channel: Some(ClaudeReleaseChannel::Stable),
            ..ClaudeSourceHints::default()
        };
        let latest = ClaudeSourceHints {
            channel: Some(ClaudeReleaseChannel::Latest),
            ..ClaudeSourceHints::default()
        };
        let apt = ClaudeSourceHints {
            channel: Some(ClaudeReleaseChannel::Stable),
            linux_package: Some(LinuxPackageManager::Apt),
            ..ClaudeSourceHints::default()
        };
        let dnf = ClaudeSourceHints {
            channel: Some(ClaudeReleaseChannel::Latest),
            linux_package: Some(LinuxPackageManager::Dnf),
            ..ClaudeSourceHints::default()
        };
        let apk = ClaudeSourceHints {
            channel: Some(ClaudeReleaseChannel::Stable),
            linux_package: Some(LinuxPackageManager::Apk),
            ..ClaudeSourceHints::default()
        };
        let unmarked = ClaudeSourceHints {
            channel: None,
            ..ClaudeSourceHints::default()
        };
        let invalid_managed = ClaudeSourceHints {
            invalid_managed_channel: true,
            ..ClaudeSourceHints::default()
        };
        let cases = [
            (
                "C:/Users/me/.local/bin/claude.exe",
                Some("C:/Users/me/.local/share/claude/versions/2.1.220/claude.exe"),
                &stable,
                Some(LatestVersionSource::Claude(ClaudeReleaseChannel::Stable)),
                Some("C:/Users/me/.local/bin/claude.exe update"),
                Some("claude-native"),
            ),
            (
                "/opt/homebrew/bin/claude",
                Some("/opt/homebrew/Caskroom/claude-code/2.1.220/claude"),
                &stable,
                Some(LatestVersionSource::Claude(ClaudeReleaseChannel::Stable)),
                Some("brew upgrade --cask claude-code"),
                Some("homebrew"),
            ),
            (
                "/opt/homebrew/bin/claude",
                Some("/opt/homebrew/Caskroom/claude-code@latest/2.1.224/claude"),
                &stable,
                Some(LatestVersionSource::Claude(ClaudeReleaseChannel::Latest)),
                Some("brew upgrade --cask claude-code@latest"),
                Some("homebrew"),
            ),
            (
                "C:/Users/me/AppData/Local/Microsoft/WinGet/Links/claude.exe",
                Some(
                    "C:/Users/me/AppData/Local/Microsoft/WinGet/Packages/Anthropic.ClaudeCode_Microsoft.Winget.Source_8wekyb3d8bbwe/claude.exe",
                ),
                &stable,
                Some(LatestVersionSource::Claude(ClaudeReleaseChannel::Latest)),
                Some("winget upgrade Anthropic.ClaudeCode"),
                Some("winget"),
            ),
            (
                "C:/Users/me/AppData/Local/Microsoft/WinGet/Packages/Anthropic.ClaudeCode_Microsoft.Winget.Source_8wekyb3d8bbwe/claude.exe",
                None,
                &stable,
                Some(LatestVersionSource::Claude(ClaudeReleaseChannel::Latest)),
                Some("winget upgrade Anthropic.ClaudeCode"),
                Some("winget"),
            ),
            (
                "/usr/bin/claude",
                Some("/usr/bin/claude"),
                &apt,
                Some(LatestVersionSource::Claude(ClaudeReleaseChannel::Stable)),
                Some("sudo apt update && sudo apt upgrade claude-code"),
                None,
            ),
            (
                "/usr/bin/claude",
                Some("/usr/bin/claude"),
                &dnf,
                Some(LatestVersionSource::Claude(ClaudeReleaseChannel::Latest)),
                Some("sudo dnf upgrade claude-code"),
                None,
            ),
            (
                "/usr/bin/claude",
                Some("/usr/bin/claude"),
                &apk,
                Some(LatestVersionSource::Claude(ClaudeReleaseChannel::Stable)),
                Some("apk update && apk upgrade claude-code"),
                None,
            ),
            (
                "/usr/bin/claude",
                Some("/usr/bin/claude"),
                &unmarked,
                None,
                None,
                None,
            ),
            (
                "/usr/local/bin/claude",
                Some("/usr/local/lib/node_modules/@anthropic-ai/claude-code/cli.js"),
                &latest,
                Some(LatestVersionSource::Npm("@anthropic-ai/claude-code")),
                Some("npm install -g @anthropic-ai/claude-code@latest"),
                Some("npm"),
            ),
            (
                "/home/me/.local/bin/claude",
                Some("/home/me/.local/share/claude/versions/2.1.220"),
                &invalid_managed,
                None,
                Some("/home/me/.local/bin/claude update"),
                Some("claude-native"),
            ),
        ];

        for (resolved, canonical, hints, expected_latest, expected_display, expected_lock) in cases
        {
            let capabilities = capabilities_for_paths_with_claude_hints(
                "claudeAgent",
                "claude",
                Some(Path::new(resolved)),
                canonical.map(Path::new),
                hints,
            );
            assert_eq!(capabilities.latest, expected_latest, "{resolved}");
            assert_eq!(
                capabilities.display_command.as_deref(),
                expected_display,
                "{resolved}"
            );
            assert_eq!(
                capabilities.update.as_ref().map(|update| update.lock_key),
                expected_lock,
                "{resolved}"
            );
        }
    }

    #[test]
    fn recognizes_only_official_cursor_release_paths() {
        let official = capabilities_for_paths(
            &ProviderMaintenanceTarget {
                instance_id: "cursor".to_owned(),
                driver: "cursor".to_owned(),
                binary_path: "cursor-agent".to_owned(),
                environment: Vec::new(),
            },
            Some(Path::new("/home/me/.local/bin/cursor-agent")),
            Some(Path::new(
                "/home/me/.local/share/cursor-agent/versions/2026.06.19-653a7fb/cursor-agent",
            )),
        );
        assert_eq!(official.latest, Some(LatestVersionSource::CursorInstaller));
        assert_eq!(official.version_scheme, VersionScheme::CursorRelease);
        assert_eq!(
            official.display_command.as_deref(),
            Some("/home/me/.local/bin/cursor-agent update")
        );
        assert!(official.update.is_some());

        let wrapper = capabilities_for_paths(
            &ProviderMaintenanceTarget {
                instance_id: "cursor".to_owned(),
                driver: "cursor".to_owned(),
                binary_path: "cursor-agent".to_owned(),
                environment: Vec::new(),
            },
            Some(Path::new("/srv/tools/cursor-agent")),
            None,
        );
        assert_eq!(wrapper, ProviderMaintenanceCapabilities::unknown());
    }
}
