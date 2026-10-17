use bibcode_server::production::orchestration_effects;

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

use bibcode_server::{
    git::{BoxWorktreeBaseDirectoryFuture, GitRepository, WorktreeBaseDirectoryProvider},
    orchestration::engine::{EngineOptions, OrchestrationCommand, OrchestrationEngine},
    persistence::{Database, OrchestrationEvent, run_migrations},
    production::host_paths::process_compatible_path,
};
use orchestration_effects::{
    BoxEffectFuture, EffectsOptions, OrchestrationEffectCallbacks, OrchestrationEffects,
    ProviderRewind, SetupScriptLaunch, normalize_project_workspace_root,
};
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::broadcast;

const NOW: &str = "2026-07-10T10:00:00.000Z";
const ORCHESTRATION_EFFECTS_INTEGRATION_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct StaticWorktreeBaseDirectory(Option<PathBuf>);

impl WorktreeBaseDirectoryProvider for StaticWorktreeBaseDirectory {
    fn worktree_base_directory<'a>(&'a self) -> BoxWorktreeBaseDirectoryFuture<'a> {
        let value = self.0.clone();
        Box::pin(async move { value })
    }
}

#[derive(Default)]
struct CallbackState {
    cwd: Mutex<Option<PathBuf>>,
    rollbacks: Mutex<Vec<(String, i64)>>,
    rewind_keys: Mutex<Vec<String>>,
    stopped: Mutex<Vec<String>>,
    terminals: Mutex<Vec<String>>,
    refreshed: Mutex<Vec<PathBuf>>,
    refresh_error: Mutex<Option<String>>,
    setup_scripts: Mutex<Vec<SetupScriptLaunch>>,
    setup_error: Mutex<Option<String>>,
}

impl OrchestrationEffectCallbacks for CallbackState {
    fn workspace_for_thread<'a>(
        &'a self,
        _thread_id: &'a str,
    ) -> BoxEffectFuture<'a, Option<PathBuf>> {
        Box::pin(async move { Ok(self.cwd.lock().unwrap().clone()) })
    }

    fn rollback_provider<'a>(
        &'a self,
        thread_id: &'a str,
        turns: i64,
        rewind_key: &'a str,
    ) -> BoxEffectFuture<'a, ProviderRewind> {
        Box::pin(async move {
            self.rollbacks
                .lock()
                .unwrap()
                .push((thread_id.to_owned(), turns));
            self.rewind_keys.lock().unwrap().push(rewind_key.to_owned());
            Ok(ProviderRewind::Rewound)
        })
    }

    fn stop_provider<'a>(&'a self, thread_id: &'a str) -> BoxEffectFuture<'a, ()> {
        Box::pin(async move {
            self.stopped.lock().unwrap().push(thread_id.to_owned());
            Ok(())
        })
    }

    fn close_terminals<'a>(&'a self, thread_id: &'a str) -> BoxEffectFuture<'a, ()> {
        Box::pin(async move {
            self.terminals.lock().unwrap().push(thread_id.to_owned());
            Ok(())
        })
    }

    fn refresh_workspace<'a>(&'a self, cwd: &'a Path) -> BoxEffectFuture<'a, ()> {
        Box::pin(async move {
            self.refreshed.lock().unwrap().push(cwd.to_path_buf());
            if let Some(error) = self.refresh_error.lock().unwrap().clone() {
                return Err(error);
            }
            Ok(())
        })
    }

    fn setup_script_is_running<'a>(
        &'a self,
        thread_id: &'a str,
        terminal_id: &'a str,
    ) -> BoxEffectFuture<'a, bool> {
        Box::pin(async move {
            Ok(self
                .setup_scripts
                .lock()
                .unwrap()
                .iter()
                .any(|launch| launch.thread_id == thread_id && launch.terminal_id == terminal_id))
        })
    }

    fn launch_setup_script<'a>(&'a self, input: SetupScriptLaunch) -> BoxEffectFuture<'a, ()> {
        Box::pin(async move {
            self.setup_scripts.lock().unwrap().push(input);
            if let Some(error) = self.setup_error.lock().unwrap().clone() {
                return Err(error);
            }
            Ok(())
        })
    }
}

async fn engine(workspace: &Path) -> OrchestrationEngine {
    let database = Database::open_in_memory().await.unwrap();
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .unwrap();
    let engine = OrchestrationEngine::start(database, EngineOptions::default())
        .await
        .unwrap();
    dispatch(
        &engine,
        json!({
            "type":"project.create", "commandId":"project", "projectId":"p1",
            "title":"Project", "workspaceRoot":workspace, "createdAt":NOW
        }),
    )
    .await;
    dispatch(
        &engine,
        json!({
            "type":"thread.create", "commandId":"thread", "threadId":"t1", "projectId":"p1",
            "title":"Thread", "modelSelection":{"instanceId":"codex","model":"gpt-5"},
            "runtimeMode":"full-access", "branch":null, "worktreePath":null, "createdAt":NOW
        }),
    )
    .await;
    engine
}

async fn dispatch(engine: &OrchestrationEngine, value: serde_json::Value) {
    let command: OrchestrationCommand = serde_json::from_value(value).unwrap();
    engine.dispatch(command).await.unwrap();
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn git_succeeds(cwd: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap()
        .success()
}

struct RealGitFixture {
    directory: TempDir,
}

impl RealGitFixture {
    fn path(&self) -> &Path {
        self.directory.path()
    }
}

async fn initialize_repository() -> RealGitFixture {
    let directory = tempfile::tempdir().unwrap();
    git(directory.path(), &["init"]);
    git(directory.path(), &["config", "user.name", "BiBCode Test"]);
    git(
        directory.path(),
        &["config", "user.email", "bibcode@example.test"],
    );
    std::fs::write(directory.path().join("tracked.txt"), "baseline\n").unwrap();
    git(directory.path(), &["add", "."]);
    git(directory.path(), &["commit", "-m", "baseline"]);
    RealGitFixture { directory }
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    for _ in 0..100 {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("condition was not met");
}

async fn wait_for_git_ref(cwd: &Path, reference: &str) {
    let cwd = cwd.to_path_buf();
    let reference = reference.to_owned();
    let diagnostic_reference = reference.clone();
    tokio::time::timeout(ORCHESTRATION_EFFECTS_INTEGRATION_DEADLINE, async move {
        loop {
            let probe_cwd = cwd.clone();
            let probe_reference = reference.clone();
            let exists = tokio::task::spawn_blocking(move || {
                git_succeeds(
                    &probe_cwd,
                    &["rev-parse", "--verify", "--quiet", &probe_reference],
                )
            })
            .await
            .expect("Git ref probe task completes");
            if exists {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("Git ref {diagnostic_reference} was not created before the fixture deadline")
    });
}

async fn wait_for_event(
    receiver: &mut broadcast::Receiver<OrchestrationEvent>,
    event_type: &str,
    predicate: impl Fn(&OrchestrationEvent) -> bool,
) -> OrchestrationEvent {
    tokio::time::timeout(ORCHESTRATION_EFFECTS_INTEGRATION_DEADLINE, async {
        loop {
            let event = receiver
                .recv()
                .await
                .unwrap_or_else(|error| panic!("orchestration event stream closed: {error}"));
            if event.event.event_type == event_type && predicate(&event) {
                return event;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("event {event_type} was not emitted before the fixture deadline"))
}

#[tokio::test]
async fn normalizes_and_optionally_creates_project_workspace_roots() {
    let parent = tempfile::tempdir().unwrap();
    let missing = parent.path().join("nested").join("project");

    let error = normalize_project_workspace_root(&missing, false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("does not exist"));

    let normalized = normalize_project_workspace_root(&missing, true)
        .await
        .unwrap();
    assert!(normalized.is_absolute());
    assert!(normalized.is_dir());
    #[cfg(windows)]
    assert!(
        !normalized.to_string_lossy().starts_with(r"\\?\"),
        "persisted workspace paths must be accepted by Git and terminal processes"
    );

    let file = parent.path().join("not-a-directory");
    std::fs::write(&file, "x").unwrap();
    let error = normalize_project_workspace_root(&file, false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not a directory"));
}

#[tokio::test]
async fn bootstrap_admission_does_not_use_the_injected_workspace_before_delivery() {
    let source = initialize_repository().await;
    let workspace = tempfile::tempdir().expect("worktree workspace");
    let canonical_workspace = process_compatible_path(
        workspace
            .path()
            .canonicalize()
            .expect("canonical workspace"),
    );
    let repository = Arc::new(GitRepository::with_worktree_settings(
        Arc::new(StaticWorktreeBaseDirectory(Some(
            workspace.path().to_path_buf(),
        ))),
        Arc::default(),
    ));
    let engine = engine(source.path()).await;
    let callbacks = Arc::new(CallbackState::default());
    let effects = OrchestrationEffects::start(
        engine.clone(),
        repository.clone(),
        callbacks,
        EffectsOptions::default(),
    )
    .await
    .expect("effects");

    dispatch(
        &engine,
        json!({
            "type":"thread.turn.start", "commandId":"workspace-bootstrap",
            "threadId":"workspace-thread",
            "message":{
                "messageId":"workspace-message", "role":"user", "text":"change it",
                "attachments":[]
            },
            "bootstrap":{
                "createThread":{
                    "projectId":"p1", "title":"Workspace thread",
                    "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                    "runtimeMode":"full-access", "interactionMode":"default",
                    "branch":null, "worktreePath":null, "createdAt":NOW
                },
                "prepareWorktree":{
                    "projectCwd":source.path(), "baseBranch":"HEAD",
                    "branch":"bibcode/workspace-test"
                },
                "runSetupScript":false
            },
            "createdAt":NOW
        }),
    )
    .await;
    let thread = engine
        .repositories()
        .get_thread("workspace-thread".to_owned())
        .await
        .expect("thread query")
        .expect("thread");
    assert!(thread.worktree_path.is_none());
    assert!(
        !git(source.path(), &["worktree", "list", "--porcelain"])
            .replace('\\', "/")
            .contains(canonical_workspace.to_string_lossy().as_ref())
    );
    effects.shutdown().await;
    engine.shutdown().await;
}

#[tokio::test]
async fn bootstrap_admission_persists_turn_without_running_prerequisites() {
    let repository = initialize_repository().await;
    let engine = engine(repository.path()).await;
    dispatch(
        &engine,
        json!({
            "type":"project.meta.update", "commandId":"scripts", "projectId":"p1",
            "scripts":[{
                "id":"setup", "name":"Install dependencies", "command":"vp install",
                "runOnWorktreeCreate":true
            }]
        }),
    )
    .await;
    let callbacks = Arc::new(CallbackState::default());
    let effects = OrchestrationEffects::start(
        engine.clone(),
        Arc::new(GitRepository::default()),
        callbacks.clone(),
        EffectsOptions::default(),
    )
    .await
    .unwrap();

    dispatch(
        &engine,
        json!({
            "type":"thread.turn.start", "commandId":"bootstrap", "threadId":"worktree-thread",
            "message":{"messageId":"message","role":"user","text":"change it","attachments":[]},
            "bootstrap":{
                "createThread":{
                    "projectId":"p1", "title":"Worktree thread",
                    "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                    "runtimeMode":"full-access", "interactionMode":"default",
                    "branch":null, "worktreePath":null, "createdAt":NOW
                },
                "prepareWorktree":{
                    "projectCwd":repository.path(), "baseBranch":"HEAD",
                    "branch":"bibcode/bootstrap-test"
                },
                "runSetupScript":true
            },
            "createdAt":NOW
        }),
    )
    .await;

    let thread = engine
        .repositories()
        .get_thread("worktree-thread".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert!(thread.branch.is_none());
    assert!(thread.worktree_path.is_none());

    {
        let setup_scripts = callbacks.setup_scripts.lock().unwrap();
        assert!(setup_scripts.is_empty());
    }

    let events = engine.read_events(0).await.unwrap();
    let thread_events: Vec<_> = events
        .iter()
        .filter(|event| event.event.aggregate_id == "worktree-thread")
        .map(|event| event.event.event_type.as_str())
        .collect();
    assert_eq!(
        thread_events,
        vec![
            "thread.created",
            "thread.message-sent",
            "thread.turn-start-requested"
        ]
    );

    effects.shutdown().await;
    engine.shutdown().await;
}

#[tokio::test]
async fn bootstrap_admission_does_not_run_failing_setup_before_delivery() {
    let repository = initialize_repository().await;
    let engine = engine(repository.path()).await;
    dispatch(
        &engine,
        json!({
            "type":"project.meta.update", "commandId":"scripts", "projectId":"p1",
            "scripts":[{
                "id":"setup", "name":"Install dependencies", "command":"vp install",
                "runOnWorktreeCreate":true
            }]
        }),
    )
    .await;
    let callbacks = Arc::new(CallbackState::default());
    *callbacks.setup_error.lock().unwrap() = Some("terminal start failed".to_owned());
    let effects = OrchestrationEffects::start(
        engine.clone(),
        Arc::new(GitRepository::default()),
        callbacks,
        EffectsOptions::default(),
    )
    .await
    .unwrap();

    let command: OrchestrationCommand = serde_json::from_value(json!({
        "type":"thread.turn.start", "commandId":"bootstrap", "threadId":"setup-failure",
        "message":{"messageId":"message","role":"user","text":"change it","attachments":[]},
        "bootstrap":{
            "createThread":{
                "projectId":"p1", "title":"Setup failure",
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access", "interactionMode":"default",
                "branch":null, "worktreePath":null, "createdAt":NOW
            },
            "prepareWorktree":{
                "projectCwd":repository.path(), "baseBranch":"HEAD",
                "branch":"bibcode/setup-failure-test"
            },
            "runSetupScript":true
        },
        "createdAt":NOW
    }))
    .unwrap();
    engine
        .dispatch(command)
        .await
        .expect("bootstrap is admitted");

    let events = engine.read_events(0).await.unwrap();
    assert!(events.iter().any(|event| {
        event.event.aggregate_id == "setup-failure"
            && event.event.event_type == "thread.turn-start-requested"
    }));
    assert!(!events.iter().any(|event| {
        event.event.aggregate_id == "setup-failure"
            && event.event.payload["activity"]["kind"] == "setup-script.failed"
    }));

    let thread = engine
        .repositories()
        .get_thread("setup-failure".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert!(thread.deleted_at.is_none());
    let worktrees = git(repository.path(), &["worktree", "list", "--porcelain"]);
    assert!(!worktrees.contains("bibcode/setup-failure-test"));
    assert!(
        git(
            repository.path(),
            &["branch", "--list", "bibcode/setup-failure-test"]
        )
        .is_empty()
    );
    effects.shutdown().await;
    engine.shutdown().await;
}

#[tokio::test]
async fn bootstrap_admission_does_not_refresh_workspace_before_delivery() {
    let repository = initialize_repository().await;
    let engine = engine(repository.path()).await;
    let callbacks = Arc::new(CallbackState::default());
    *callbacks.refresh_error.lock().unwrap() = Some("index refresh failed".to_owned());
    let effects = OrchestrationEffects::start(
        engine.clone(),
        Arc::new(GitRepository::default()),
        callbacks,
        EffectsOptions::default(),
    )
    .await
    .unwrap();
    let command: OrchestrationCommand = serde_json::from_value(json!({
        "type":"thread.turn.start", "commandId":"bootstrap", "threadId":"refresh-failure",
        "message":{"messageId":"message","role":"user","text":"change it","attachments":[]},
        "bootstrap":{
            "createThread":{
                "projectId":"p1", "title":"Refresh failure",
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access", "interactionMode":"default",
                "branch":null, "worktreePath":null, "createdAt":NOW
            },
            "prepareWorktree":{
                "projectCwd":repository.path(), "baseBranch":"HEAD",
                "branch":"bibcode/refresh-failure-test"
            }
        },
        "createdAt":NOW
    }))
    .unwrap();

    engine
        .dispatch(command)
        .await
        .expect("bootstrap is admitted");
    let worktrees = git(repository.path(), &["worktree", "list", "--porcelain"]);
    assert!(!worktrees.contains("bibcode/refresh-failure-test"));
    assert!(
        git(
            repository.path(),
            &["branch", "--list", "bibcode/refresh-failure-test"]
        )
        .is_empty()
    );
    let thread = engine
        .repositories()
        .get_thread("refresh-failure".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert!(thread.deleted_at.is_none());

    effects.shutdown().await;
    engine.shutdown().await;
}

#[tokio::test]
async fn bootstrap_admission_does_not_fetch_origin_before_delivery() {
    let repository = initialize_repository().await;
    let engine = engine(repository.path()).await;
    let effects = OrchestrationEffects::start(
        engine.clone(),
        Arc::new(GitRepository::default()),
        Arc::new(CallbackState::default()),
        EffectsOptions::default(),
    )
    .await
    .unwrap();
    let command: OrchestrationCommand = serde_json::from_value(json!({
        "type":"thread.turn.start", "commandId":"bootstrap", "threadId":"fetch-failure",
        "message":{"messageId":"message","role":"user","text":"change it","attachments":[]},
        "bootstrap":{
            "createThread":{
                "projectId":"p1", "title":"Fetch failure",
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access", "interactionMode":"default",
                "branch":null, "worktreePath":null, "createdAt":NOW
            },
            "prepareWorktree":{
                "projectCwd":repository.path(), "baseBranch":"main",
                "branch":"bibcode/fetch-failure-test", "startFromOrigin":true
            }
        },
        "createdAt":NOW
    }))
    .unwrap();

    engine
        .dispatch(command)
        .await
        .expect("bootstrap is admitted");
    assert!(
        engine
            .repositories()
            .get_thread("fetch-failure".to_owned())
            .await
            .expect("thread")
            .expect("persisted thread")
            .deleted_at
            .is_none()
    );
    assert!(
        git(
            repository.path(),
            &["branch", "--list", "bibcode/fetch-failure-test"]
        )
        .is_empty()
    );

    effects.shutdown().await;
    engine.shutdown().await;
}

#[tokio::test]
async fn captures_baseline_and_replaces_missing_turn_checkpoint_with_real_diff() {
    let repository = initialize_repository().await;
    let engine = engine(repository.path()).await;
    let callbacks = Arc::new(CallbackState::default());
    *callbacks.cwd.lock().unwrap() = Some(repository.path().to_path_buf());
    let effects = OrchestrationEffects::start(
        engine.clone(),
        Arc::new(GitRepository::default()),
        callbacks,
        EffectsOptions::default(),
    )
    .await
    .unwrap();

    dispatch(
        &engine,
        json!({
            "type":"thread.turn.start", "commandId":"turn-start", "threadId":"t1",
            "message":{"messageId":"m1","role":"user","text":"change it","attachments":[]},
            "createdAt":NOW
        }),
    )
    .await;
    let baseline_ref = orchestration_effects::checkpoint_ref("t1", 0);
    wait_for_git_ref(repository.path(), &baseline_ref).await;

    std::fs::write(repository.path().join("tracked.txt"), "changed\n").unwrap();
    std::fs::write(repository.path().join("new.txt"), "new\n").unwrap();
    let checkpoint_ref = orchestration_effects::checkpoint_ref("t1", 1);
    let mut events = engine.subscribe_events();
    dispatch(
        &engine,
        json!({
            "type":"thread.turn.diff.complete", "commandId":"placeholder", "threadId":"t1",
            "turnId":"turn-1", "checkpointTurnCount":1,
            "checkpointRef":"missing", "status":"missing", "files":[],
            "assistantMessageId":"assistant-1", "completedAt":NOW, "createdAt":NOW
        }),
    )
    .await;
    wait_for_event(&mut events, "thread.turn-diff-completed", |event| {
        event.event.payload["threadId"] == "t1"
            && event.event.payload["status"] == "ready"
            && event.event.payload["checkpointRef"] == checkpoint_ref
    })
    .await;

    let checkpoint = engine
        .repositories()
        .get_checkpoint("t1".to_owned(), 1)
        .await
        .unwrap()
        .expect("real checkpoint was projected");
    assert_eq!(checkpoint.status, "ready");
    assert_eq!(checkpoint.checkpoint_ref, checkpoint_ref);
    let files = checkpoint.files;
    assert!(
        files
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["path"] == "tracked.txt")
    );
    assert!(
        files
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["path"] == "new.txt")
    );
    effects.shutdown().await;
    engine.shutdown().await;
}

#[tokio::test]
async fn reverts_workspace_provider_history_and_stale_checkpoint_refs() {
    let repository = initialize_repository().await;
    let engine = engine(repository.path()).await;
    let callbacks = Arc::new(CallbackState::default());
    *callbacks.cwd.lock().unwrap() = Some(repository.path().to_path_buf());
    let effects = OrchestrationEffects::start(
        engine.clone(),
        Arc::new(GitRepository::default()),
        callbacks.clone(),
        EffectsOptions::default(),
    )
    .await
    .unwrap();

    orchestration_effects::capture_checkpoint(repository.path(), "t1", 0)
        .await
        .unwrap();
    std::fs::write(repository.path().join("tracked.txt"), "one\n").unwrap();
    orchestration_effects::capture_checkpoint(repository.path(), "t1", 1)
        .await
        .unwrap();
    std::fs::write(repository.path().join("tracked.txt"), "two\n").unwrap();
    orchestration_effects::capture_checkpoint(repository.path(), "t1", 2)
        .await
        .unwrap();
    for turn_count in 1..=2 {
        dispatch(
            &engine,
            json!({
                "type":"thread.turn.diff.complete", "commandId":format!("diff-{turn_count}"),
                "threadId":"t1", "turnId":format!("turn-{turn_count}"),
                "checkpointTurnCount":turn_count,
                "checkpointRef":orchestration_effects::checkpoint_ref("t1", turn_count),
                "status":"ready", "files":[], "assistantMessageId":format!("a-{turn_count}"),
                "completedAt":NOW, "createdAt":NOW
            }),
        )
        .await;
    }

    let mut events = engine.subscribe_events();
    dispatch(
        &engine,
        json!({
            "type":"thread.checkpoint.revert", "commandId":"revert", "threadId":"t1",
            "turnCount":1, "createdAt":NOW
        }),
    )
    .await;
    wait_for_event(&mut events, "thread.reverted", |event| {
        event.event.payload["threadId"] == "t1" && event.event.payload["turnCount"] == 1
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(repository.path().join("tracked.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "one\n"
    );
    assert_eq!(
        callbacks.rollbacks.lock().unwrap().as_slice(),
        &[("t1".to_owned(), 1)]
    );
    // The rewind is keyed by the transcript it counts from, named by its newest turn.
    assert_eq!(
        callbacks.rewind_keys.lock().unwrap().as_slice(),
        &["t1/turn-2".to_owned()]
    );
    assert!(!git_succeeds(
        repository.path(),
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &orchestration_effects::checkpoint_ref("t1", 2)
        ],
    ));

    effects.shutdown().await;
    engine.shutdown().await;
}

/// A running turn would write over the restored files, so the server refuses the revert itself,
/// whichever client asked for it.
#[tokio::test]
async fn revert_during_a_running_turn_changes_nothing() {
    let repository = initialize_repository().await;
    let engine = engine(repository.path()).await;
    let callbacks = Arc::new(CallbackState::default());
    *callbacks.cwd.lock().unwrap() = Some(repository.path().to_path_buf());
    let effects = OrchestrationEffects::start(
        engine.clone(),
        Arc::new(GitRepository::default()),
        callbacks.clone(),
        EffectsOptions::default(),
    )
    .await
    .unwrap();
    for (turn_count, content) in [(0, None), (1, Some("one\n")), (2, Some("two\n"))] {
        if let Some(content) = content {
            std::fs::write(repository.path().join("tracked.txt"), content).unwrap();
        }
        orchestration_effects::capture_checkpoint(repository.path(), "t1", turn_count)
            .await
            .unwrap();
    }
    for turn_count in 1..=2 {
        dispatch(
            &engine,
            json!({
                "type":"thread.turn.diff.complete", "commandId":format!("diff-{turn_count}"),
                "threadId":"t1", "turnId":format!("turn-{turn_count}"),
                "checkpointTurnCount":turn_count,
                "checkpointRef":orchestration_effects::checkpoint_ref("t1", turn_count),
                "status":"ready", "files":[], "assistantMessageId":format!("a-{turn_count}"),
                "completedAt":NOW, "createdAt":NOW
            }),
        )
        .await;
    }
    dispatch(
        &engine,
        json!({
            "type":"thread.session.set", "commandId":"running", "threadId":"t1",
            "session":{"threadId":"t1","status":"running","providerName":"claude",
                "activeTurnId":"turn-3","lastError":null,"updatedAt":NOW},
            "createdAt":NOW
        }),
    )
    .await;

    let mut events = engine.subscribe_events();
    dispatch(
        &engine,
        json!({
            "type":"thread.checkpoint.revert", "commandId":"revert", "threadId":"t1",
            "turnCount":1, "createdAt":NOW
        }),
    )
    .await;
    let failure = wait_for_event(&mut events, "thread.activity-appended", |event| {
        event.event.payload["activity"]["kind"] == "checkpoint.revert.failed"
    })
    .await;
    assert!(
        failure.event.payload["activity"]["payload"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("Interrupt the current turn")),
        "{failure:?}"
    );
    assert!(callbacks.rollbacks.lock().unwrap().is_empty());
    assert_eq!(
        std::fs::read_to_string(repository.path().join("tracked.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "two\n"
    );

    effects.shutdown().await;
    engine.shutdown().await;
}

/// The provider rewind cannot be undone, so a revert whose files cannot be restored fails
/// before the provider is asked to rewind.
#[tokio::test]
async fn revert_with_a_missing_checkpoint_fails_before_the_provider_rewinds() {
    let repository = initialize_repository().await;
    let engine = engine(repository.path()).await;
    let callbacks = Arc::new(CallbackState::default());
    *callbacks.cwd.lock().unwrap() = Some(repository.path().to_path_buf());
    let effects = OrchestrationEffects::start(
        engine.clone(),
        Arc::new(GitRepository::default()),
        callbacks.clone(),
        EffectsOptions::default(),
    )
    .await
    .unwrap();

    for (turn_count, content) in [(0, None), (1, Some("one\n")), (2, Some("two\n"))] {
        if let Some(content) = content {
            std::fs::write(repository.path().join("tracked.txt"), content).unwrap();
        }
        orchestration_effects::capture_checkpoint(repository.path(), "t1", turn_count)
            .await
            .unwrap();
    }
    for turn_count in 1..=2 {
        dispatch(
            &engine,
            json!({
                "type":"thread.turn.diff.complete", "commandId":format!("diff-{turn_count}"),
                "threadId":"t1", "turnId":format!("turn-{turn_count}"),
                "checkpointTurnCount":turn_count,
                "checkpointRef":orchestration_effects::checkpoint_ref("t1", turn_count),
                "status":"ready", "files":[], "assistantMessageId":format!("a-{turn_count}"),
                "completedAt":NOW, "createdAt":NOW
            }),
        )
        .await;
    }
    assert!(git_succeeds(
        repository.path(),
        &[
            "update-ref",
            "-d",
            &orchestration_effects::checkpoint_ref("t1", 1)
        ],
    ));

    let mut events = engine.subscribe_events();
    dispatch(
        &engine,
        json!({
            "type":"thread.checkpoint.revert", "commandId":"revert", "threadId":"t1",
            "turnCount":1, "createdAt":NOW
        }),
    )
    .await;
    let failure = wait_for_event(&mut events, "thread.activity-appended", |event| {
        event.event.payload["activity"]["kind"] == "checkpoint.revert.failed"
    })
    .await;
    assert!(
        failure.event.payload["activity"]["payload"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("Filesystem checkpoint is unavailable")),
        "{failure:?}"
    );
    assert!(callbacks.rollbacks.lock().unwrap().is_empty());
    assert_eq!(
        std::fs::read_to_string(repository.path().join("tracked.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "two\n"
    );

    effects.shutdown().await;
    engine.shutdown().await;
}

/// Once the provider has rewound, the revert is recorded even if Git then fails to restore the
/// files, so retrying cannot rewind the provider a second time.
#[tokio::test]
async fn revert_records_a_completed_rewind_when_files_fail_to_restore() {
    struct IndexLockingCallbacks {
        workspace: PathBuf,
        rollbacks: Mutex<Vec<(String, i64)>>,
    }
    impl OrchestrationEffectCallbacks for IndexLockingCallbacks {
        fn workspace_for_thread<'a>(&'a self, _: &'a str) -> BoxEffectFuture<'a, Option<PathBuf>> {
            Box::pin(async move { Ok(Some(self.workspace.clone())) })
        }
        fn rollback_provider<'a>(
            &'a self,
            thread_id: &'a str,
            turns: i64,
            _rewind_key: &'a str,
        ) -> BoxEffectFuture<'a, ProviderRewind> {
            Box::pin(async move {
                self.rollbacks
                    .lock()
                    .unwrap()
                    .push((thread_id.to_owned(), turns));
                // Another Git process holds the index, so the restore that follows fails.
                std::fs::write(self.workspace.join(".git").join("index.lock"), "").unwrap();
                Ok(ProviderRewind::Rewound)
            })
        }
        fn stop_provider<'a>(&'a self, _: &'a str) -> BoxEffectFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn close_terminals<'a>(&'a self, _: &'a str) -> BoxEffectFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn refresh_workspace<'a>(&'a self, _: &'a Path) -> BoxEffectFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    let repository = initialize_repository().await;
    let engine = engine(repository.path()).await;
    let callbacks = Arc::new(IndexLockingCallbacks {
        workspace: repository.path().to_path_buf(),
        rollbacks: Mutex::default(),
    });
    let effects = OrchestrationEffects::start(
        engine.clone(),
        Arc::new(GitRepository::default()),
        callbacks.clone(),
        EffectsOptions::default(),
    )
    .await
    .unwrap();
    for (turn_count, content) in [(0, None), (1, Some("one\n")), (2, Some("two\n"))] {
        if let Some(content) = content {
            std::fs::write(repository.path().join("tracked.txt"), content).unwrap();
        }
        orchestration_effects::capture_checkpoint(repository.path(), "t1", turn_count)
            .await
            .unwrap();
    }
    for turn_count in 1..=2 {
        dispatch(
            &engine,
            json!({
                "type":"thread.turn.diff.complete", "commandId":format!("diff-{turn_count}"),
                "threadId":"t1", "turnId":format!("turn-{turn_count}"),
                "checkpointTurnCount":turn_count,
                "checkpointRef":orchestration_effects::checkpoint_ref("t1", turn_count),
                "status":"ready", "files":[], "assistantMessageId":format!("a-{turn_count}"),
                "completedAt":NOW, "createdAt":NOW
            }),
        )
        .await;
    }

    let mut events = engine.subscribe_events();
    dispatch(
        &engine,
        json!({
            "type":"thread.checkpoint.revert", "commandId":"revert", "threadId":"t1",
            "turnCount":1, "createdAt":NOW
        }),
    )
    .await;
    wait_for_event(&mut events, "thread.reverted", |event| {
        event.event.payload["threadId"] == "t1" && event.event.payload["turnCount"] == 1
    })
    .await;
    let failure = wait_for_event(&mut events, "thread.activity-appended", |event| {
        event.event.payload["activity"]["kind"] == "checkpoint.revert.failed"
    })
    .await;
    assert!(
        failure.event.payload["activity"]["payload"]["detail"]
            .as_str()
            .is_some_and(|detail| detail
                .contains("The conversation was rewound, but the files could not be restored")),
        "{failure:?}"
    );
    assert_eq!(
        callbacks.rollbacks.lock().unwrap().as_slice(),
        &[("t1".to_owned(), 1)]
    );
    std::fs::remove_file(repository.path().join(".git").join("index.lock")).unwrap();
    assert_eq!(
        std::fs::read_to_string(repository.path().join("tracked.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "two\n"
    );
    // The recorded revert dropped turn 2 from the projection, so its ref goes too.
    assert!(!git_succeeds(
        repository.path(),
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &orchestration_effects::checkpoint_ref("t1", 2)
        ],
    ));

    effects.shutdown().await;
    engine.shutdown().await;
}

#[tokio::test]
async fn thread_deletion_attempts_provider_and_terminal_cleanup_independently() {
    struct FailingProviderCallbacks(CallbackState);
    impl OrchestrationEffectCallbacks for FailingProviderCallbacks {
        fn workspace_for_thread<'a>(&'a self, _: &'a str) -> BoxEffectFuture<'a, Option<PathBuf>> {
            Box::pin(async { Ok(None) })
        }
        fn rollback_provider<'a>(
            &'a self,
            _: &'a str,
            _: i64,
            _rewind_key: &'a str,
        ) -> BoxEffectFuture<'a, ProviderRewind> {
            Box::pin(async { Ok(ProviderRewind::Rewound) })
        }
        fn stop_provider<'a>(&'a self, thread_id: &'a str) -> BoxEffectFuture<'a, ()> {
            Box::pin(async move {
                self.0.stopped.lock().unwrap().push(thread_id.to_owned());
                Err("provider already stopped".to_owned())
            })
        }
        fn close_terminals<'a>(&'a self, thread_id: &'a str) -> BoxEffectFuture<'a, ()> {
            Box::pin(async move {
                self.0.terminals.lock().unwrap().push(thread_id.to_owned());
                Ok(())
            })
        }
        fn refresh_workspace<'a>(&'a self, _: &'a Path) -> BoxEffectFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    let workspace = tempfile::tempdir().unwrap();
    let engine = engine(workspace.path()).await;
    let callbacks = Arc::new(FailingProviderCallbacks(CallbackState::default()));
    let default_launch_error = callbacks
        .launch_setup_script(SetupScriptLaunch {
            thread_id: "t1".to_owned(),
            terminal_id: "setup-default".to_owned(),
            script_id: "default".to_owned(),
            script_name: "Default".to_owned(),
            command: "true".to_owned(),
            cwd: workspace.path().to_path_buf(),
            worktree_path: workspace.path().to_path_buf(),
            env: Default::default(),
        })
        .await
        .expect_err("default setup callback is unavailable");
    assert!(default_launch_error.contains("unavailable"));
    let effects = OrchestrationEffects::start(
        engine.clone(),
        Arc::new(GitRepository::default()),
        callbacks.clone(),
        EffectsOptions { queue_capacity: 1 },
    )
    .await
    .unwrap();

    dispatch(
        &engine,
        json!({
            "type":"thread.delete", "commandId":"delete", "threadId":"t1"
        }),
    )
    .await;
    wait_until(|| !callbacks.0.terminals.lock().unwrap().is_empty()).await;
    assert_eq!(callbacks.0.stopped.lock().unwrap().as_slice(), &["t1"]);
    assert_eq!(callbacks.0.terminals.lock().unwrap().as_slice(), &["t1"]);

    effects.shutdown().await;
    engine.shutdown().await;
}

#[tokio::test]
async fn created_projects_receive_their_repository_identity_once() {
    let checkout = tempfile::tempdir().unwrap();
    git(checkout.path(), &["init", "-q"]);
    git(
        checkout.path(),
        &["remote", "add", "origin", "git@github.com:acme/repo.git"],
    );
    let database = Database::open_in_memory().await.unwrap();
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .unwrap();
    let engine = OrchestrationEngine::start(database, EngineOptions::default())
        .await
        .unwrap();
    let effects = OrchestrationEffects::start(
        engine.clone(),
        Arc::new(GitRepository::default()),
        Arc::new(CallbackState::default()),
        EffectsOptions::default(),
    )
    .await
    .unwrap();

    dispatch(
        &engine,
        json!({
            "type":"project.create", "commandId":"project", "projectId":"p",
            "title":"Project", "workspaceRoot":checkout.path(), "createdAt":NOW
        }),
    )
    .await;

    let identity = tokio::time::timeout(ORCHESTRATION_EFFECTS_INTEGRATION_DEADLINE, async {
        loop {
            let project = engine
                .repositories()
                .get_project("p".to_owned())
                .await
                .unwrap()
                .unwrap();
            if let Some(identity) = project.repository_identity {
                return (identity, project.updated_at);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the repository identity was recorded before the fixture deadline");
    assert_eq!(identity.0["canonicalKey"], "github.com/acme/repo");
    assert_eq!(identity.1, NOW);

    // Give a spurious second reconcile time to show up before counting.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let identity_events = engine
        .repositories()
        .read_events_from_sequence(0, 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| {
            event.event.event_type == "project.meta-updated"
                && event.event.payload.get("repositoryIdentity").is_some()
        })
        .count();
    assert_eq!(identity_events, 1);
    effects.shutdown().await;
    engine.shutdown().await;
}
