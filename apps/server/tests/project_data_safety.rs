#[path = "support/hermetic_providers.rs"]
mod hermetic_providers;

use coddy_studio_server::{
    ServerConfig, ServerRuntime,
    persistence::{
        BackupTrigger, PreparedStore, RecoveryError, StatePaths, StoreClassification,
        StoreInspectionStatus, StoreStartupError, create_verified_backup, inspect_store,
        prepare_store, preserve_and_start_empty, restore_backup, run_migrations,
    },
    production::jwt::PersistentJwtCodec,
    resolve_data_root,
};
use reqwest::{Client, StatusCode};
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use tempfile::TempDir;
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::fs::symlink;

const CRASH_STORE_CHILD_ROOT: &str = "CODDY_STUDIO_CRASH_STORE_CHILD_ROOT";
const CRASH_STORE_CHILD_READY: &str = "CODDY_STUDIO_CRASH_STORE_CHILD_READY";

struct StoreFixture {
    _root: TempDir,
    config: ServerConfig,
    paths: StatePaths,
}

impl StoreFixture {
    fn new() -> Self {
        let root = TempDir::new().expect("temporary absolute data root");
        let mut config = ServerConfig::new(root.path());
        let resolved = resolve_data_root(config.data_root_request.clone()).expect("resolve root");
        config.base_dir = resolved.effective.clone();
        config.resolved_data_root = Some(resolved);
        let paths = StatePaths::from_config(&config);
        std::fs::create_dir_all(&paths.state_dir).expect("state directory");
        Self {
            _root: root,
            config,
            paths,
        }
    }

    async fn with_project(title: &str) -> Self {
        let fixture = Self::new();
        let mut connection = Connection::open(&fixture.paths.database).expect("fixture database");
        run_migrations(&mut connection, None).expect("fixture migrations");
        connection
            .execute(
                "INSERT INTO projection_projects (
                   project_id, title, workspace_root, default_model_selection_json,
                   scripts_json, created_at, updated_at, deleted_at
                 ) VALUES (?1, ?2, '/tmp/protected-project', NULL, '{}',
                           '2026-08-09T00:00:00Z', '2026-08-09T00:00:00Z', NULL)",
                ["protected-project", title],
            )
            .expect("fixture project");
        drop(connection);
        fixture
    }

    fn write_marker(&self, storage_instance_id: Uuid) {
        std::fs::write(
            &self.paths.environment_id,
            format!("{storage_instance_id}\n"),
        )
        .expect("marker fixture");
    }

    fn write_marker_bytes(&self, bytes: &[u8]) {
        std::fs::write(&self.paths.environment_id, bytes).expect("marker fixture");
    }

    async fn prepare(&self) -> Result<PreparedStore, StoreStartupError> {
        prepare_store(&self.config).await
    }
}

// Use only after backup work has settled and the fixture will never use this database again.
// Dropping the handle only releases its sender; acknowledge actual file closure before
// offline marker changes or snapshots. The detached worker retains only an inert memory DB.
async fn close_prepared_store_for_offline_fixture(prepared: PreparedStore) {
    prepared
        .database
        .call(|connection| {
            let replacement = Connection::open_in_memory()?;
            let file_connection = std::mem::replace(connection, replacement);
            file_connection.close().map_err(|(_, error)| error)?;
            Ok(())
        })
        .await
        .expect("close prepared fixture file connection");
    drop(prepared);
}

fn server_endpoint(handle: &coddy_studio_server::ServerHandle, path: &str) -> String {
    format!("http://{}{path}", handle.local_addr())
}

fn assert_descriptor_is_path_redacted(
    descriptor: &Value,
    requested_root: &Path,
    effective_root: &Path,
) {
    const FORBIDDEN_KEYS: [&str; 5] = [
        "baseDir",
        "resolvedDataRoot",
        "requestedRoot",
        "effectiveRoot",
        "isFilesystemAlias",
    ];
    let requested = requested_root.to_string_lossy();
    let effective = effective_root.to_string_lossy();
    fn assert_value_has_no_path(value: &Value, requested: &str, effective: &str) {
        match value {
            Value::Array(values) => {
                for value in values {
                    assert_value_has_no_path(value, requested, effective);
                }
            }
            Value::Object(values) => {
                for (key, value) in values {
                    assert!(
                        !FORBIDDEN_KEYS.contains(&key.as_str()),
                        "descriptor leaked local root diagnostic field {key}"
                    );
                    assert_value_has_no_path(value, requested, effective);
                }
            }
            Value::String(value) => {
                assert!(
                    !value.contains(requested),
                    "descriptor leaked requested root"
                );
                assert!(
                    !value.contains(effective),
                    "descriptor leaked effective root"
                );
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }
    assert_value_has_no_path(descriptor, &requested, &effective);
}

async fn fetch_axum_descriptor(
    client: &Client,
    handle: &coddy_studio_server::ServerHandle,
) -> Value {
    let response = client
        .get(server_endpoint(
            handle,
            "/.well-known/coddy-studio/environment",
        ))
        .send()
        .await
        .expect("environment descriptor response");
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.expect("environment descriptor JSON")
}

async fn fetch_connect_descriptor(
    client: &Client,
    handle: &coddy_studio_server::ServerHandle,
) -> Value {
    let credential = handle
        .startup_access()
        .expect("web startup access")
        .credential
        .clone();
    let token_response = client
        .post(server_endpoint(handle, "/oauth/token"))
        .form(&[
            (
                "grant_type",
                "urn:ietf:params:oauth:grant-type:token-exchange",
            ),
            ("subject_token", credential.as_str()),
            (
                "subject_token_type",
                "urn:coddy-studio:params:oauth:token-type:environment-bootstrap",
            ),
            (
                "requested_token_type",
                "urn:ietf:params:oauth:token-type:access_token",
            ),
        ])
        .send()
        .await
        .expect("startup credential exchange");
    assert_eq!(token_response.status(), StatusCode::OK);
    let access_token = token_response
        .json::<Value>()
        .await
        .expect("access token JSON")["access_token"]
        .as_str()
        .expect("access token")
        .to_owned();
    let cloud_keys = TempDir::new().expect("temporary Connect cloud key directory");
    let cloud_codec = PersistentJwtCodec::open(cloud_keys.path().join("cloud-keypair.json"))
        .await
        .expect("Connect cloud JWT codec");
    let (_, cloud_public_key) = cloud_codec
        .key_pair()
        .await
        .expect("Connect cloud key pair");
    let config_response = client
        .post(server_endpoint(handle, "/api/connect/relay-config"))
        .bearer_auth(&access_token)
        .json(&json!({
            "relayUrl": "https://relay.example",
            "relayIssuer": "https://relay.example",
            "cloudUserId": "project-data-safety-user",
            "environmentCredential": "project-data-safety-credential",
            "cloudMintPublicKey": cloud_public_key,
            "endpointRuntime": null
        }))
        .send()
        .await
        .expect("Connect relay configuration response");
    let config_status = config_response.status();
    let config_body = config_response
        .text()
        .await
        .expect("Connect relay configuration response body");
    assert_eq!(
        config_status,
        StatusCode::OK,
        "Connect relay configuration error: {config_body}"
    );
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let proof = cloud_codec
        .sign(
            "coddy-studio-cloud-health+jwt",
            json!({
                "iss": "https://relay.example",
                "aud": "coddy-studio-env:local",
                "sub": "project-data-safety-user",
                "jti": Uuid::new_v4().to_string(),
                "iat": now,
                "exp": now + 300,
                "environmentId": "local",
                "nonce": Uuid::new_v4().to_string(),
                "scope": ["environment:status"]
            }),
        )
        .await
        .expect("signed Connect health proof");
    let response = client
        .post(server_endpoint(handle, "/api/coddy-connect/health"))
        .bearer_auth(access_token)
        .json(&json!({ "proof": proof }))
        .send()
        .await
        .expect("Connect health response");
    let status = response.status();
    let body = response.text().await.expect("Connect health response body");
    assert_eq!(status, StatusCode::OK, "Connect health error: {body}");
    serde_json::from_str::<Value>(&body).expect("Connect health JSON")["descriptor"].clone()
}

#[tokio::test]
async fn descriptor_surfaces_publish_one_stable_uuid_without_local_path_leakage() {
    let root = TempDir::new().expect("temporary absolute data root");
    hermetic_providers::write_hermetic_settings(
        &ServerConfig::new(root.path()).state_dir(),
        json!({}),
    );
    let client = Client::builder()
        .no_proxy()
        .build()
        .expect("proxy-free HTTP client");

    let first = ServerRuntime::start(ServerConfig::new(root.path()).with_bind("127.0.0.1", 0))
        .await
        .expect("first server start");
    let requested_root = first.data_root().requested.clone();
    let effective_root = first.data_root().effective.clone();
    let first_axum = fetch_axum_descriptor(&client, &first).await;
    let first_connect = fetch_connect_descriptor(&client, &first).await;
    let first_storage_id = first_axum["storageInstanceId"]
        .as_str()
        .expect("new local server storage UUID");
    Uuid::parse_str(first_storage_id).expect("valid server storage UUID");
    assert_eq!(first_connect["storageInstanceId"], first_storage_id);
    assert_descriptor_is_path_redacted(&first_axum, &requested_root, &effective_root);
    assert_descriptor_is_path_redacted(&first_connect, &requested_root, &effective_root);
    first.shutdown();
    first.join().await.expect("first server shutdown");

    let second = ServerRuntime::start(ServerConfig::new(root.path()).with_bind("127.0.0.1", 0))
        .await
        .expect("second server start");
    let second_axum = fetch_axum_descriptor(&client, &second).await;
    let second_connect = fetch_connect_descriptor(&client, &second).await;
    assert_eq!(second_axum["storageInstanceId"], first_storage_id);
    assert_eq!(second_connect["storageInstanceId"], first_storage_id);
    assert_descriptor_is_path_redacted(&second_axum, &requested_root, &effective_root);
    assert_descriptor_is_path_redacted(&second_connect, &requested_root, &effective_root);
    second.shutdown();
    second.join().await.expect("second server shutdown");
}

#[tokio::test]
async fn marker_without_database_never_creates_replacement_sqlite() {
    let fixture = StoreFixture::new();
    fixture.write_marker(Uuid::new_v4());
    let marker_before = std::fs::read(&fixture.paths.environment_id).expect("marker fixture bytes");

    let error = match fixture.prepare().await {
        Ok(_) => panic!("missing database must block"),
        Err(error) => error,
    };

    assert!(matches!(error, StoreStartupError::DatabaseMissing { .. }));
    assert!(!fixture.paths.database.exists());
    assert_eq!(
        std::fs::read(&fixture.paths.environment_id).expect("marker remains"),
        marker_before
    );
}

#[tokio::test]
async fn existing_unmarked_database_is_adopted_without_catalog_changes() {
    let fixture = StoreFixture::with_project("Protected project").await;
    assert!(!fixture.paths.environment_id.exists());

    let prepared = fixture.prepare().await.expect("adopt existing database");

    assert_eq!(
        prepared.classification,
        StoreClassification::ExistingUnmarked
    );
    let titles = prepared
        .database
        .call(|connection| {
            let mut statement =
                connection.prepare("SELECT title FROM projection_projects ORDER BY title")?;
            Ok(statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .expect("project titles");
    assert_eq!(titles, ["Protected project"]);
    assert!(fixture.paths.environment_id.is_file());
}

#[tokio::test]
async fn first_run_creates_the_database_and_one_valid_marker() {
    let fixture = StoreFixture::new();

    let prepared = fixture.prepare().await.expect("prepare first run");

    assert_eq!(prepared.classification, StoreClassification::FirstRun);
    assert!(fixture.paths.database.is_file());
    let marker = std::fs::read_to_string(&fixture.paths.environment_id).expect("marker");
    Uuid::parse_str(marker.trim()).expect("marker UUID");
}

#[tokio::test]
async fn existing_database_with_valid_marker_reuses_the_store() {
    let fixture = StoreFixture::with_project("Existing project").await;
    let marker = Uuid::new_v4();
    fixture.write_marker(marker);
    let original_marker = std::fs::read(&fixture.paths.environment_id).expect("marker bytes");

    let prepared = fixture.prepare().await.expect("prepare existing store");

    assert_eq!(prepared.classification, StoreClassification::Existing);
    assert_eq!(
        std::fs::read(&fixture.paths.environment_id).expect("marker bytes"),
        original_marker
    );
}

#[tokio::test]
async fn invalid_sqlite_is_preserved_without_publishing_an_identity() {
    let fixture = StoreFixture::new();
    let original_database_bytes = b"not a sqlite database";
    std::fs::write(&fixture.paths.database, original_database_bytes).expect("invalid database");

    let error = match fixture.prepare().await {
        Ok(_) => panic!("invalid SQLite must block"),
        Err(error) => error,
    };

    assert!(matches!(error, StoreStartupError::CorruptDatabase { .. }));
    assert_eq!(
        std::fs::read(&fixture.paths.database).expect("invalid database remains"),
        original_database_bytes
    );
    assert!(!fixture.paths.environment_id.exists());
}

#[tokio::test]
async fn unrelated_valid_sqlite_is_preserved_without_publishing_an_identity() {
    let fixture = StoreFixture::new();
    let connection = Connection::open(&fixture.paths.database).expect("unrelated database");
    connection
        .execute_batch(
            "CREATE TABLE unrelated_data (value TEXT NOT NULL);\
             INSERT INTO unrelated_data (value) VALUES ('preserve me');",
        )
        .expect("unrelated schema");
    drop(connection);
    let original_database_bytes =
        std::fs::read(&fixture.paths.database).expect("unrelated database bytes");

    let error = match fixture.prepare().await {
        Ok(_) => panic!("unrelated SQLite must block"),
        Err(error) => error,
    };

    assert!(matches!(error, StoreStartupError::UnrecognizedStore { .. }));
    assert_eq!(
        std::fs::read(&fixture.paths.database).expect("unrelated database remains"),
        original_database_bytes
    );
    assert!(!fixture.paths.environment_id.exists());
}

#[tokio::test]
async fn empty_unknown_and_mismatched_ledgers_are_preserved_without_marker_publication() {
    for mutation in ["empty-ledger", "unknown-id", "renamed-row"] {
        let fixture = StoreFixture::with_project("Ledger project").await;
        let connection = Connection::open(&fixture.paths.database).expect("ledger database");
        match mutation {
            "empty-ledger" => {
                connection
                    .execute("DELETE FROM effect_sql_migrations", [])
                    .expect("empty ledger");
            }
            "unknown-id" => {
                connection
                    .execute(
                        "INSERT INTO effect_sql_migrations (migration_id, name) VALUES (99, 'Unknown')",
                        [],
                    )
                    .expect("unknown ledger row");
            }
            "renamed-row" => {
                connection
                    .execute(
                        "UPDATE effect_sql_migrations SET name = 'Renamed' WHERE migration_id = 1",
                        [],
                    )
                    .expect("renamed ledger row");
            }
            _ => unreachable!(),
        }
        drop(connection);
        let original_database_bytes =
            std::fs::read(&fixture.paths.database).expect("ledger database bytes");

        let error = match fixture.prepare().await {
            Ok(_) => panic!("{mutation} must block"),
            Err(error) => error,
        };

        assert!(
            matches!(error, StoreStartupError::UnrecognizedStore { .. }),
            "unexpected error for {mutation}: {error}"
        );
        assert_eq!(
            std::fs::read(&fixture.paths.database).expect("ledger database remains"),
            original_database_bytes,
            "database changed for {mutation}"
        );
        assert!(!fixture.paths.environment_id.exists());
    }
}

#[tokio::test]
async fn missing_core_table_is_not_accepted_as_a_coddy_studio_store() {
    let fixture = StoreFixture::with_project("Incomplete project").await;
    let connection = Connection::open(&fixture.paths.database).expect("recognized database");
    connection
        .execute_batch("DROP TABLE projection_projects")
        .expect("remove required table");
    drop(connection);
    let original_database_bytes =
        std::fs::read(&fixture.paths.database).expect("incomplete database bytes");

    let error = match fixture.prepare().await {
        Ok(_) => panic!("missing core table must block"),
        Err(error) => error,
    };

    assert!(matches!(error, StoreStartupError::UnrecognizedStore { .. }));
    assert_eq!(
        std::fs::read(&fixture.paths.database).expect("incomplete database remains"),
        original_database_bytes
    );
    assert!(!fixture.paths.environment_id.exists());
}

#[tokio::test]
async fn malformed_marker_and_database_are_preserved_byte_for_byte() {
    let fixture = StoreFixture::with_project("Marker project").await;
    let malformed_marker_bytes = b"definitely-not-one-uuid\n";
    fixture.write_marker_bytes(malformed_marker_bytes);
    let original_database_bytes = std::fs::read(&fixture.paths.database).expect("database bytes");

    let error = match fixture.prepare().await {
        Ok(_) => panic!("malformed marker must block"),
        Err(error) => error,
    };

    assert!(matches!(error, StoreStartupError::MarkerMalformed { .. }));
    assert_eq!(
        std::fs::read(&fixture.paths.database).expect("database remains"),
        original_database_bytes
    );
    assert_eq!(
        std::fs::read(&fixture.paths.environment_id).expect("marker remains"),
        malformed_marker_bytes
    );
}

#[tokio::test]
async fn malformed_marker_without_database_never_creates_sqlite() {
    let fixture = StoreFixture::new();
    let malformed_marker_bytes = b"broken identity";
    fixture.write_marker_bytes(malformed_marker_bytes);

    let error = match fixture.prepare().await {
        Ok(_) => panic!("malformed marker must block"),
        Err(error) => error,
    };

    assert!(matches!(error, StoreStartupError::MarkerMalformed { .. }));
    assert!(!fixture.paths.database.exists());
    assert_eq!(
        std::fs::read(&fixture.paths.environment_id).expect("marker remains"),
        malformed_marker_bytes
    );
}

#[tokio::test]
async fn valid_marker_never_allows_corrupt_or_unrelated_database_adoption() {
    for database_kind in ["corrupt", "unrelated"] {
        let fixture = StoreFixture::new();
        match database_kind {
            "corrupt" => {
                std::fs::write(&fixture.paths.database, b"not a sqlite database")
                    .expect("corrupt database");
            }
            "unrelated" => {
                let connection =
                    Connection::open(&fixture.paths.database).expect("unrelated database");
                connection
                    .execute("CREATE TABLE foreign_catalog (value TEXT)", [])
                    .expect("unrelated schema");
            }
            _ => unreachable!(),
        }
        fixture.write_marker(Uuid::new_v4());
        let database_before =
            std::fs::read(&fixture.paths.database).expect("database fixture bytes");
        let marker_before =
            std::fs::read(&fixture.paths.environment_id).expect("marker fixture bytes");

        let error = fixture
            .prepare()
            .await
            .expect_err("unrecognized database must block despite a valid marker");

        assert!(
            matches!(
                error,
                StoreStartupError::CorruptDatabase { .. }
                    | StoreStartupError::UnrecognizedStore { .. }
            ),
            "unexpected error for {database_kind}: {error}"
        );
        assert_eq!(
            std::fs::read(&fixture.paths.database).expect("database remains"),
            database_before
        );
        assert_eq!(
            std::fs::read(&fixture.paths.environment_id).expect("marker remains"),
            marker_before
        );
    }
}

#[tokio::test]
async fn concurrent_adoption_of_a_recognized_store_converges_on_one_identity() {
    let fixture = StoreFixture::with_project("Concurrent project").await;

    let (first, second) = tokio::join!(fixture.prepare(), fixture.prepare());
    let first = first.expect("first adoption");
    let second = second.expect("second adoption");

    assert_eq!(first.storage_instance_id, second.storage_instance_id);
    assert!(fixture.paths.environment_id.is_file());
}

#[tokio::test]
async fn crash_left_sidecars_restart_with_the_same_store_identity_and_project() {
    let (_root, config, paths) = crashed_store_fixture();
    let marker_before = std::fs::read(&paths.environment_id).expect("crash-store marker");
    assert!(sqlite_sidecar(&paths.database, "-wal").is_file());
    assert!(sqlite_sidecar(&paths.database, "-shm").is_file());

    let prepared = prepare_store(&config)
        .await
        .expect("valid marked crash-store restarts");
    let title = prepared
        .database
        .call(|connection| {
            Ok(connection.query_row(
                "SELECT title FROM projection_projects WHERE project_id = 'crash-project'",
                [],
                |row| row.get::<_, String>(0),
            )?)
        })
        .await
        .expect("crash-store project");

    assert_eq!(prepared.classification, StoreClassification::Existing);
    assert_eq!(
        prepared.storage_instance_id.to_string(),
        marker_text(&marker_before)
    );
    assert_eq!(title, "Crash project");
    assert_eq!(
        std::fs::read(&paths.environment_id).expect("marker remains"),
        marker_before
    );
}

#[tokio::test]
async fn crash_left_wal_without_shm_restarts_with_only_sqlite_coordination_recreated() {
    let (_root, config, paths) = crashed_store_fixture();
    let marker_before = std::fs::read(&paths.environment_id).expect("crash-store marker");
    let shared_memory = sqlite_sidecar(&paths.database, "-shm");
    std::fs::remove_file(&shared_memory).expect("remove crash-left SHM fixture");
    let mut expected_entries = directory_entry_names(&paths.state_dir);
    expected_entries.push(std::ffi::OsString::from("state.sqlite-shm"));
    expected_entries.sort();

    let prepared = prepare_store(&config)
        .await
        .expect("valid marked crash-store without SHM restarts");
    let (title, integrity) = prepared
        .database
        .call(|connection| {
            Ok((
                connection.query_row(
                    "SELECT title FROM projection_projects WHERE project_id = 'crash-project'",
                    [],
                    |row| row.get::<_, String>(0),
                )?,
                connection.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))?,
            ))
        })
        .await
        .expect("crash-store project and integrity");

    assert_eq!(prepared.classification, StoreClassification::Existing);
    assert_eq!(
        prepared.storage_instance_id.to_string(),
        marker_text(&marker_before)
    );
    assert_eq!(title, "Crash project");
    assert_eq!(integrity, "ok");
    assert_eq!(directory_entry_names(&paths.state_dir), expected_entries);
    assert!(
        shared_memory
            .metadata()
            .expect("recreated SQLite SHM")
            .len()
            >= 32 * 1024,
        "SQLite recreates its valid volatile WAL-index coordination file"
    );
    assert_eq!(
        std::fs::read(&paths.environment_id).expect("marker remains"),
        marker_before
    );
}

#[tokio::test]
async fn crash_store_fixture_child() {
    let Some(root) = std::env::var_os(CRASH_STORE_CHILD_ROOT).map(PathBuf::from) else {
        return;
    };
    let ready = PathBuf::from(
        std::env::var_os(CRASH_STORE_CHILD_READY).expect("crash-store child ready path"),
    );
    let mut config = ServerConfig::new(&root);
    let resolved = resolve_data_root(config.data_root_request.clone()).expect("resolve child root");
    config.base_dir = resolved.effective.clone();
    config.resolved_data_root = Some(resolved);
    let paths = StatePaths::from_config(&config);
    std::fs::create_dir_all(&paths.state_dir).expect("child state directory");
    let prepared = prepare_store(&config).await.expect("child store starts");
    prepared
        .database
        .call(|connection| {
            connection.pragma_update(None, "wal_autocheckpoint", 0)?;
            connection.execute(
                "INSERT INTO projection_projects (
                   project_id, title, workspace_root, default_model_selection_json,
                   scripts_json, created_at, updated_at, deleted_at
                 ) VALUES ('crash-project', 'Crash project', '/tmp/crash-project', NULL, '{}',
                           '2026-08-10T00:00:00Z', '2026-08-10T00:00:00Z', NULL)",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("child project write");
    assert!(sqlite_sidecar(&paths.database, "-wal").is_file());
    assert!(sqlite_sidecar(&paths.database, "-shm").is_file());
    std::fs::write(&ready, b"ready").expect("publish child readiness");
    std::future::pending::<()>().await;
}

#[cfg(unix)]
#[tokio::test]
async fn dangling_marker_entry_with_missing_database_is_not_first_run() {
    let fixture = StoreFixture::new();
    symlink(
        fixture.paths.state_dir.join("missing-marker-target"),
        &fixture.paths.environment_id,
    )
    .expect("dangling marker fixture");
    let marker_before = std::fs::symlink_metadata(&fixture.paths.environment_id)
        .expect("marker entry")
        .file_type();

    fixture
        .prepare()
        .await
        .expect_err("dangling marker must block first run");

    assert!(!fixture.paths.database.exists());
    assert!(marker_before.is_symlink());
    assert!(
        std::fs::symlink_metadata(&fixture.paths.environment_id)
            .expect("marker entry remains")
            .file_type()
            .is_symlink()
    );
}

#[tokio::test]
async fn offline_fixture_teardown_waits_for_owned_work_and_closes_the_file_connection() {
    let fixture = StoreFixture::with_project("Before queued work").await;
    fixture.write_marker(Uuid::new_v4());
    let prepared = fixture.prepare().await.expect("prepare teardown fixture");
    let database = prepared.database.clone();
    let (entered_sender, entered_receiver) = tokio::sync::oneshot::channel();
    let (release_sender, release_receiver) = std::sync::mpsc::channel();
    let held_work = tokio::spawn(async move {
        database
            .call(move |connection| {
                connection.execute(
                    "UPDATE projection_projects SET title = 'After queued work' WHERE project_id = 'protected-project'",
                    [],
                )?;
                entered_sender.send(()).expect("worker entered held work");
                release_receiver
                    .recv()
                    .map_err(|_| coddy_studio_server::persistence::PersistenceError::WorkerUnavailable)?;
                Ok(())
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), entered_receiver)
        .await
        .expect("worker admission deadline")
        .expect("worker entered held work");
    assert!(sqlite_sidecar(&fixture.paths.database, "-wal").is_file());

    tokio::time::pause();
    let teardown = close_prepared_store_for_offline_fixture(prepared);
    tokio::pin!(teardown);
    assert!(futures_util::poll!(teardown.as_mut()).is_pending());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(
        futures_util::poll!(teardown.as_mut()).is_pending(),
        "offline teardown must await the held file connection instead of a delay"
    );
    tokio::time::resume();

    release_sender
        .send(())
        .expect("release owned database work");
    tokio::time::timeout(Duration::from_secs(5), held_work)
        .await
        .expect("owned work completion deadline")
        .expect("owned database task joins")
        .expect("owned database work completes");
    tokio::time::timeout(Duration::from_secs(5), teardown)
        .await
        .expect("file connection close deadline");
    assert!(!sqlite_sidecar(&fixture.paths.database, "-wal").exists());
    assert!(!sqlite_sidecar(&fixture.paths.database, "-shm").exists());
    let connection = Connection::open(&fixture.paths.database).expect("closed fixture database");
    let title: String = connection
        .query_row(
            "SELECT title FROM projection_projects WHERE project_id = 'protected-project'",
            [],
            |row| row.get(0),
        )
        .expect("owned work persisted before offline teardown");
    assert_eq!(title, "After queued work");
}

#[tokio::test]
async fn recovery_restore_preserves_the_live_store_before_installing_a_verified_backup() {
    let fixture = StoreFixture::with_project("Before restore").await;
    let storage_instance_id = Uuid::new_v4();
    fixture.write_marker(storage_instance_id);
    let prepared = fixture.prepare().await.expect("prepare recovery fixture");
    let backup = create_verified_backup(
        &prepared.database,
        &prepared,
        BackupTrigger::PreUpdate,
        env!("CARGO_PKG_VERSION"),
    )
    .await
    .expect("create verified recovery generation");
    prepared
        .database
        .call(|connection| {
            connection.execute(
                "UPDATE projection_projects SET title = 'After backup' WHERE project_id = 'protected-project'",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("mutate live project after backup");
    close_prepared_store_for_offline_fixture(prepared).await;

    let result = restore_backup(
        fixture
            .config
            .resolved_data_root
            .as_ref()
            .expect("resolved recovery root"),
        backup.manifest.backup_id,
    )
    .await
    .expect("restore verified generation");

    let restored = Connection::open(&fixture.paths.database).expect("restored database");
    let restored_title: String = restored
        .query_row(
            "SELECT title FROM projection_projects WHERE project_id = 'protected-project'",
            [],
            |row| row.get(0),
        )
        .expect("restored project title");
    assert_eq!(restored_title, "Before restore");
    assert_eq!(
        std::fs::read_to_string(&fixture.paths.environment_id)
            .expect("restored marker")
            .trim(),
        storage_instance_id.to_string()
    );

    let preserved_database = result.preserved_directory.join("state.sqlite");
    let preserved = Connection::open(preserved_database).expect("preserved live database");
    let preserved_title: String = preserved
        .query_row(
            "SELECT title FROM projection_projects WHERE project_id = 'protected-project'",
            [],
            |row| row.get(0),
        )
        .expect("preserved live project title");
    assert_eq!(preserved_title, "After backup");
}

#[tokio::test]
async fn recovery_restore_rejects_a_backup_from_a_different_known_storage_identity() {
    let fixture = StoreFixture::with_project("Original project").await;
    fixture.write_marker(Uuid::new_v4());
    let prepared = fixture.prepare().await.expect("prepare recovery fixture");
    let backup = create_verified_backup(
        &prepared.database,
        &prepared,
        BackupTrigger::PreUpdate,
        env!("CARGO_PKG_VERSION"),
    )
    .await
    .expect("create verified recovery generation");
    close_prepared_store_for_offline_fixture(prepared).await;
    fixture.write_marker(Uuid::new_v4());
    let database_before = std::fs::read(&fixture.paths.database).expect("live database bytes");
    let marker_before = std::fs::read(&fixture.paths.environment_id).expect("live marker bytes");
    let entries_before = directory_entry_names(&fixture.paths.state_dir);

    let error = restore_backup(
        fixture
            .config
            .resolved_data_root
            .as_ref()
            .expect("resolved recovery root"),
        backup.manifest.backup_id,
    )
    .await
    .expect_err("different known storage identity must block restore");

    assert!(matches!(error, RecoveryError::StorageIdentityMismatch));
    assert_eq!(
        std::fs::read(&fixture.paths.database).expect("live database remains"),
        database_before
    );
    assert_eq!(
        std::fs::read(&fixture.paths.environment_id).expect("live marker remains"),
        marker_before
    );
    assert_eq!(
        directory_entry_names(&fixture.paths.state_dir),
        entries_before
    );
    assert!(!fixture.paths.recovery_journal().exists());
}

#[tokio::test]
async fn recovery_restore_rejects_a_tampered_manifest_without_mutating_the_live_store() {
    let fixture = StoreFixture::with_project("Original project").await;
    fixture.write_marker(Uuid::new_v4());
    let prepared = fixture.prepare().await.expect("prepare recovery fixture");
    let backup = create_verified_backup(
        &prepared.database,
        &prepared,
        BackupTrigger::PreUpdate,
        env!("CARGO_PKG_VERSION"),
    )
    .await
    .expect("create verified recovery generation");
    close_prepared_store_for_offline_fixture(prepared).await;
    let mut manifest: Value = serde_json::from_slice(
        &std::fs::read(&backup.manifest_path).expect("backup manifest bytes"),
    )
    .expect("backup manifest JSON");
    manifest["sha256"] = Value::String("0".repeat(64));
    std::fs::write(
        &backup.manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("tampered manifest JSON"),
    )
    .expect("tamper backup manifest");
    let database_before = std::fs::read(&fixture.paths.database).expect("live database bytes");
    let marker_before = std::fs::read(&fixture.paths.environment_id).expect("live marker bytes");
    let entries_before = directory_entry_names(&fixture.paths.state_dir);

    let error = restore_backup(
        fixture
            .config
            .resolved_data_root
            .as_ref()
            .expect("resolved recovery root"),
        backup.manifest.backup_id,
    )
    .await
    .expect_err("tampered backup must block restore");

    assert!(matches!(
        error,
        RecoveryError::Backup(coddy_studio_server::persistence::BackupError::Verification(
            _
        ))
    ));
    assert_eq!(
        std::fs::read(&fixture.paths.database).expect("live database remains"),
        database_before
    );
    assert_eq!(
        std::fs::read(&fixture.paths.environment_id).expect("live marker remains"),
        marker_before
    );
    assert_eq!(
        directory_entry_names(&fixture.paths.state_dir),
        entries_before
    );
    assert!(!fixture.paths.recovery_journal().exists());
}

#[tokio::test]
async fn recovery_restore_rejects_a_backup_from_the_other_state_kind_without_mutating_live_data() {
    let fixture = StoreFixture::with_project("Userdata project").await;
    let production_id = Uuid::new_v4();
    fixture.write_marker(production_id);
    let production = fixture.prepare().await.expect("prepare userdata store");
    drop(production);

    let dev_config = fixture
        .config
        .clone()
        .with_dev_url("http://127.0.0.1:5173".parse().expect("development URL"));
    let dev_paths = StatePaths::from_config(&dev_config);
    std::fs::create_dir_all(&dev_paths.state_dir).expect("development state directory");
    let mut dev_connection = Connection::open(&dev_paths.database).expect("development database");
    run_migrations(&mut dev_connection, None).expect("development migrations");
    drop(dev_connection);
    std::fs::write(&dev_paths.environment_id, format!("{}\n", Uuid::new_v4()))
        .expect("development marker");
    let dev = prepare_store(&dev_config)
        .await
        .expect("prepare development store");
    let dev_backup = create_verified_backup(
        &dev.database,
        &dev,
        BackupTrigger::PreUpdate,
        env!("CARGO_PKG_VERSION"),
    )
    .await
    .expect("create development backup");
    drop(dev);

    let database_before = std::fs::read(&fixture.paths.database).expect("userdata database bytes");
    let marker_before =
        std::fs::read(&fixture.paths.environment_id).expect("userdata marker bytes");

    let error = restore_backup(
        fixture
            .config
            .resolved_data_root
            .as_ref()
            .expect("resolved userdata root"),
        dev_backup.manifest.backup_id,
    )
    .await
    .expect_err("development backup must not restore into userdata");

    assert!(matches!(error, RecoveryError::BackupNotFound));
    assert_eq!(
        std::fs::read(&fixture.paths.database).expect("userdata database remains"),
        database_before
    );
    assert_eq!(
        std::fs::read(&fixture.paths.environment_id).expect("userdata marker remains"),
        marker_before
    );
    assert!(!fixture.paths.recovery_journal().exists());
}

#[tokio::test]
async fn recovery_restore_rejects_a_checksum_valid_non_sqlite_backup_without_mutating_live_data() {
    let fixture = StoreFixture::with_project("Original project").await;
    fixture.write_marker(Uuid::new_v4());
    let prepared = fixture.prepare().await.expect("prepare recovery fixture");
    let backup = create_verified_backup(
        &prepared.database,
        &prepared,
        BackupTrigger::PreUpdate,
        env!("CARGO_PKG_VERSION"),
    )
    .await
    .expect("create verified recovery generation");
    drop(prepared);

    let invalid_database = b"not a SQLite database";
    std::fs::write(&backup.database, invalid_database).expect("replace backup database");
    let mut manifest: Value = serde_json::from_slice(
        &std::fs::read(&backup.manifest_path).expect("backup manifest bytes"),
    )
    .expect("backup manifest JSON");
    manifest["databaseSizeBytes"] = Value::from(invalid_database.len() as u64);
    manifest["sha256"] = Value::String(
        Sha256::digest(invalid_database)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    );
    std::fs::write(
        &backup.manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("refingerprinted manifest JSON"),
    )
    .expect("rewrite backup manifest");
    let database_before = std::fs::read(&fixture.paths.database).expect("live database bytes");
    let marker_before = std::fs::read(&fixture.paths.environment_id).expect("live marker bytes");

    let error = restore_backup(
        fixture
            .config
            .resolved_data_root
            .as_ref()
            .expect("resolved recovery root"),
        backup.manifest.backup_id,
    )
    .await
    .expect_err("checksum-valid non-SQLite backup must block restore");

    assert!(matches!(
        error,
        RecoveryError::Backup(coddy_studio_server::persistence::BackupError::Verification(
            _
        ))
    ));
    assert_eq!(
        std::fs::read(&fixture.paths.database).expect("live database remains"),
        database_before
    );
    assert_eq!(
        std::fs::read(&fixture.paths.environment_id).expect("live marker remains"),
        marker_before
    );
    assert!(!fixture.paths.recovery_journal().exists());
}

#[tokio::test]
async fn recovery_restore_preserves_a_malformed_marker_and_installs_the_verified_identity() {
    let fixture = StoreFixture::with_project("Before restore").await;
    let storage_instance_id = Uuid::new_v4();
    fixture.write_marker(storage_instance_id);
    let prepared = fixture.prepare().await.expect("prepare recovery fixture");
    let backup = create_verified_backup(
        &prepared.database,
        &prepared,
        BackupTrigger::PreUpdate,
        env!("CARGO_PKG_VERSION"),
    )
    .await
    .expect("create verified recovery generation");
    close_prepared_store_for_offline_fixture(prepared).await;
    let malformed = b"not-a-storage-uuid\n";
    fixture.write_marker_bytes(malformed);

    let result = restore_backup(
        fixture
            .config
            .resolved_data_root
            .as_ref()
            .expect("resolved recovery root"),
        backup.manifest.backup_id,
    )
    .await
    .expect("explicit verified restore repairs malformed marker");

    assert_eq!(
        std::fs::read(result.preserved_directory.join("environment-id"))
            .expect("malformed marker was preserved"),
        malformed
    );
    assert_eq!(
        std::fs::read_to_string(&fixture.paths.environment_id)
            .expect("verified marker installed")
            .trim(),
        storage_instance_id.to_string()
    );
}

#[tokio::test]
async fn recovery_start_empty_preserves_crash_left_sqlite_files_before_new_identity_creation() {
    let (_root, config, paths) = crashed_store_fixture();
    let old_marker = std::fs::read_to_string(&paths.environment_id)
        .expect("crash-left marker")
        .trim()
        .to_owned();
    assert!(sqlite_sidecar(&paths.database, "-wal").is_file());
    assert!(sqlite_sidecar(&paths.database, "-shm").is_file());

    let result = preserve_and_start_empty(
        config
            .resolved_data_root
            .as_ref()
            .expect("resolved recovery root"),
    )
    .await
    .expect("preserve crash-left store before start-empty");

    assert!(!paths.database.exists());
    assert!(!sqlite_sidecar(&paths.database, "-wal").exists());
    assert!(!sqlite_sidecar(&paths.database, "-shm").exists());
    assert!(!paths.environment_id.exists());
    for name in [
        "state.sqlite",
        "state.sqlite-wal",
        "state.sqlite-shm",
        "environment-id",
    ] {
        assert!(result.preserved_directory.join(name).is_file(), "{name}");
    }
    let preserved = Connection::open(result.preserved_directory.join("state.sqlite"))
        .expect("open preserved crash-left store");
    let preserved_title: String = preserved
        .query_row(
            "SELECT title FROM projection_projects WHERE project_id = 'crash-project'",
            [],
            |row| row.get(0),
        )
        .expect("preserved WAL project");
    assert_eq!(preserved_title, "Crash project");
    drop(preserved);

    let prepared = prepare_store(&config)
        .await
        .expect("normal startup creates explicit empty store");
    assert_eq!(prepared.classification, StoreClassification::FirstRun);
    assert_ne!(prepared.storage_instance_id.to_string(), old_marker);
}

#[tokio::test]
async fn recovery_incomplete_journal_never_becomes_a_first_run_store() {
    let fixture = StoreFixture::new();
    let journal_bytes = br#"{"operationId":"interrupted"}"#;
    std::fs::write(fixture.paths.recovery_journal(), journal_bytes)
        .expect("interrupted recovery journal");

    let error = fixture
        .prepare()
        .await
        .expect_err("incomplete recovery must block first-run creation");

    assert!(matches!(
        error,
        StoreStartupError::RecoveryIncomplete { .. }
    ));
    assert!(!fixture.paths.database.exists());
    assert!(!fixture.paths.environment_id.exists());
    assert_eq!(
        std::fs::read(fixture.paths.recovery_journal()).expect("journal remains"),
        journal_bytes
    );
}

#[tokio::test]
async fn recovery_restore_with_an_existing_journal_performs_no_additional_filesystem_writes() {
    let fixture = StoreFixture::with_project("Journal-protected project").await;
    fixture.write_marker(Uuid::new_v4());
    let prepared = fixture.prepare().await.expect("prepare recovery fixture");
    let backup = create_verified_backup(
        &prepared.database,
        &prepared,
        BackupTrigger::PreUpdate,
        env!("CARGO_PKG_VERSION"),
    )
    .await
    .expect("create verified recovery generation");
    drop(prepared);
    std::fs::write(fixture.paths.runtime_lock(), b"").expect("existing runtime lock");
    std::fs::write(fixture.paths.recovery_journal(), b"existing recovery\n")
        .expect("existing recovery journal");
    let entries_before = directory_entry_names(&fixture.paths.base_dir);
    let database_before = std::fs::read(&fixture.paths.database).expect("live database bytes");
    let marker_before = std::fs::read(&fixture.paths.environment_id).expect("live marker bytes");

    let error = restore_backup(
        fixture
            .config
            .resolved_data_root
            .as_ref()
            .expect("resolved recovery root"),
        backup.manifest.backup_id,
    )
    .await
    .expect_err("existing journal must block a second recovery");

    assert!(matches!(error, RecoveryError::RecoveryInProgress { .. }));
    assert_eq!(
        directory_entry_names(&fixture.paths.base_dir),
        entries_before
    );
    assert_eq!(
        std::fs::read(&fixture.paths.database).expect("live database remains"),
        database_before
    );
    assert_eq!(
        std::fs::read(&fixture.paths.environment_id).expect("live marker remains"),
        marker_before
    );
}

#[tokio::test]
async fn recovery_incomplete_staging_never_becomes_a_first_run_store() {
    let fixture = StoreFixture::new();
    let staging = fixture.paths.recovery_staging_dir(Uuid::new_v4());
    std::fs::create_dir(&staging).expect("interrupted recovery staging directory");
    std::fs::write(staging.join("state.sqlite"), b"partial")
        .expect("interrupted recovery staging file");

    let error = fixture
        .prepare()
        .await
        .expect_err("incomplete recovery staging must block first-run creation");

    assert!(matches!(
        error,
        StoreStartupError::RecoveryIncomplete { path } if path == staging
    ));
    assert!(!fixture.paths.database.exists());
    assert!(!fixture.paths.environment_id.exists());
    assert_eq!(
        std::fs::read(staging.join("state.sqlite")).expect("staging remains"),
        b"partial"
    );
}

#[tokio::test]
async fn recovery_refuses_to_mutate_a_store_owned_by_a_running_server() {
    let root = TempDir::new().expect("temporary active-store root");
    hermetic_providers::write_hermetic_settings(
        &ServerConfig::new(root.path()).state_dir(),
        json!({}),
    );
    let handle = ServerRuntime::start(ServerConfig::new(root.path()).with_bind("127.0.0.1", 0))
        .await
        .expect("start active store owner");
    let resolved = handle.data_root().clone();
    let paths = {
        let mut config = ServerConfig::new(&resolved.effective);
        config.base_dir.clone_from(&resolved.effective);
        config.resolved_data_root = Some(resolved.clone());
        StatePaths::from_config(&config)
    };
    let database_before = std::fs::read(&paths.database).expect("active database bytes");
    let marker_before = std::fs::read(&paths.environment_id).expect("active marker bytes");

    let error = preserve_and_start_empty(&resolved)
        .await
        .expect_err("running store must block offline recovery");

    assert!(matches!(error, RecoveryError::StoreRunning));
    assert_eq!(
        std::fs::read(&paths.database).expect("active database remains"),
        database_before
    );
    assert_eq!(
        std::fs::read(&paths.environment_id).expect("active marker remains"),
        marker_before
    );
    assert!(!paths.recovery_journal().exists());
    handle.shutdown();
    handle.join().await.expect("active store owner stops");
}

#[tokio::test]
async fn recovery_inspection_reports_verified_store_state_without_mutating_it() {
    let fixture = StoreFixture::with_project("Inspectable project").await;
    let storage_instance_id = Uuid::new_v4();
    fixture.write_marker(storage_instance_id);
    let prepared = fixture.prepare().await.expect("prepare inspection fixture");
    let backup = create_verified_backup(
        &prepared.database,
        &prepared,
        BackupTrigger::PreUpdate,
        env!("CARGO_PKG_VERSION"),
    )
    .await
    .expect("create verified inspection generation");
    close_prepared_store_for_offline_fixture(prepared).await;
    let database_before = std::fs::read(&fixture.paths.database).expect("database bytes");
    let marker_before = std::fs::read(&fixture.paths.environment_id).expect("marker bytes");
    let entries_before = directory_entry_names(&fixture.paths.state_dir);

    let inspection = inspect_store(
        fixture
            .config
            .resolved_data_root
            .as_ref()
            .expect("resolved inspection root"),
    )
    .await
    .expect("inspect store");

    assert_eq!(inspection.classification, StoreInspectionStatus::Existing);
    assert_eq!(
        inspection
            .storage_instance_id
            .map(|value| value.to_string()),
        Some(storage_instance_id.to_string())
    );
    assert_eq!(inspection.backups.len(), 1);
    assert_eq!(
        inspection.backups[0].manifest.backup_id,
        backup.manifest.backup_id
    );
    assert_eq!(
        inspection.requested_root,
        fixture
            .config
            .resolved_data_root
            .as_ref()
            .expect("resolved root")
            .requested
    );
    assert_eq!(
        std::fs::read(&fixture.paths.database).expect("database remains"),
        database_before
    );
    assert_eq!(
        std::fs::read(&fixture.paths.environment_id).expect("marker remains"),
        marker_before
    );
    assert_eq!(
        directory_entry_names(&fixture.paths.state_dir),
        entries_before
    );
}

#[tokio::test]
async fn recovery_inspection_reports_an_absent_root_as_first_run_without_creating_it() {
    let parent = TempDir::new().expect("temporary parent");
    let missing_root = parent.path().join("missing-coddy-studio-root");
    let config = ServerConfig::new(&missing_root);
    let root = resolve_data_root(config.data_root_request.clone()).expect("resolve missing root");

    let inspection = inspect_store(&root)
        .await
        .expect("inspect absent project-data root");

    assert_eq!(inspection.classification, StoreInspectionStatus::FirstRun);
    assert!(inspection.storage_instance_id.is_none());
    assert!(inspection.backups.is_empty());
    assert!(inspection.backup_issues.is_empty());
    assert!(
        !missing_root.exists(),
        "inspection must not create the root"
    );
}

fn sqlite_sidecar(database: &Path, suffix: &str) -> PathBuf {
    let mut path = database.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

fn marker_text(bytes: &[u8]) -> String {
    std::str::from_utf8(bytes)
        .expect("UTF-8 marker")
        .trim()
        .to_owned()
}

fn crashed_store_fixture() -> (TempDir, ServerConfig, StatePaths) {
    let root = TempDir::new().expect("temporary crash-store root");
    let ready = root.path().join("crash-store-ready");
    let mut child = Command::new(std::env::current_exe().expect("current test executable"))
        .arg("--exact")
        .arg("crash_store_fixture_child")
        .arg("--nocapture")
        .env(CRASH_STORE_CHILD_ROOT, root.path())
        .env(CRASH_STORE_CHILD_READY, &ready)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn crash-store fixture child");
    wait_for_path(&ready, &mut child);
    child.kill().expect("kill crash-store fixture child");
    child.wait().expect("reap crash-store fixture child");

    let mut config = ServerConfig::new(root.path());
    let resolved = resolve_data_root(config.data_root_request.clone()).expect("resolve root");
    config.base_dir = resolved.effective.clone();
    config.resolved_data_root = Some(resolved);
    let paths = StatePaths::from_config(&config);
    (root, config, paths)
}

fn directory_entry_names(path: &Path) -> Vec<std::ffi::OsString> {
    let mut entries = std::fs::read_dir(path)
        .expect("state directory")
        .map(|entry| entry.expect("state entry").file_name())
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

fn wait_for_path(path: &Path, child: &mut std::process::Child) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        if let Some(status) = child.try_wait().expect("inspect fixture child") {
            panic!("crash-store fixture child exited before ready: {status}");
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for crash-store fixture child"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
