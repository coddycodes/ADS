use bibcode_server::{
    assets,
    git::canonical_worktree_path_key,
    production::host_paths::process_compatible_path,
    project, review, workspace,
    worktree_catalog::{
        AdoptedWorktreeAvailability, WorkspaceAvailabilityRegistry, WorkspaceLossTransition,
    },
};

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use assets::{AssetAccess, AssetIssueRequest, AssetResource, ResolvedAsset};
use project::ProjectFaviconResolver;
use review::{ReviewBackend, ReviewDiffPreviewInput, ReviewError, ReviewService};
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use workspace::{
    AssetContextResolver, EntryKind, SearchLimits, SearchResult, WorkspaceEntry, WorkspaceError,
    WorkspaceRpc, WorkspaceRpcDependencies, WorkspaceSearchIndex, WorkspaceService,
    WorkspaceWatcher,
};

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn entry_is_ignored(result: &serde_json::Value, path: &str) -> bool {
    result["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| entry["path"] == path)
        .unwrap_or_else(|| panic!("missing entry {path}"))["ignored"]
        .as_bool()
        .unwrap_or(false)
}

#[tokio::test]
async fn workspace_unavailable_rejects_file_search_and_review_before_side_effects() {
    let root = TempDir::new().expect("root");
    let registry = WorkspaceAvailabilityRegistry::new();
    assert!(
        registry
            .mark_unavailable(WorkspaceLossTransition {
                thread_id: "thread-1".to_owned(),
                repository_key: "repository-1".to_owned(),
                generation: 9,
                path: root.path().to_path_buf(),
                availability: AdoptedWorktreeAvailability::MissingRegistered,
            })
            .await
            .expect("physical identity resolves")
    );
    let rpc = WorkspaceRpc::new(WorkspaceService::default()).with_availability_registry(registry);
    let physical_root = canonical_worktree_path_key(root.path())
        .await
        .expect("physical workspace root");

    for (method, payload) in [
        (
            "projects.readFile",
            json!({"cwd":path_string(root.path()),"relativePath":"missing.txt"}),
        ),
        (
            "projects.searchEntries",
            json!({"cwd":path_string(root.path()),"query":"secret","limit":10}),
        ),
        (
            "review.getDiffPreview",
            json!({"cwd":path_string(root.path()),"baseRef":null}),
        ),
    ] {
        let error = rpc
            .handle(method, payload)
            .await
            .expect_err("guarded request must fail");
        assert_eq!(error["_tag"], "WorkspaceUnavailableError");
        assert_eq!(error["reason"], "workspace-unavailable");
        assert_eq!(error["threadId"], "thread-1");
        assert_eq!(error["path"], physical_root);
        assert_eq!(error["availability"], "missing-registered");
    }
}

async fn write(root: &Path, relative: &str, contents: &[u8]) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.expect("parent");
    }
    tokio::fs::write(path, contents)
        .await
        .expect("write fixture");
}

struct StaticAssetContextResolver {
    roots: std::collections::HashMap<String, PathBuf>,
    failing_thread_id: Option<String>,
}

impl AssetContextResolver for StaticAssetContextResolver {
    fn resolve_workspace_root<'a>(
        &'a self,
        thread_id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<PathBuf>, String>> + Send + 'a>,
    > {
        Box::pin(async move {
            if self.failing_thread_id.as_deref() == Some(thread_id) {
                return Err("projection lookup failed".to_owned());
            }
            Ok(self.roots.get(thread_id).cloned())
        })
    }
}

#[tokio::test]
async fn workspace_rpc_rejects_traversal_and_preserves_wire_error_tag() {
    let root = TempDir::new().expect("root");
    let outside = root.path().parent().expect("parent").join("outside.txt");
    tokio::fs::write(&outside, "secret").await.expect("outside");
    let rpc = WorkspaceRpc::new(WorkspaceService::default());

    let error = rpc
        .handle(
            "projects.readFile",
            json!({"cwd": path_string(root.path()), "relativePath": "../outside.txt"}),
        )
        .await
        .expect_err("traversal must fail");

    assert_eq!(error["_tag"], "ProjectReadFileError");
    assert_eq!(error["failure"], "workspace_path_outside_root");
    assert!(!error.to_string().contains("secret"));
}

#[cfg(unix)]
#[tokio::test]
async fn workspace_rpc_rejects_symlink_escape_for_read_and_delete() {
    use std::os::unix::fs::symlink;

    let root = TempDir::new().expect("root");
    let outside = TempDir::new().expect("outside");
    write(outside.path(), "secret.txt", b"secret").await;
    symlink(outside.path(), root.path().join("escape")).expect("symlink");
    let rpc = WorkspaceRpc::new(WorkspaceService::default());
    let cwd = path_string(root.path());

    for (method, payload) in [
        (
            "projects.readFile",
            json!({"cwd": cwd, "relativePath": "escape/secret.txt"}),
        ),
        (
            "projects.deleteEntry",
            json!({"cwd": cwd, "relativePath": "escape/secret.txt"}),
        ),
    ] {
        let error = rpc.handle(method, payload).await.expect_err("escape");
        assert_eq!(error["failure"], "resolved_path_outside_root");
    }
    assert!(outside.path().join("secret.txt").exists());
}

#[tokio::test]
async fn reads_are_binary_safe_and_bounded_to_one_mebibyte() {
    let root = TempDir::new().expect("root");
    write(root.path(), "binary.dat", b"visible\0secret").await;
    write(root.path(), "large.txt", &vec![b'a'; 1024 * 1024 + 17]).await;
    let service = WorkspaceService::default();

    let binary = service
        .read_file(root.path(), "binary.dat")
        .await
        .expect_err("binary");
    assert!(matches!(binary, WorkspaceError::BinaryFile { .. }));

    let large = service
        .read_file(root.path(), "large.txt")
        .await
        .expect("large");
    assert_eq!(large.contents.len(), 1024 * 1024);
    assert_eq!(large.byte_length, 1024 * 1024 + 17);
    assert!(large.truncated);
}

#[tokio::test]
async fn search_honors_ignores_pagination_and_memory_limits() {
    let root = TempDir::new().expect("root");
    write(
        root.path(),
        ".gitignore",
        "ignored.txt\n.convex/\n".as_bytes(),
    )
    .await;
    write(root.path(), "src/components/Composer.tsx", b"").await;
    write(root.path(), "src/index.ts", b"").await;
    write(root.path(), "ignored.txt", b"").await;
    write(root.path(), ".convex/local/data.json", b"").await;
    write(root.path(), "node_modules/pkg/index.js", b"").await;
    let index = WorkspaceSearchIndex::new(
        root.path().to_path_buf(),
        SearchLimits {
            max_entries: 4,
            max_memory_bytes: 1024,
            ..SearchLimits::default()
        },
    );
    index
        .refresh(CancellationToken::new())
        .await
        .expect("refresh");

    let listed = index.list(None).await;
    assert!(listed.truncated);
    assert!(listed.entries.iter().all(|entry| {
        !entry.path.starts_with("node_modules") && !entry.path.starts_with(".convex")
    }));
    assert!(
        !listed
            .entries
            .iter()
            .any(|entry| entry.path == "ignored.txt")
    );

    let searched = index.search("cmp", 1).await;
    assert_eq!(searched.entries[0].path, "src/components/Composer.tsx");
    assert!(searched.truncated);
    assert!(index.memory_bytes().await <= 1024);
}

#[tokio::test]
async fn cancelled_index_refresh_does_not_replace_the_previous_snapshot() {
    let root = TempDir::new().expect("root");
    write(root.path(), "before.txt", b"").await;
    let index = WorkspaceSearchIndex::new(root.path().to_path_buf(), SearchLimits::default());
    index
        .refresh(CancellationToken::new())
        .await
        .expect("first refresh");
    write(root.path(), "after.txt", b"").await;
    let cancelled = CancellationToken::new();
    cancelled.cancel();

    assert!(matches!(
        index.refresh(cancelled).await,
        Err(WorkspaceError::Cancelled)
    ));
    let listed = index.list(None).await;
    assert!(
        listed
            .entries
            .iter()
            .any(|entry| entry.path == "before.txt")
    );
    assert!(!listed.entries.iter().any(|entry| entry.path == "after.txt"));
}

#[tokio::test]
async fn workspace_rpc_bounds_list_entries_and_preserves_unbounded_calls() {
    let root = TempDir::new().expect("root");
    for index in 0..205 {
        write(root.path(), &format!("file-{index:03}.txt"), b"").await;
    }
    let rpc = WorkspaceRpc::new(WorkspaceService::default());
    let cwd = path_string(root.path());

    let bounded = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd, "limit": 80 }))
        .await
        .expect("bounded list");
    assert_eq!(bounded["entries"].as_array().expect("entries").len(), 80);
    assert_eq!(bounded["truncated"], true);

    let minimum = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd, "limit": 0 }))
        .await
        .expect("minimum-clamped list");
    assert_eq!(minimum["entries"].as_array().expect("entries").len(), 1);
    assert_eq!(minimum["truncated"], true);

    let maximum = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd, "limit": 500 }))
        .await
        .expect("maximum-clamped list");
    assert_eq!(maximum["entries"].as_array().expect("entries").len(), 200);
    assert_eq!(maximum["truncated"], true);

    let unbounded = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("unbounded list");
    assert_eq!(unbounded["entries"].as_array().expect("entries").len(), 205);
    assert_eq!(unbounded["truncated"], false);
}

fn git_in(cwd: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "BiBCode Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "BiBCode Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .expect("git must be installed for integration tests");
    assert!(output.status.success(), "git {args:?} failed");
}

fn init_isolated_git_fixture(root: &Path) {
    git_in(root, &["init", "-b", "main"]);
    let excludes = root.join(".git/bibcode-test-global-excludes");
    std::fs::write(&excludes, "").expect("empty fixture excludes file");
    git_in(
        root,
        &[
            "config",
            "--local",
            "core.excludesFile",
            &path_string(&excludes),
        ],
    );
}

/// A directory holding no files is still a directory the user created and can see on disk.
///
/// `git ls-files` only reports files, so a git-backed scan that infers directories from file paths
/// alone cannot see one — while the non-git walk does. The tree must not disagree with the disk just
/// because the workspace happens to be a repository.
#[tokio::test]
async fn empty_directories_are_listed_in_a_git_workspace() {
    let root = TempDir::new().expect("root");
    git_in(root.path(), &["init", "-b", "main"]);
    write(root.path(), "tracked.txt", b"tracked").await;
    git_in(root.path(), &["add", "-A"]);
    git_in(root.path(), &["commit", "-m", "init"]);

    // Exactly what "New Folder…" produces, and what an mkdir in a file manager produces.
    tokio::fs::create_dir_all(root.path().join("docs/screenshots/empty-child"))
        .await
        .expect("create empty directory");

    let rpc = WorkspaceRpc::new(WorkspaceService::default());
    let result = rpc
        .handle(
            "projects.listEntries",
            json!({ "cwd": path_string(root.path()) }),
        )
        .await
        .expect("list entries");
    let paths = result["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .filter_map(|entry| entry["path"].as_str())
        .collect::<Vec<_>>();

    assert!(
        paths.contains(&"docs/screenshots/empty-child"),
        "an empty directory on disk must appear in the tree; got {paths:?}"
    );
}

#[tokio::test]
async fn workspace_index_contract_matches_git_and_bounded_fallback_exactly() {
    let root = TempDir::new().expect("root");
    init_isolated_git_fixture(root.path());
    write(
        root.path(),
        ".gitignore",
        b"ignored-file.txt\nignored-root/\n",
    )
    .await;
    write(root.path(), "tracked.txt", b"tracked").await;
    write(root.path(), "deleted.txt", b"deleted").await;
    git_in(
        root.path(),
        &["add", ".gitignore", "tracked.txt", "deleted.txt"],
    );
    tokio::fs::remove_file(root.path().join("deleted.txt"))
        .await
        .expect("delete tracked fixture file");
    write(root.path(), "untracked.txt", b"untracked").await;
    write(root.path(), "ignored-file.txt", b"ignored").await;
    write(root.path(), "ignored-root/eager-child.txt", b"ignored").await;
    tokio::fs::create_dir(root.path().join("empty-directory"))
        .await
        .expect("empty directory");

    let rpc = WorkspaceRpc::new(WorkspaceService::default());
    let git_result = rpc
        .handle(
            "projects.listEntries",
            json!({ "cwd": path_string(root.path()) }),
        )
        .await
        .expect("list entries");
    assert_eq!(
        serde_json::from_value::<SearchResult>(git_result).expect("typed Git result"),
        SearchResult {
            entries: vec![
                WorkspaceEntry {
                    path: ".gitignore".to_owned(),
                    kind: EntryKind::File,
                    ignored: false,
                },
                WorkspaceEntry {
                    path: "empty-directory".to_owned(),
                    kind: EntryKind::Directory,
                    ignored: false,
                },
                WorkspaceEntry {
                    path: "ignored-file.txt".to_owned(),
                    kind: EntryKind::File,
                    ignored: true,
                },
                WorkspaceEntry {
                    path: "ignored-root".to_owned(),
                    kind: EntryKind::Directory,
                    ignored: true,
                },
                WorkspaceEntry {
                    path: "ignored-root/eager-child.txt".to_owned(),
                    kind: EntryKind::File,
                    ignored: true,
                },
                WorkspaceEntry {
                    path: "tracked.txt".to_owned(),
                    kind: EntryKind::File,
                    ignored: false,
                },
                WorkspaceEntry {
                    path: "untracked.txt".to_owned(),
                    kind: EntryKind::File,
                    ignored: false,
                },
            ],
            truncated: false,
        }
    );

    std::fs::remove_dir_all(root.path().join(".git")).expect("remove test-owned Git metadata");
    let fallback = WorkspaceSearchIndex::new(root.path().to_path_buf(), SearchLimits::default());
    fallback
        .refresh(CancellationToken::new())
        .await
        .expect("non-Git fallback");
    assert_eq!(
        fallback.list(None).await,
        SearchResult {
            entries: vec![
                WorkspaceEntry {
                    path: ".gitignore".to_owned(),
                    kind: EntryKind::File,
                    ignored: false,
                },
                WorkspaceEntry {
                    path: "empty-directory".to_owned(),
                    kind: EntryKind::Directory,
                    ignored: false,
                },
                WorkspaceEntry {
                    path: "tracked.txt".to_owned(),
                    kind: EntryKind::File,
                    ignored: false,
                },
                WorkspaceEntry {
                    path: "untracked.txt".to_owned(),
                    kind: EntryKind::File,
                    ignored: false,
                },
            ],
            truncated: false,
        }
    );

    let bounded = WorkspaceSearchIndex::new(
        root.path().to_path_buf(),
        SearchLimits {
            max_entries: 2,
            max_memory_bytes: usize::MAX,
            max_path_bytes: usize::MAX,
        },
    );
    bounded
        .refresh(CancellationToken::new())
        .await
        .expect("bounded non-Git fallback");
    assert_eq!(
        bounded.list(None).await,
        SearchResult {
            entries: vec![
                WorkspaceEntry {
                    path: "tracked.txt".to_owned(),
                    kind: EntryKind::File,
                    ignored: false,
                },
                WorkspaceEntry {
                    path: "untracked.txt".to_owned(),
                    kind: EntryKind::File,
                    ignored: false,
                },
            ],
            truncated: true,
        }
    );
}

#[tokio::test]
async fn subscribed_roots_pick_up_outside_changes_without_an_explicit_refresh() {
    let root = TempDir::new().expect("root");
    write(root.path(), "nested/tracked.txt", b"tracked").await;
    let rpc = WorkspaceRpc::new(WorkspaceService::default())
        .with_watch_timing(Duration::from_millis(25), Duration::from_millis(50));
    let cwd = path_string(root.path());

    let lists = |payload| {
        let rpc = rpc.clone();
        async move {
            let result = rpc
                .handle("projects.listEntries", payload)
                .await
                .expect("list entries");
            result["entries"]
                .as_array()
                .expect("entries")
                .iter()
                .any(|entry| entry["path"] == "nested/pasted.txt")
        }
    };

    assert!(!lists(json!({ "cwd": cwd })).await, "cache is warmed");
    let mut changes = rpc
        .subscribe_entry_changes(root.path())
        .await
        .expect("subscription");

    // The paste an Explorer window would make: no workspace RPC is involved.
    write(root.path(), "nested/pasted.txt", b"pasted").await;

    tokio::time::timeout(Duration::from_secs(10), changes.recv())
        .await
        .expect("change signal timeout")
        .expect("change signal");

    // No refresh flag: the sweep must already have dropped the stale snapshot.
    assert!(
        lists(json!({ "cwd": cwd })).await,
        "a subscribed root reflects outside changes without an explicit refresh"
    );
}

/// An unavailable workspace must not acquire a sweep.
///
/// Every other request against a workspace path goes through admission; a subscription that skipped
/// it could start sweeping a root the availability guard has already fenced for removal.
#[tokio::test]
async fn subscribing_is_refused_while_the_workspace_is_unavailable() {
    let root = TempDir::new().expect("root");
    write(root.path(), "nested/tracked.txt", b"tracked").await;
    let registry = WorkspaceAvailabilityRegistry::new();
    assert!(
        registry
            .mark_unavailable(WorkspaceLossTransition {
                thread_id: "thread-1".to_owned(),
                repository_key: "repository-1".to_owned(),
                generation: 3,
                path: root.path().to_path_buf(),
                availability: AdoptedWorktreeAvailability::MissingRegistered,
            })
            .await
            .expect("physical identity resolves")
    );
    let rpc = WorkspaceRpc::new(WorkspaceService::default())
        .with_availability_registry(registry)
        .with_watch_timing(Duration::from_millis(25), Duration::from_millis(20));

    let Err(error) = rpc.subscribe_entry_changes(root.path()).await else {
        panic!("an unavailable workspace must refuse a subscription");
    };
    assert_eq!(error["_tag"], "WorkspaceUnavailableError");
    assert_eq!(
        rpc.active_entry_watches().await,
        0,
        "a refused subscription must not leave a sweep behind"
    );
}

/// Concurrent listers of a cold root must share one scan.
///
/// A scan runs outside the cache lock, so without single-flight every caller that misses the cache
/// starts its own full scan of the same tree -- seconds of duplicated work each, on exactly the
/// paths a large workspace makes expensive.
#[tokio::test]
async fn concurrent_listers_share_a_single_workspace_scan() {
    let root = TempDir::new().expect("root");
    for index in 0..40 {
        write(root.path(), &format!("src/module-{index}.ts"), b"export {}").await;
    }
    let rpc = WorkspaceRpc::new(WorkspaceService::default());
    let cwd = path_string(root.path());
    assert_eq!(rpc.index_scans(), 0);

    let listers = (0..6)
        .map(|_| {
            let rpc = rpc.clone();
            let cwd = cwd.clone();
            tokio::spawn(async move {
                rpc.handle("projects.listEntries", json!({ "cwd": cwd }))
                    .await
            })
        })
        .collect::<Vec<_>>();
    for lister in listers {
        lister
            .await
            .expect("lister task")
            .expect("concurrent list succeeds");
    }

    assert_eq!(
        rpc.index_scans(),
        1,
        "six concurrent listers of one cold root must produce one scan"
    );
}

/// Subscribing must resync, not trust a snapshot older than the sweep.
///
/// A change made while no sweep existed is already on disk when the baseline is stamped, so it can
/// never appear as a difference. If the cached snapshot predates that change, the tree stays stale
/// until something else changes or the user refreshes by hand.
#[tokio::test]
async fn subscribing_resyncs_a_snapshot_that_predates_the_sweep() {
    let root = TempDir::new().expect("root");
    write(root.path(), "nested/tracked.txt", b"tracked").await;
    let rpc = WorkspaceRpc::new(WorkspaceService::default())
        .with_watch_timing(Duration::from_millis(25), Duration::from_millis(20));
    let cwd = path_string(root.path());

    // Warm the cache, then change the workspace while nothing is watching it.
    rpc.handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("warm the cache");
    write(root.path(), "nested/added-while-unwatched.txt", b"added").await;

    let _changes = rpc
        .subscribe_entry_changes(root.path())
        .await
        .expect("subscription");

    // No refresh flag: subscribing is itself a resync point.
    let result = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("list entries");
    let present = result["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .any(|entry| entry["path"] == "nested/added-while-unwatched.txt");
    assert!(
        present,
        "starting a sweep must resync the snapshot it is about to track"
    );
}

/// A sweep must not outlive its subscribers on a quiet workspace.
///
/// Losing the last receiver is invisible to a loop parked on the watcher, so without an independent
/// idle check the sweep survives panel closure until some unrelated path changes — which on a quiet
/// workspace never happens, leaving one two-second sweep per root running for the process lifetime.
#[tokio::test]
async fn idle_sweeps_are_retired_without_waiting_for_a_filesystem_change() {
    let root = TempDir::new().expect("root");
    write(root.path(), "nested/tracked.txt", b"tracked").await;
    let rpc = WorkspaceRpc::new(WorkspaceService::default())
        .with_watch_timing(Duration::from_millis(25), Duration::from_millis(20));

    let changes = rpc
        .subscribe_entry_changes(root.path())
        .await
        .expect("subscription");
    assert_eq!(rpc.active_entry_watches().await, 1);

    // The panel closes. Nothing on disk changes afterwards.
    drop(changes);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while rpc.active_entry_watches().await > 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        rpc.active_entry_watches().await,
        0,
        "the sweep must retire once nothing is listening"
    );

    // A later subscriber gets a working sweep rather than the retired one.
    let mut resubscribed = rpc
        .subscribe_entry_changes(root.path())
        .await
        .expect("second subscription");
    assert_eq!(rpc.active_entry_watches().await, 1);
    write(root.path(), "nested/after-resubscribe.txt", b"after").await;
    tokio::time::timeout(Duration::from_secs(10), resubscribed.recv())
        .await
        .expect("resubscribed signal timeout")
        .expect("resubscribed signal");
}

#[tokio::test]
async fn entry_change_subscribers_share_one_sweep_per_root() {
    let root = TempDir::new().expect("root");
    write(root.path(), "nested/tracked.txt", b"tracked").await;
    let rpc = WorkspaceRpc::new(WorkspaceService::default())
        .with_watch_timing(Duration::from_millis(25), Duration::from_millis(50));
    let noncanonical = root.path().join(".");

    let mut first = rpc
        .subscribe_entry_changes(root.path())
        .await
        .expect("first subscription");
    let mut second = rpc
        .subscribe_entry_changes(&noncanonical)
        .await
        .expect("second subscription");

    write(root.path(), "nested/pasted.txt", b"pasted").await;

    // Both receivers see the same signal, which is what proves they share a sweep rather than each
    // starting one; the noncanonical path must resolve onto the same root.
    tokio::time::timeout(Duration::from_secs(10), first.recv())
        .await
        .expect("first signal timeout")
        .expect("first signal");
    tokio::time::timeout(Duration::from_secs(10), second.recv())
        .await
        .expect("second signal timeout")
        .expect("second signal");
}

/// The save a formatter or editor makes: write a sibling temp file, then rename it over the target.
async fn save_atomically(root: &Path, relative: &str, contents: &[u8]) {
    let target = root.join(relative);
    let temp = target.with_extension("bibcode-save-tmp");
    tokio::fs::write(&temp, contents).await.expect("temp file");
    tokio::fs::rename(&temp, &target)
        .await
        .expect("rename over target");
}

/// An atomic save moves the parent directory's mtime twice but leaves the entry list unchanged.
///
/// Every signal makes each subscriber re-read the whole list, so a formatter run over a large
/// workspace used to push the full list once per sweep while nothing a client shows had changed
/// (#111).
#[tokio::test]
async fn atomic_saves_that_keep_the_entry_list_do_not_signal() {
    let root = TempDir::new().expect("root");
    write(root.path(), "nested/tracked.txt", b"tracked").await;
    let rpc = WorkspaceRpc::new(WorkspaceService::default())
        .with_watch_timing(Duration::from_millis(25), Duration::from_millis(20));
    let mut changes = rpc
        .subscribe_entry_changes(root.path())
        .await
        .expect("subscription");

    for round in 0..3 {
        save_atomically(
            root.path(),
            "nested/tracked.txt",
            format!("{round}").as_bytes(),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(500), changes.recv())
            .await
            .is_err(),
        "an unchanged entry list must not be announced"
    );

    // The same subscription still reports a change a client would show.
    write(root.path(), "nested/added.txt", b"added").await;
    tokio::time::timeout(Duration::from_secs(10), changes.recv())
        .await
        .expect("change signal timeout")
        .expect("change signal");
}

/// Equal paths are not enough: an atomic `.gitignore` save that reclassifies an entry changes what
/// the tree shows, so it must still be announced.
#[tokio::test]
async fn an_atomic_gitignore_save_that_reclassifies_entries_still_signals() {
    let root = TempDir::new().expect("root");
    git_in(root.path(), &["init", "-b", "main"]);
    write(root.path(), ".gitignore", b"").await;
    write(root.path(), "build.log", b"log").await;
    let rpc = WorkspaceRpc::new(WorkspaceService::default())
        .with_watch_timing(Duration::from_millis(25), Duration::from_millis(20));
    let mut changes = rpc
        .subscribe_entry_changes(root.path())
        .await
        .expect("subscription");

    save_atomically(root.path(), ".gitignore", b"build.log\n").await;

    tokio::time::timeout(Duration::from_secs(10), changes.recv())
        .await
        .expect("reclassification signal timeout")
        .expect("reclassification signal");
}

/// A list rebuilt outside the sweep may show more than the sweep last announced.
///
/// A rescan or a mutation rebuilds the index and the panel shows that list. If what it showed is
/// undone before the next sweep, the sweep's rebuild matches its last announcement while the panel
/// does not, so the sweep must announce rather than treat the rebuild as old news.
#[tokio::test]
async fn a_list_rebuilt_outside_the_sweep_is_corrected_by_the_next_announcement() {
    let root = TempDir::new().expect("root");
    write(root.path(), "nested/tracked.txt", b"tracked").await;
    let rpc = WorkspaceRpc::new(WorkspaceService::default())
        .with_watch_timing(Duration::from_millis(300), Duration::from_secs(5));
    let cwd = path_string(root.path());
    let mut changes = rpc
        .subscribe_entry_changes(root.path())
        .await
        .expect("subscription");

    write(root.path(), "nested/transient.txt", b"transient").await;
    let rescanned = rpc
        .handle(
            "projects.listEntries",
            json!({ "cwd": cwd, "refresh": true }),
        )
        .await
        .expect("rescan");
    assert!(
        rescanned["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .any(|entry| entry["path"] == "nested/transient.txt"),
        "the rescan shows the transient file"
    );
    tokio::fs::remove_file(root.path().join("nested/transient.txt"))
        .await
        .expect("remove transient file");

    tokio::time::timeout(Duration::from_secs(5), changes.recv())
        .await
        .expect("correction signal timeout")
        .expect("correction signal");
}

/// Supplies a fixed directory set, standing in for the workspace index.
struct FixedScope(Vec<String>);

impl workspace::WatchScope for FixedScope {
    fn directories(&self, _root: PathBuf) -> workspace::WatchScopeFuture {
        let directories = self.0.clone();
        Box::pin(async move { directories })
    }
}

#[tokio::test]
async fn watcher_reports_directories_whose_contents_changed_and_coalesces_bursts() {
    let root = TempDir::new().expect("root");
    write(root.path(), "nested/baseline.txt", b"baseline").await;
    let coalesce_window = Duration::from_millis(500);
    // A burst ends at the first sweep that sees nothing new, so the sweep must be slower than the
    // writes below for them to read as one burst.
    let watcher = WorkspaceWatcher::new(Duration::from_millis(200), coalesce_window, 2);
    let mut subscription = watcher
        .watch(
            root.path().to_path_buf(),
            Arc::new(FixedScope(vec!["nested".to_owned()])),
        )
        .await;

    // Everything that already existed is the baseline, so a quiet workspace must stay silent
    // rather than reporting a change that would rebuild a freshly built index.
    tokio::time::sleep(coalesce_window + Duration::from_millis(200)).await;
    assert!(
        subscription.try_recv().is_err(),
        "an unchanged workspace must not report a change"
    );

    for sequence in 0..8 {
        write(
            root.path(),
            &format!("nested/burst-{sequence}.txt"),
            sequence.to_string().as_bytes(),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let event = tokio::time::timeout(Duration::from_secs(3), subscription.recv())
        .await
        .expect("watch timeout")
        .expect("watch event");
    // The sweep stats directories, so a new file surfaces as its parent directory moving.
    assert!(
        event
            .changed_paths
            .iter()
            .any(|path| path.ends_with("nested")),
        "expected the containing directory, got {:?}",
        event.changed_paths
    );
    tokio::time::sleep(coalesce_window + Duration::from_millis(150)).await;
    assert!(
        subscription.try_recv().is_err(),
        "burst should be coalesced"
    );

    subscription.cancel();
    tokio::time::timeout(Duration::from_secs(1), subscription.stopped())
        .await
        .expect("watcher cancellation");
    assert_eq!(watcher.active_watchers(), 0);
}

/// Sustained change must not starve notifications.
///
/// Production runs a coalesce window shorter than the poll interval, so a deadline reset on every
/// observed change is always pushed past the current tick. A long build or an agent writing files
/// touches a directory on every poll, which would suppress every signal for as long as it runs —
/// exactly when the tree most needs to update.
#[tokio::test]
async fn watcher_flushes_during_sustained_change_instead_of_starving() {
    let root = TempDir::new().expect("root");
    write(root.path(), "nested/seed.txt", b"seed").await;
    // Same relationship as production: the coalesce window is shorter than the poll interval.
    let poll_interval = Duration::from_millis(60);
    let coalesce_window = Duration::from_millis(20);
    let watcher = WorkspaceWatcher::new(poll_interval, coalesce_window, 4);
    let mut subscription = watcher
        .watch(
            root.path().to_path_buf(),
            Arc::new(FixedScope(vec!["nested".to_owned()])),
        )
        .await;

    // Churn until aborted, so the changes outlast the window we assert over: the point is that a
    // signal arrives *while* changes are still arriving, not once they stop.
    let churn_root = root.path().to_path_buf();
    let churn = tokio::spawn(async move {
        let mut sequence = 0u32;
        loop {
            let path = churn_root.join(format!("nested/churn-{sequence}.txt"));
            let _ = tokio::fs::write(path, b"churn").await;
            sequence += 1;
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    });

    let event = tokio::time::timeout(Duration::from_secs(1), subscription.recv()).await;
    churn.abort();
    assert!(
        event.is_ok_and(|received| received.is_some()),
        "a signal must arrive while changes are still arriving"
    );
    subscription.cancel();
}

/// A burst is announced once it stops, and a lone change is not held back by the window.
///
/// The window only bounds how long sustained change can defer a signal; a burst ends at the first
/// sweep that sees nothing new. Every signal costs each open Files panel a full re-read, so a tool
/// touching files for a few seconds must cost one read rather than one per sweep (#111).
#[tokio::test]
async fn watcher_announces_a_burst_once_it_stops_without_waiting_out_the_window() {
    let root = TempDir::new().expect("root");
    write(root.path(), "nested/seed.txt", b"seed").await;
    let poll_interval = Duration::from_millis(200);
    let watcher = WorkspaceWatcher::new(poll_interval, Duration::from_secs(5), 4);
    let mut subscription = watcher
        .watch(
            root.path().to_path_buf(),
            Arc::new(FixedScope(vec!["nested".to_owned()])),
        )
        .await;

    write(root.path(), "nested/lone.txt", b"lone").await;
    tokio::time::timeout(Duration::from_secs(2), subscription.recv())
        .await
        .expect("a lone change must not wait out the window")
        .expect("lone change event");

    // Faster than the sweep, so no sweep in between is quiet.
    for sequence in 0..15 {
        write(
            root.path(),
            &format!("nested/burst-{sequence}.txt"),
            sequence.to_string().as_bytes(),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::timeout(Duration::from_secs(2), subscription.recv())
        .await
        .expect("burst event timeout")
        .expect("burst event");
    tokio::time::sleep(poll_interval * 4).await;
    assert!(
        subscription.try_recv().is_err(),
        "a burst must be announced once"
    );
    subscription.cancel();
}

#[tokio::test]
async fn watcher_ignores_in_place_edits_that_cannot_change_the_index() {
    let root = TempDir::new().expect("root");
    write(root.path(), "nested/stable.txt", b"before").await;
    let coalesce_window = Duration::from_millis(300);
    let watcher = WorkspaceWatcher::new(Duration::from_millis(20), coalesce_window, 2);
    let mut subscription = watcher
        .watch(
            root.path().to_path_buf(),
            Arc::new(FixedScope(vec!["nested".to_owned()])),
        )
        .await;
    tokio::time::sleep(coalesce_window + Duration::from_millis(200)).await;
    let _ = subscription.try_recv();

    // The index stores paths and kinds, never contents, so rewriting a file in place cannot
    // invalidate it and must not cost a rebuild.
    write(root.path(), "nested/stable.txt", b"after-the-edit").await;
    tokio::time::sleep(coalesce_window + Duration::from_millis(400)).await;

    assert!(
        subscription.try_recv().is_err(),
        "an in-place edit must not report a path-set change"
    );
    subscription.cancel();
}

#[tokio::test]
async fn directory_stamps_cover_the_root_and_skip_missing_directories() {
    let root = TempDir::new().expect("root");
    write(root.path(), "present/file.txt", b"x").await;
    let stamps =
        workspace::directory_stamps(root.path(), &["present".to_owned(), "absent".to_owned()])
            .await;

    assert!(
        stamps.get(root.path()).is_some_and(Option::is_some),
        "the root is always swept so top-level creations are seen"
    );
    assert!(
        stamps
            .get(&root.path().join("present"))
            .is_some_and(Option::is_some)
    );
    assert_eq!(
        stamps.get(&root.path().join("absent")),
        Some(&None),
        "a missing directory is a distinct state, not an omission"
    );
}

#[tokio::test]
async fn browse_shows_hidden_directories_for_directory_and_hidden_prefix_modes() {
    let root = TempDir::new().expect("root");
    write(root.path(), ".config/settings.json", b"{}").await;
    write(root.path(), "config/settings.json", b"{}").await;
    let service = WorkspaceService::default();
    let cwd_with_separator = format!(
        "{}{}",
        root.path().to_string_lossy(),
        std::path::MAIN_SEPARATOR
    );

    let directory_result = service
        .browse(&cwd_with_separator, None, false)
        .await
        .expect("directory browse");
    assert_eq!(
        directory_result
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec![".config", "config"]
    );

    let hidden_prefix_result = service
        .browse(&format!("{cwd_with_separator}.c"), None, false)
        .await
        .expect("hidden browse");
    assert_eq!(
        hidden_prefix_result
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec![".config"]
    );
}

#[tokio::test]
async fn mutations_create_move_duplicate_and_delete_entries() {
    let root = TempDir::new().expect("root");
    let service = WorkspaceService::default();
    service
        .create_entry(root.path(), "src/a.txt", EntryKind::File)
        .await
        .expect("create");
    service
        .write_file(root.path(), "src/a.txt", "hello")
        .await
        .expect("write");
    let duplicate = service
        .duplicate_entry(root.path(), "src/a.txt")
        .await
        .expect("duplicate");
    assert_eq!(duplicate, "src/a copy.txt");
    service
        .rename_entry(root.path(), "src/a copy.txt", "moved/b.txt")
        .await
        .expect("rename");
    service
        .delete_entry(root.path(), "moved")
        .await
        .expect("delete");
    assert!(!root.path().join("moved").exists());
}

#[tokio::test]
async fn service_mutation_edge_cases_return_specific_errors_without_partial_changes() {
    let root = TempDir::new().expect("root");
    let service = WorkspaceService::new(0);
    write(root.path(), "parent-file", b"occupied").await;
    write(root.path(), "source.txt", b"source").await;
    write(root.path(), "destination.txt", b"destination").await;
    tokio::fs::create_dir(root.path().join("directory"))
        .await
        .expect("directory");

    assert!(matches!(
        service
            .write_file(root.path(), "parent-file/child.txt", "content")
            .await,
        Err(WorkspaceError::Operation { .. })
    ));
    assert!(matches!(
        service
            .create_entry(root.path(), "parent-file/child", EntryKind::Directory)
            .await,
        Err(WorkspaceError::Operation { .. })
    ));
    assert!(matches!(
        service
            .create_entry(root.path(), "parent-file/child.txt", EntryKind::File)
            .await,
        Err(WorkspaceError::Operation { .. })
    ));
    assert!(matches!(
        service
            .rename_entry(root.path(), "source.txt", "parent-file/moved.txt")
            .await,
        Err(WorkspaceError::Operation { .. })
    ));
    assert!(root.path().join("source.txt").is_file());

    assert!(matches!(
        service
            .create_entry(root.path(), "source.txt", EntryKind::File)
            .await,
        Err(WorkspaceError::AlreadyExists { .. })
    ));
    assert!(matches!(
        service
            .rename_entry(root.path(), "missing.txt", "renamed.txt")
            .await,
        Err(WorkspaceError::NotFound { .. })
    ));
    assert!(matches!(
        service
            .rename_entry(root.path(), "source.txt", "destination.txt")
            .await,
        Err(WorkspaceError::AlreadyExists { .. })
    ));
    assert!(matches!(
        service.delete_entry(root.path(), "missing.txt").await,
        Err(WorkspaceError::NotFound { .. })
    ));
    assert!(matches!(
        service.duplicate_entry(root.path(), "missing.txt").await,
        Err(WorkspaceError::NotFound { .. })
    ));
    assert!(matches!(
        service.duplicate_entry(root.path(), "directory").await,
        Err(WorkspaceError::NotFile { .. })
    ));
    assert!(matches!(
        service.read_file(root.path(), "directory").await,
        Err(WorkspaceError::NotFile { .. })
    ));

    let deleted = service
        .delete_entry(root.path(), "destination.txt")
        .await
        .expect("delete file");
    assert_eq!(deleted, "destination.txt");
    assert!(!root.path().join("destination.txt").exists());
}

#[tokio::test]
async fn workspace_rpc_reports_typed_index_browse_and_read_results() {
    let root = TempDir::new().expect("root");
    let missing = root.path().join("missing-root");
    let root_file = root.path().join("not-a-directory");
    tokio::fs::write(&root_file, "file")
        .await
        .expect("root file");
    write(root.path(), "readable.txt", b"hello workspace").await;
    let rpc = WorkspaceRpc::new(WorkspaceService::default());

    let read = rpc
        .handle(
            "projects.readFile",
            json!({
                "cwd": path_string(root.path()),
                "relativePath": "readable.txt"
            }),
        )
        .await
        .expect("read result");
    assert_eq!(read["relativePath"], "readable.txt");
    assert_eq!(read["contents"], "hello workspace");
    assert_eq!(read["byteLength"], 15);
    assert_eq!(read["truncated"], false);

    let missing_list = rpc
        .handle(
            "projects.listEntries",
            json!({ "cwd": path_string(&missing) }),
        )
        .await
        .expect_err("missing root");
    assert_eq!(missing_list["_tag"], "ProjectListEntriesError");
    assert_eq!(missing_list["failure"], "workspace_root_not_found");

    let file_search = rpc
        .handle(
            "projects.searchEntries",
            json!({ "cwd": path_string(&root_file), "query": "x", "limit": 10 }),
        )
        .await
        .expect_err("root file");
    assert_eq!(file_search["_tag"], "ProjectSearchEntriesError");
    assert_eq!(file_search["failure"], "workspace_root_not_directory");

    let browse = rpc
        .handle(
            "filesystem.browse",
            json!({ "partialPath": "./relative", "cwd": null }),
        )
        .await
        .expect_err("relative browse needs cwd");
    assert_eq!(browse["_tag"], "FilesystemBrowseError");
    assert_eq!(browse["failure"], "current_project_required");
}

#[tokio::test]
async fn workspace_rpc_rejects_every_malformed_input_shape_and_unknown_methods() {
    let rpc = WorkspaceRpc::new(WorkspaceService::default());
    for method in [
        "projects.readFile",
        "projects.writeFile",
        "projects.createEntry",
        "projects.renameEntry",
        "projects.deleteEntry",
        "projects.duplicateEntry",
        "projects.listEntries",
        "projects.searchEntries",
        "filesystem.browse",
        "assets.createUrl",
        "review.getDiffPreview",
    ] {
        let error = rpc
            .handle(method, json!({}))
            .await
            .expect_err("missing required input");
        assert_eq!(error["_tag"], "InvalidRequest", "method {method}");
        assert!(
            error["message"]
                .as_str()
                .is_some_and(|message| !message.is_empty()),
            "method {method}"
        );
    }

    let unsupported = rpc
        .handle("projects.unsupported", json!({}))
        .await
        .expect_err("unsupported method");
    assert_eq!(unsupported["_tag"], "Defect");
    assert!(
        unsupported["message"]
            .as_str()
            .expect("message")
            .contains("projects.unsupported")
    );
}

#[tokio::test]
async fn workspace_rpc_surfaces_optional_dependency_and_backend_failures() {
    let root = TempDir::new().expect("root");
    let plain = WorkspaceRpc::new(WorkspaceService::default());

    let asset_not_configured = plain
        .handle(
            "assets.createUrl",
            json!({
                "resource": {
                    "_tag": "workspace-file",
                    "threadId": "thread-1",
                    "path": "missing.html"
                }
            }),
        )
        .await
        .expect_err("asset dependency");
    assert_eq!(asset_not_configured["_tag"], "Defect");

    let review_not_configured = plain
        .handle(
            "review.getDiffPreview",
            json!({ "cwd": path_string(root.path()), "baseRef": null }),
        )
        .await
        .expect_err("review dependency");
    assert_eq!(review_not_configured["_tag"], "Defect");

    let access = AssetAccess::new(vec![7; 32], root.path().join("attachments"));
    let asset_rpc = WorkspaceRpc::with_dependencies(
        WorkspaceService::default(),
        WorkspaceRpcDependencies {
            asset_access: Some(access),
            transfer_access: None,
            asset_context_resolver: Some(Arc::new(StaticAssetContextResolver {
                roots: std::collections::HashMap::from([(
                    "thread-1".to_owned(),
                    root.path().to_path_buf(),
                )]),
                failing_thread_id: None,
            })),
            review_service: None,
            mutation_observer: None,
            ..WorkspaceRpcDependencies::default()
        },
    );
    let missing_asset = asset_rpc
        .handle(
            "assets.createUrl",
            json!({
                "resource": {
                    "_tag": "workspace-file",
                    "threadId": "thread-1",
                    "path": "missing.html"
                }
            }),
        )
        .await
        .expect_err("missing asset");
    assert_eq!(missing_asset["_tag"], "AssetWorkspaceAssetInspectionError");

    let review_rpc = WorkspaceRpc::with_dependencies(
        WorkspaceService::default(),
        WorkspaceRpcDependencies {
            asset_access: None,
            transfer_access: None,
            asset_context_resolver: None,
            review_service: Some(ReviewService::new(Arc::new(FailingReviewBackend))),
            mutation_observer: None,
            ..WorkspaceRpcDependencies::default()
        },
    );
    let backend_failure = review_rpc
        .handle(
            "review.getDiffPreview",
            json!({ "cwd": path_string(root.path()), "baseRef": null }),
        )
        .await
        .expect_err("backend failure");
    assert_eq!(backend_failure["_tag"], "Defect");
    assert!(
        backend_failure["message"]
            .as_str()
            .expect("message")
            .contains("review backend failed")
    );
}

#[tokio::test]
async fn explicit_index_refresh_replaces_a_cached_snapshot() {
    let root = TempDir::new().expect("root");
    write(root.path(), "before.txt", b"").await;
    let rpc = WorkspaceRpc::new(WorkspaceService::default());
    let noncanonical_root = root.path().join(".");
    let cwd = path_string(&noncanonical_root);

    let initial = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("initial list");
    assert!(
        initial["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .any(|entry| entry["path"] == "before.txt")
    );

    write(root.path(), "after.txt", b"").await;
    rpc.refresh_index(&noncanonical_root).await;
    let refreshed = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("refreshed list");
    assert!(
        refreshed["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .any(|entry| entry["path"] == "after.txt")
    );
}

#[tokio::test]
async fn list_entries_refreshes_only_when_the_request_opts_in() {
    let root = TempDir::new().expect("root");
    write(root.path(), "tracked.txt", b"").await;
    let rpc = WorkspaceRpc::new(WorkspaceService::default());
    let noncanonical_root = root.path().join(".");
    let cwd = path_string(&noncanonical_root);
    let lists = |payload| {
        let rpc = rpc.clone();
        async move {
            let result = rpc
                .handle("projects.listEntries", payload)
                .await
                .expect("list entries");
            result["entries"]
                .as_array()
                .expect("entries")
                .iter()
                .any(|entry| entry["path"] == "pasted.txt")
        }
    };

    assert!(!lists(json!({ "cwd": cwd })).await, "cache is warmed");

    // Simulate an out-of-band paste: the file appears without any workspace RPC.
    write(root.path(), "pasted.txt", b"").await;

    assert!(
        !lists(json!({ "cwd": cwd })).await,
        "cached snapshots stay stale without an explicit refresh"
    );
    assert!(
        !lists(json!({ "cwd": cwd, "refresh": false })).await,
        "an explicit refresh: false keeps the cached snapshot"
    );
    assert!(
        lists(json!({ "cwd": cwd, "refresh": true })).await,
        "refresh: true rebuilds the index from the filesystem"
    );
    assert!(
        lists(json!({ "cwd": cwd })).await,
        "the refreshed snapshot repopulates the cache for later requests"
    );
}

#[tokio::test]
async fn write_file_existing_path_preserves_cached_index_and_wire_shape() {
    let root = TempDir::new().expect("root");
    write(root.path(), "existing.txt", b"before").await;
    let rpc = WorkspaceRpc::new(WorkspaceService::default());
    let cwd = path_string(root.path());

    rpc.handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("warm index");
    assert_eq!(rpc.index_scans(), 1);

    let response = rpc
        .handle(
            "projects.writeFile",
            json!({ "cwd": cwd, "relativePath": "existing.txt", "contents": "after" }),
        )
        .await
        .expect("write existing file");
    assert_eq!(response, json!({ "relativePath": "existing.txt" }));
    assert_eq!(
        std::fs::read_to_string(root.path().join("existing.txt")).expect("written contents"),
        "after"
    );

    rpc.handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("reuse index");
    assert_eq!(rpc.index_scans(), 1);
}

#[tokio::test]
async fn write_file_new_path_invalidates_cached_index_once_and_preserves_wire_shape() {
    let root = TempDir::new().expect("root");
    let rpc = WorkspaceRpc::new(WorkspaceService::default());
    let cwd = path_string(root.path());

    rpc.handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("warm index");
    let response = rpc
        .handle(
            "projects.writeFile",
            json!({ "cwd": cwd, "relativePath": "created.txt", "contents": "created" }),
        )
        .await
        .expect("create file");
    assert_eq!(response, json!({ "relativePath": "created.txt" }));
    assert_eq!(
        std::fs::read_to_string(root.path().join("created.txt")).expect("created contents"),
        "created"
    );

    for _ in 0..2 {
        rpc.handle("projects.listEntries", json!({ "cwd": cwd }))
            .await
            .expect("list created file");
    }
    assert_eq!(rpc.index_scans(), 2);
}

#[tokio::test]
async fn write_file_new_parent_paths_invalidates_cached_index_once_and_preserves_wire_shape() {
    let root = TempDir::new().expect("root");
    let rpc = WorkspaceRpc::new(WorkspaceService::default());
    let cwd = path_string(root.path());

    rpc.handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("warm index");
    let response = rpc
        .handle(
            "projects.writeFile",
            json!({
                "cwd": cwd,
                "relativePath": "new/parents/created.txt",
                "contents": "created"
            }),
        )
        .await
        .expect("create nested file");
    assert_eq!(
        response,
        json!({ "relativePath": "new/parents/created.txt" })
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("new/parents/created.txt"))
            .expect("created nested contents"),
        "created"
    );

    for _ in 0..2 {
        rpc.handle("projects.listEntries", json!({ "cwd": cwd }))
            .await
            .expect("list nested file");
    }
    assert_eq!(rpc.index_scans(), 2);
}

#[tokio::test]
async fn write_file_nested_gitignore_rebuilds_cached_classification() {
    let root = TempDir::new().expect("root");
    git_in(root.path(), &["init", "-b", "main"]);
    write(root.path(), ".bibcode-task2-empty-excludes", b"").await;
    let empty_excludes = root
        .path()
        .join(".bibcode-task2-empty-excludes")
        .to_string_lossy()
        .replace('\\', "/");
    git_in(
        root.path(),
        &["config", "core.excludesFile", &empty_excludes],
    );
    write(root.path(), "nested/.gitignore", b"").await;
    write(
        root.path(),
        "nested/bibcode-task2-classification-9f3a7c1b",
        b"target",
    )
    .await;
    let rpc = WorkspaceRpc::new(WorkspaceService::default());
    let cwd = path_string(root.path());

    let initial = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("warm index");
    assert!(!entry_is_ignored(
        &initial,
        "nested/bibcode-task2-classification-9f3a7c1b"
    ));

    rpc.handle(
        "projects.writeFile",
        json!({
            "cwd": cwd,
            "relativePath": "nested/.gitignore",
            "contents": "bibcode-task2-classification-9f3a7c1b\n"
        }),
    )
    .await
    .expect("ignore target");
    let ignored = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("rebuild ignored classification");
    assert!(entry_is_ignored(
        &ignored,
        "nested/bibcode-task2-classification-9f3a7c1b"
    ));
    assert_eq!(rpc.index_scans(), 2);

    rpc.handle(
        "projects.writeFile",
        json!({ "cwd": cwd, "relativePath": "nested/.gitignore", "contents": "" }),
    )
    .await
    .expect("unignore target");
    let unignored = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("rebuild unignored classification");
    assert!(!entry_is_ignored(
        &unignored,
        "nested/bibcode-task2-classification-9f3a7c1b"
    ));
    assert_eq!(rpc.index_scans(), 3);
}

#[tokio::test]
async fn write_file_git_internal_controls_rebuild_cached_classification() {
    let root = TempDir::new().expect("root");
    git_in(root.path(), &["init", "-b", "main"]);
    write(root.path(), ".bibcode-task2-empty-excludes", b"").await;
    let empty_excludes = root
        .path()
        .join(".bibcode-task2-empty-excludes")
        .to_string_lossy()
        .replace('\\', "/");
    git_in(
        root.path(),
        &["config", "core.excludesFile", &empty_excludes],
    );
    write(
        root.path(),
        "bibcode-task2-info-exclude-41ac90d7",
        b"target",
    )
    .await;
    write(
        root.path(),
        "bibcode-task2-config-exclude-6e2d38f4",
        b"target",
    )
    .await;
    write(
        root.path(),
        ".task2-global-ignore",
        b"bibcode-task2-config-exclude-6e2d38f4\n",
    )
    .await;
    let original_exclude =
        std::fs::read_to_string(root.path().join(".git/info/exclude")).expect("Git exclude");
    let original_config =
        std::fs::read_to_string(root.path().join(".git/config")).expect("Git config");
    let rpc = WorkspaceRpc::new(WorkspaceService::default());
    let cwd = path_string(root.path());

    let initial = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("warm index");
    assert!(!entry_is_ignored(
        &initial,
        "bibcode-task2-info-exclude-41ac90d7"
    ));
    assert!(!entry_is_ignored(
        &initial,
        "bibcode-task2-config-exclude-6e2d38f4"
    ));

    rpc.handle(
        "projects.writeFile",
        json!({
            "cwd": cwd,
            "relativePath": ".git/info/exclude",
            "contents": format!("{original_exclude}\nbibcode-task2-info-exclude-41ac90d7\n")
        }),
    )
    .await
    .expect("write Git exclude");
    let excluded = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("rebuild exclude classification");
    assert!(entry_is_ignored(
        &excluded,
        "bibcode-task2-info-exclude-41ac90d7"
    ));
    assert_eq!(rpc.index_scans(), 2);

    let excludes_path = root
        .path()
        .join(".task2-global-ignore")
        .to_string_lossy()
        .replace('\\', "/");
    rpc.handle(
        "projects.writeFile",
        json!({
            "cwd": cwd,
            "relativePath": ".git/config",
            "contents": format!(
                "{original_config}\n[core]\n\texcludesFile = {excludes_path}\n"
            )
        }),
    )
    .await
    .expect("write Git config");
    let configured = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("rebuild configured classification");
    assert!(entry_is_ignored(
        &configured,
        "bibcode-task2-info-exclude-41ac90d7"
    ));
    assert!(entry_is_ignored(
        &configured,
        "bibcode-task2-config-exclude-6e2d38f4"
    ));
    assert_eq!(rpc.index_scans(), 3);

    rpc.handle(
        "projects.writeFile",
        json!({ "cwd": cwd, "relativePath": ".git/config", "contents": original_config }),
    )
    .await
    .expect("restore Git config");
    let config_restored = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("rebuild restored config classification");
    assert!(entry_is_ignored(
        &config_restored,
        "bibcode-task2-info-exclude-41ac90d7"
    ));
    assert!(!entry_is_ignored(
        &config_restored,
        "bibcode-task2-config-exclude-6e2d38f4"
    ));
    assert_eq!(rpc.index_scans(), 4);

    rpc.handle(
        "projects.writeFile",
        json!({
            "cwd": cwd,
            "relativePath": ".git/info/exclude",
            "contents": original_exclude
        }),
    )
    .await
    .expect("restore Git exclude");
    let exclude_restored = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("rebuild restored exclude classification");
    assert!(!entry_is_ignored(
        &exclude_restored,
        "bibcode-task2-info-exclude-41ac90d7"
    ));
    assert_eq!(rpc.index_scans(), 5);
}

#[cfg(unix)]
#[tokio::test]
async fn write_file_symlink_aliases_to_classification_controls_invalidate() {
    use std::os::unix::fs::symlink;

    let root = TempDir::new().expect("root");
    git_in(root.path(), &["init", "-b", "main"]);
    write(root.path(), ".bibcode-task2-empty-excludes", b"").await;
    let empty_excludes = root
        .path()
        .join(".bibcode-task2-empty-excludes")
        .to_string_lossy()
        .into_owned();
    git_in(
        root.path(),
        &["config", "core.excludesFile", &empty_excludes],
    );
    write(root.path(), "nested/.gitignore", b"").await;
    write(
        root.path(),
        "nested/bibcode-task2-alias-ignore-582e713c",
        b"target",
    )
    .await;
    write(
        root.path(),
        "bibcode-task2-alias-exclude-a9306d4e",
        b"target",
    )
    .await;
    symlink(
        root.path().join("nested/.gitignore"),
        root.path().join("ignore-control-alias"),
    )
    .expect("ignore control symlink");
    symlink(
        root.path().join(".git"),
        root.path().join("git-control-alias"),
    )
    .expect("Git control symlink");
    let original_exclude =
        std::fs::read_to_string(root.path().join(".git/info/exclude")).expect("Git exclude");
    let rpc = WorkspaceRpc::new(WorkspaceService::default());
    let cwd = path_string(root.path());

    rpc.handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("warm index");
    rpc.handle(
        "projects.writeFile",
        json!({
            "cwd": cwd,
            "relativePath": "ignore-control-alias",
            "contents": "bibcode-task2-alias-ignore-582e713c\n"
        }),
    )
    .await
    .expect("write ignore alias");
    let ignored = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("rebuild ignore alias");
    assert!(entry_is_ignored(
        &ignored,
        "nested/bibcode-task2-alias-ignore-582e713c"
    ));
    assert_eq!(rpc.index_scans(), 2);

    rpc.handle(
        "projects.writeFile",
        json!({
            "cwd": cwd,
            "relativePath": "git-control-alias/info/exclude",
            "contents": format!(
                "{original_exclude}\nbibcode-task2-alias-exclude-a9306d4e\n"
            )
        }),
    )
    .await
    .expect("write Git control alias");
    let excluded = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("rebuild Git control alias");
    assert!(entry_is_ignored(
        &excluded,
        "bibcode-task2-alias-exclude-a9306d4e"
    ));
    assert_eq!(rpc.index_scans(), 3);
}

#[tokio::test]
async fn write_file_mixed_case_classification_controls_invalidate_on_all_hosts() {
    let root = TempDir::new().expect("root");
    for path in [
        "nested/.GITIGNORE",
        ".GIT/config",
        ".GiT/INFO/EXCLUDE",
        "ordinary.txt",
    ] {
        write(root.path(), path, b"before").await;
    }
    let rpc = WorkspaceRpc::new(WorkspaceService::default());
    let cwd = path_string(root.path());

    rpc.handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("warm index");
    assert_eq!(rpc.index_scans(), 1);

    for (expected_scans, path) in [
        (2, "nested/.GITIGNORE"),
        (3, ".GIT/config"),
        (4, ".GiT/INFO/EXCLUDE"),
    ] {
        rpc.handle(
            "projects.writeFile",
            json!({ "cwd": cwd, "relativePath": path, "contents": "after" }),
        )
        .await
        .unwrap_or_else(|error| panic!("write {path}: {error}"));
        rpc.handle("projects.listEntries", json!({ "cwd": cwd }))
            .await
            .unwrap_or_else(|error| panic!("list after {path}: {error}"));
        assert_eq!(rpc.index_scans(), expected_scans, "path {path}");
    }

    rpc.handle(
        "projects.writeFile",
        json!({ "cwd": cwd, "relativePath": "ordinary.txt", "contents": "after" }),
    )
    .await
    .expect("write ordinary file");
    rpc.handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("reuse index after ordinary write");
    assert_eq!(rpc.index_scans(), 4);
}

#[tokio::test]
async fn workspace_rpc_invalidates_cached_indexes_after_mutations() {
    let root = TempDir::new().expect("root");
    write(root.path(), "src/existing.ts", b"export {};\n").await;
    let rpc = WorkspaceRpc::new(WorkspaceService::default());
    let noncanonical_root = root.path().join(".");
    let cwd = path_string(&noncanonical_root);

    let initial = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("initial list");
    assert!(
        !initial["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .any(|entry| entry["path"] == "plans/added.md")
    );

    rpc.handle(
        "projects.writeFile",
        json!({ "cwd": cwd, "relativePath": "plans/added.md", "contents": "# Plan\n" }),
    )
    .await
    .expect("write");
    let after_write = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("list after write");
    assert!(
        after_write["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .any(|entry| entry["path"] == "plans/added.md")
    );

    rpc.handle(
        "projects.duplicateEntry",
        json!({ "cwd": cwd, "relativePath": "src/existing.ts" }),
    )
    .await
    .expect("duplicate");
    let after_duplicate = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("list after duplicate");
    assert!(
        after_duplicate["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .any(|entry| entry["path"] == "src/existing copy.ts")
    );

    rpc.handle(
        "projects.renameEntry",
        json!({
            "cwd": cwd,
            "fromRelativePath": "plans/added.md",
            "toRelativePath": "docs/renamed.md"
        }),
    )
    .await
    .expect("rename");
    let after_rename = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("list after rename");
    assert!(
        after_rename["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .any(|entry| entry["path"] == "docs/renamed.md")
    );
    assert!(
        !after_rename["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .any(|entry| entry["path"] == "plans/added.md")
    );

    rpc.handle(
        "projects.deleteEntry",
        json!({ "cwd": cwd, "relativePath": "docs/renamed.md" }),
    )
    .await
    .expect("delete");
    let after_delete = rpc
        .handle("projects.listEntries", json!({ "cwd": cwd }))
        .await
        .expect("list after delete");
    assert!(
        !after_delete["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .any(|entry| entry["path"] == "docs/renamed.md")
    );
}

#[tokio::test]
async fn browse_filters_files_sorts_directories_and_requires_cwd_for_relative_paths() {
    let root = TempDir::new().expect("root");
    write(root.path(), "alpha/file.txt", b"").await;
    write(root.path(), "alpine/file.txt", b"").await;
    write(root.path(), "alphabet.txt", b"").await;
    let service = WorkspaceService::default();
    let partial = format!(
        "{}{}alp",
        root.path().to_string_lossy(),
        std::path::MAIN_SEPARATOR
    );
    let result = service.browse(&partial, None, false).await.expect("browse");
    assert_eq!(
        result
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "alpine"]
    );
    assert!(matches!(
        service.browse("./src", None, false).await,
        Err(WorkspaceError::CurrentProjectRequired { .. })
    ));
}

#[tokio::test]
async fn directory_browse_returns_canonical_navigation_and_only_directories() {
    let root = TempDir::new().expect("root");
    write(root.path(), "parent/selected/child/file.txt", b"file").await;
    write(
        root.path(),
        "parent/selected/another-child/nested.txt",
        b"nested",
    )
    .await;
    write(root.path(), "parent/selected/ignored.txt", b"file").await;
    let selected = root.path().join("parent/selected");
    let canonical_selected =
        process_compatible_path(std::fs::canonicalize(&selected).expect("canonical selected"));
    let canonical_parent = process_compatible_path(
        std::fs::canonicalize(root.path().join("parent")).expect("canonical parent"),
    );
    let rpc = WorkspaceRpc::new(WorkspaceService::default());

    let result = rpc
        .handle(
            "filesystem.browse",
            json!({
                "partialPath": selected.to_string_lossy(),
                "mode": "directory",
            }),
        )
        .await
        .expect("directory browse");

    assert_eq!(result["directoryPath"], path_string(&canonical_selected));
    assert_eq!(result["ancestorPath"], path_string(&canonical_parent));
    assert_eq!(
        result["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .map(|entry| entry["name"].as_str().expect("entry name"))
            .collect::<Vec<_>>(),
        vec!["another-child", "child"]
    );
    assert!(
        !result["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .any(|entry| entry["name"] == "ignored.txt")
    );
    let breadcrumbs = result["breadcrumbs"].as_array().expect("breadcrumbs");
    assert_eq!(
        breadcrumbs.last().expect("selected breadcrumb")["fullPath"],
        path_string(&canonical_selected)
    );
    assert_eq!(
        breadcrumbs[breadcrumbs.len() - 2]["fullPath"],
        path_string(&canonical_parent)
    );
}

#[tokio::test]
async fn workspace_rpc_routes_asset_urls_through_workspace_context_resolution() {
    let root = TempDir::new().expect("root");
    write(root.path(), "preview/report.html", b"<html></html>").await;
    write(root.path(), "preview/report.css", b"body{}").await;
    let access = AssetAccess::new(vec![4; 32], root.path().join("attachments"));
    let rpc = WorkspaceRpc::with_dependencies(
        WorkspaceService::default(),
        WorkspaceRpcDependencies {
            asset_access: Some(access.clone()),
            transfer_access: None,
            asset_context_resolver: Some(Arc::new(StaticAssetContextResolver {
                roots: std::collections::HashMap::from([(
                    "thread-1".to_owned(),
                    root.path().to_path_buf(),
                )]),
                failing_thread_id: Some("thread-error".to_owned()),
            })),
            review_service: None,
            mutation_observer: None,
            ..WorkspaceRpcDependencies::default()
        },
    );

    let issued = rpc
        .handle(
            "assets.createUrl",
            json!({
                "resource": {
                    "_tag": "workspace-file",
                    "threadId": "thread-1",
                    "path": "preview/report.html"
                }
            }),
        )
        .await
        .expect("asset URL");
    let relative_url = issued["relativeUrl"].as_str().expect("relativeUrl");
    let token = relative_url.split('/').nth(3).expect("token");
    assert!(matches!(
        access.resolve(token, "report.css").await,
        Some(ResolvedAsset::File(_))
    ));

    let missing = rpc
        .handle(
            "assets.createUrl",
            json!({
                "resource": {
                    "_tag": "workspace-file",
                    "threadId": "thread-missing",
                    "path": "preview/report.html"
                }
            }),
        )
        .await
        .expect_err("missing workspace context");
    assert_eq!(missing["_tag"], "AssetWorkspaceContextNotFoundError");

    let failed = rpc
        .handle(
            "assets.createUrl",
            json!({
                "resource": {
                    "_tag": "workspace-file",
                    "threadId": "thread-error",
                    "path": "preview/report.html"
                }
            }),
        )
        .await
        .expect_err("failed workspace context");
    assert_eq!(failed["_tag"], "AssetWorkspaceContextResolutionError");
}

#[tokio::test]
async fn assets_create_url_never_signs_an_untrusted_attachment_id() {
    let state = TempDir::new().expect("state");
    let attachments = state.path().join("attachments");
    tokio::fs::create_dir(&attachments)
        .await
        .expect("attachments");
    write(state.path(), "state.sqlite", b"database secret").await;
    let absolute = state.path().join("absolute-secret");
    tokio::fs::write(&absolute, b"absolute secret")
        .await
        .unwrap();
    let overlong = "a".repeat(129);
    tokio::fs::write(attachments.join(&overlong), b"long id secret")
        .await
        .unwrap();
    let rpc = WorkspaceRpc::with_dependencies(
        WorkspaceService::default(),
        WorkspaceRpcDependencies {
            asset_access: Some(AssetAccess::new(vec![8; 32], attachments)),
            transfer_access: None,
            asset_context_resolver: None,
            review_service: None,
            mutation_observer: None,
            ..WorkspaceRpcDependencies::default()
        },
    );

    for attachment_id in [
        "../state.sqlite".to_owned(),
        absolute.to_string_lossy().into_owned(),
        "CON".to_owned(),
        overlong,
    ] {
        let error = rpc
            .handle(
                "assets.createUrl",
                json!({
                    "resource": {
                        "_tag": "attachment",
                        "attachmentId": attachment_id
                    }
                }),
            )
            .await
            .expect_err("untrusted attachment ids never receive signed URLs");
        assert_eq!(error["_tag"], "AssetAttachmentNotFoundError");
    }
}

#[tokio::test]
async fn signed_assets_are_exact_or_confined_to_safe_preview_siblings() {
    let root = TempDir::new().expect("root");
    write(root.path(), "preview/report.html", b"<html></html>").await;
    write(root.path(), "preview/report.css", b"body{}").await;
    write(root.path(), "preview/.env", b"secret").await;
    write(root.path(), "image.png", b"png").await;
    let access = AssetAccess::new(vec![9; 32], root.path().join("attachments"));

    let html = access
        .issue(AssetIssueRequest {
            resource: AssetResource::WorkspaceFile {
                thread_id: "thread-1".to_owned(),
                path: "preview/report.html".to_owned(),
            },
            workspace_root: Some(root.path().to_path_buf()),
        })
        .await
        .expect("asset URL");
    let token = html.relative_url.split('/').nth(3).expect("token");
    assert!(matches!(
        access.resolve(token, "report.css").await,
        Some(ResolvedAsset::File(_))
    ));
    assert_eq!(access.resolve(token, "../secret.txt").await, None);
    assert_eq!(access.resolve(token, ".env").await, None);

    let image = access
        .issue(AssetIssueRequest {
            resource: AssetResource::WorkspaceFile {
                thread_id: "thread-1".to_owned(),
                path: "image.png".to_owned(),
            },
            workspace_root: Some(root.path().to_path_buf()),
        })
        .await
        .expect("image URL");
    let token = image.relative_url.split('/').nth(3).expect("token");
    assert!(access.resolve(token, "image.png").await.is_some());
    assert_eq!(access.resolve(token, "report.css").await, None);
}

#[tokio::test]
async fn signed_asset_capabilities_expire_and_never_follow_workspace_symlinks() {
    let root = TempDir::new().expect("root");
    let outside = TempDir::new().expect("outside");
    write(root.path(), "preview/report.html", b"<html></html>").await;
    write(outside.path(), "secret.css", b"secret").await;

    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), root.path().join("preview/escape"))
        .expect("symlink");

    let access = AssetAccess::with_ttl(
        vec![3; 32],
        root.path().join("attachments"),
        Duration::from_millis(1),
    );
    let issued = access
        .issue(AssetIssueRequest {
            resource: AssetResource::WorkspaceFile {
                thread_id: "thread-1".to_owned(),
                path: "preview/report.html".to_owned(),
            },
            workspace_root: Some(root.path().to_path_buf()),
        })
        .await
        .expect("asset URL");
    let token = issued.relative_url.split('/').nth(3).expect("token");

    #[cfg(unix)]
    assert_eq!(access.resolve(token, "escape/secret.css").await, None);
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert_eq!(access.resolve(token, "report.css").await, None);
}

#[tokio::test]
async fn workspace_rpc_routes_review_requests_through_the_injected_service() {
    let service = ReviewService::new(Arc::new(EmptyReviewBackend));
    let rpc = WorkspaceRpc::with_dependencies(
        WorkspaceService::default(),
        WorkspaceRpcDependencies {
            asset_access: None,
            transfer_access: None,
            asset_context_resolver: None,
            review_service: Some(service),
            mutation_observer: None,
            ..WorkspaceRpcDependencies::default()
        },
    );
    let cwd = path_string(TempDir::new().expect("cwd").path());

    let result = rpc
        .handle(
            "review.getDiffPreview",
            json!({ "cwd": cwd, "baseRef": null, "ignoreWhitespace": true }),
        )
        .await
        .expect("review result");

    assert_eq!(result["cwd"], cwd);
    assert_eq!(result["sources"], json!([]));
}

#[tokio::test]
async fn favicon_resolution_stays_within_the_project_and_reads_icon_metadata() {
    let root = TempDir::new().expect("root");
    write(
        root.path(),
        "index.html",
        br#"<link rel="icon" href="/brand/logo.svg">"#,
    )
    .await;
    write(root.path(), "public/brand/logo.svg", b"<svg/>").await;
    write(root.path(), "public/favicon.png", b"png").await;

    let resolver = ProjectFaviconResolver;
    let resolved = resolver.resolve_path(root.path()).await.expect("favicon");
    assert!(
        resolved
            .expect("preferred favicon")
            .ends_with("public/favicon.png")
    );

    tokio::fs::remove_file(root.path().join("public/favicon.png"))
        .await
        .expect("remove preferred icon");
    let resolved = resolver
        .resolve_path(root.path())
        .await
        .expect("metadata icon");
    assert!(
        resolved
            .expect("metadata favicon")
            .ends_with("public/brand/logo.svg")
    );
}

struct FailingReviewBackend;

impl ReviewBackend for FailingReviewBackend {
    fn get_diff_preview<'a>(
        &'a self,
        _input: &'a ReviewDiffPreviewInput,
    ) -> review::ReviewFuture<'a> {
        Box::pin(async { Err(ReviewError::Backend("fixture failure".to_owned())) })
    }
}
struct EmptyReviewBackend;

impl ReviewBackend for EmptyReviewBackend {
    fn get_diff_preview<'a>(
        &'a self,
        _input: &'a ReviewDiffPreviewInput,
    ) -> review::ReviewFuture<'a> {
        Box::pin(async { Ok(None) })
    }
}

#[tokio::test]
async fn review_accepts_projects_outside_server_root_and_returns_empty_sources() {
    let outside = TempDir::new().expect("outside");
    let service = ReviewService::new(Arc::new(EmptyReviewBackend));
    let result = service
        .get_diff_preview(ReviewDiffPreviewInput {
            cwd: path_string(outside.path()),
            base_ref: None,
            ignore_whitespace: None,
        })
        .await
        .expect("review");

    assert_eq!(result.cwd, path_string(outside.path()));
    assert!(result.sources.is_empty());
}

#[test]
fn owned_rpc_inventory_matches_task_six_contract_methods() {
    assert_eq!(
        workspace::TASK_SIX_RPC_METHODS,
        [
            "projects.searchEntries",
            "projects.listEntries",
            "projects.readFile",
            "projects.writeFile",
            "projects.createEntry",
            "projects.renameEntry",
            "projects.deleteEntry",
            "projects.duplicateEntry",
            "projects.createDownloadUrl",
            "projects.createUploadUrl",
            "filesystem.browse",
            "assets.createUrl",
            "review.getDiffPreview",
        ]
    );
}

#[test]
fn fixture_paths_are_language_neutral() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/contracts/fixtures/workspace/task6-cases.json");
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture).expect("fixture")).expect("json");
    assert_eq!(value["readLimitBytes"], 1024 * 1024);
    assert_eq!(value["maxSearchLimit"], 200);
}

#[cfg(unix)]
#[tokio::test]
async fn public_workspace_service_maps_filesystem_failures() {
    use std::os::unix::fs::PermissionsExt;

    let root = TempDir::new().expect("root");
    let service = WorkspaceService::default();
    let blocker = root.path().join("blocker");
    std::fs::write(&blocker, "not a directory").expect("blocker");

    for result in [
        service
            .write_file(root.path(), "blocker/file.txt", "contents")
            .await
            .map(|_| ()),
        service
            .create_entry(root.path(), "blocker/directory", EntryKind::Directory)
            .await
            .map(|_| ()),
        service
            .create_entry(root.path(), "blocker/file.txt", EntryKind::File)
            .await
            .map(|_| ()),
        service
            .rename_entry(root.path(), "blocker", "blocker/renamed")
            .await
            .map(|_| ()),
    ] {
        assert!(matches!(result, Err(WorkspaceError::Operation { .. })));
    }

    std::fs::create_dir(root.path().join("write-target")).expect("write target");
    assert!(matches!(
        service
            .write_file(root.path(), "write-target", "contents")
            .await,
        Err(WorkspaceError::Operation { .. })
    ));

    let locked = root.path().join("locked");
    std::fs::create_dir(&locked).expect("locked directory");
    let mut locked_permissions = std::fs::metadata(&locked).unwrap().permissions();
    locked_permissions.set_mode(0o500);
    std::fs::set_permissions(&locked, locked_permissions).expect("lock directory");
    for result in [
        service
            .create_entry(
                root.path(),
                "locked/missing/directory",
                EntryKind::Directory,
            )
            .await,
        service
            .create_entry(root.path(), "locked/missing/file.txt", EntryKind::File)
            .await,
        service
            .rename_entry(root.path(), "blocker", "locked/missing/renamed")
            .await,
    ] {
        assert!(matches!(result, Err(WorkspaceError::Operation { .. })));
    }
    let mut locked_permissions = std::fs::metadata(&locked).unwrap().permissions();
    locked_permissions.set_mode(0o700);
    std::fs::set_permissions(&locked, locked_permissions).expect("unlock directory");

    std::fs::write(root.path().join("source.txt"), "copy source").expect("source");
    let mut source_permissions = std::fs::metadata(root.path().join("source.txt"))
        .unwrap()
        .permissions();
    source_permissions.set_mode(0o000);
    std::fs::set_permissions(root.path().join("source.txt"), source_permissions)
        .expect("lock source");
    let copy_result = service.duplicate_entry(root.path(), "source.txt").await;
    let mut source_permissions = std::fs::metadata(root.path().join("source.txt"))
        .unwrap()
        .permissions();
    source_permissions.set_mode(0o600);
    std::fs::set_permissions(root.path().join("source.txt"), source_permissions)
        .expect("unlock source");
    assert!(matches!(copy_result, Err(WorkspaceError::Operation { .. })));

    for entry in ["rename-source", "delete-file"] {
        std::fs::write(root.path().join(entry), entry).expect("fixture file");
    }
    std::fs::create_dir(root.path().join("delete-directory")).expect("fixture directory");
    let mut root_permissions = std::fs::metadata(root.path()).unwrap().permissions();
    root_permissions.set_mode(0o500);
    std::fs::set_permissions(root.path(), root_permissions).expect("lock root");
    for result in [
        service
            .rename_entry(root.path(), "rename-source", "rename-target")
            .await,
        service.delete_entry(root.path(), "delete-file").await,
        service.delete_entry(root.path(), "delete-directory").await,
    ] {
        assert!(matches!(result, Err(WorkspaceError::Operation { .. })));
    }
    let mut root_permissions = std::fs::metadata(root.path()).unwrap().permissions();
    root_permissions.set_mode(0o700);
    std::fs::set_permissions(root.path(), root_permissions).expect("unlock root");
}

#[tokio::test]
async fn transfer_urls_are_minted_for_files_folders_and_root_and_reject_escapes() {
    let temp = TempDir::new().expect("root");
    let root = temp.path().to_path_buf();
    let rpc = WorkspaceRpc::with_dependencies(
        WorkspaceService::default(),
        WorkspaceRpcDependencies {
            transfer_access: Some(bibcode_server::transfer::TransferAccess::new(
                b"secret".to_vec(),
            )),
            ..WorkspaceRpcDependencies::default()
        },
    );
    let unary = |method: &'static str, payload: serde_json::Value| rpc.handle(method, payload);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/app.ts"), "x").unwrap();
    let cwd = path_string(&root);

    let file = unary(
        "projects.createDownloadUrl",
        json!({"cwd": cwd, "relativePath": "src/app.ts"}),
    )
    .await
    .unwrap();
    assert_eq!(file["kind"], "file");
    assert_eq!(file["fileName"], "app.ts");
    assert!(
        file["relativeUrl"]
            .as_str()
            .unwrap()
            .starts_with("/api/transfers/")
    );

    let folder = unary(
        "projects.createDownloadUrl",
        json!({"cwd": cwd, "relativePath": "src"}),
    )
    .await
    .unwrap();
    assert_eq!(folder["kind"], "archive");
    assert_eq!(folder["fileName"], "src.zip");

    let upload_root = unary(
        "projects.createUploadUrl",
        json!({"cwd": cwd, "relativeDirectory": "", "fileName": "notes.md"}),
    )
    .await
    .unwrap();
    assert_eq!(upload_root["maxBytes"], 1024 * 1024 * 1024);
    let upload_dir = unary(
        "projects.createUploadUrl",
        json!({"cwd": cwd, "relativeDirectory": "src", "fileName": "notes.md"}),
    )
    .await
    .unwrap();
    assert!(
        upload_dir["relativeUrl"]
            .as_str()
            .unwrap()
            .starts_with("/api/transfers/")
    );

    let escape = unary(
        "projects.createDownloadUrl",
        json!({"cwd": cwd, "relativePath": "../etc"}),
    )
    .await
    .unwrap_err();
    assert_eq!(escape["_tag"], "ProjectTransferError");
    assert_eq!(escape["failure"], "outside_root");
    let missing = unary(
        "projects.createDownloadUrl",
        json!({"cwd": cwd, "relativePath": "nope"}),
    )
    .await
    .unwrap_err();
    assert_eq!(missing["failure"], "not_found");
    let file_as_dir = unary(
        "projects.createUploadUrl",
        json!({"cwd": cwd, "relativeDirectory": "src/app.ts", "fileName": "notes.md"}),
    )
    .await
    .unwrap_err();
    assert_eq!(file_as_dir["failure"], "not_found");
}

#[tokio::test]
async fn minting_an_upload_refuses_a_name_that_is_not_a_plain_file_name() {
    let temp = TempDir::new().expect("root");
    let root = temp.path().to_path_buf();
    let rpc = WorkspaceRpc::with_dependencies(
        WorkspaceService::default(),
        WorkspaceRpcDependencies {
            transfer_access: Some(bibcode_server::transfer::TransferAccess::new(
                b"secret".to_vec(),
            )),
            ..WorkspaceRpcDependencies::default()
        },
    );
    let cwd = path_string(&root);
    // The token carries the name, so a name the write would refuse must never reach a token.
    for bad in ["..", "../escape", "nested/name.txt", "back\\slash.txt"] {
        let refused = rpc
            .handle(
                "projects.createUploadUrl",
                json!({"cwd": cwd, "relativeDirectory": "", "fileName": bad}),
            )
            .await
            .unwrap_err();
        assert_eq!(refused["_tag"], "ProjectTransferError", "{bad}");
        assert_eq!(refused["failure"], "operation_failed", "{bad}");
        assert_eq!(refused["relativePath"], bad, "{bad}");
        assert!(
            refused["message"]
                .as_str()
                .unwrap()
                .contains("plain file name"),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn minting_a_folder_download_refuses_a_tree_over_the_archive_limits() {
    let temp = TempDir::new().expect("root");
    let root = temp.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/app.ts"), "0123456789").unwrap();
    let cwd = path_string(&root);
    let rpc_with = |limits: bibcode_server::transfer::archive::ArchiveLimits| {
        WorkspaceRpc::with_dependencies(
            WorkspaceService::default(),
            WorkspaceRpcDependencies {
                transfer_access: Some(bibcode_server::transfer::TransferAccess::new(
                    b"secret".to_vec(),
                )),
                archive_limits: limits,
                ..WorkspaceRpcDependencies::default()
            },
        )
    };

    // The person sees why the download was refused, at mint time, in the panel's toast --
    // rather than a bare HTTP status once the transfer is already under way.
    let too_many_bytes = rpc_with(bibcode_server::transfer::archive::ArchiveLimits {
        max_entries: 1_000,
        max_bytes: 2 * 1024 * 1024 * 1024,
    });
    // A folder that fits still mints.
    assert_eq!(
        too_many_bytes
            .handle(
                "projects.createDownloadUrl",
                json!({"cwd": cwd, "relativePath": "src"}),
            )
            .await
            .unwrap()["kind"],
        "archive"
    );

    let over_bytes = rpc_with(bibcode_server::transfer::archive::ArchiveLimits {
        max_entries: 1_000,
        max_bytes: 4,
    })
    .handle(
        "projects.createDownloadUrl",
        json!({"cwd": cwd, "relativePath": "src"}),
    )
    .await
    .unwrap_err();
    assert_eq!(over_bytes["_tag"], "ProjectTransferError");
    assert_eq!(over_bytes["failure"], "operation_failed");
    assert_eq!(
        over_bytes["message"],
        "Folder exceeds the 4 bytes download limit. Download a subfolder instead."
    );

    let over_entries = rpc_with(bibcode_server::transfer::archive::ArchiveLimits {
        max_entries: 0,
        max_bytes: 2 * 1024 * 1024 * 1024,
    })
    .handle(
        "projects.createDownloadUrl",
        json!({"cwd": cwd, "relativePath": "src"}),
    )
    .await
    .unwrap_err();
    assert_eq!(over_entries["failure"], "operation_failed");
    assert_eq!(
        over_entries["message"],
        "Folder exceeds the 0-entry download limit. Download a subfolder instead."
    );

    // A single file never runs the walk, so a tiny archive budget cannot refuse it.
    assert_eq!(
        rpc_with(bibcode_server::transfer::archive::ArchiveLimits {
            max_entries: 0,
            max_bytes: 0,
        })
        .handle(
            "projects.createDownloadUrl",
            json!({"cwd": cwd, "relativePath": "src/app.ts"}),
        )
        .await
        .unwrap()["kind"],
        "file"
    );
}
