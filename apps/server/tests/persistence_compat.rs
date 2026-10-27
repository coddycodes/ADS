use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use coddy_studio_server::persistence::run_migrations;
use rusqlite::{Connection, OpenFlags, Params, types::ValueRef};
use serde::Deserialize;
use tempfile::TempDir;

const FIXED_LEDGER_TIMESTAMP: &str = "2026-01-01 00:00:00";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    current_migration_id: u32,
    application_tables: Vec<String>,
    scenarios: Vec<Scenario>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Scenario {
    id: String,
    files: ScenarioFiles,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScenarioFiles {
    database: String,
    migrated_database: String,
}

#[test]
fn rust_migrations_match_typescript_for_every_golden_database_state() {
    let fixture_root = fixture_root();
    let manifest: Manifest = serde_json::from_slice(
        &fs::read(fixture_root.join("manifest.json")).expect("persistence fixture manifest"),
    )
    .expect("valid persistence fixture manifest");
    assert_eq!(manifest.current_migration_id, 33);
    assert_eq!(manifest.application_tables.len(), 15);

    for scenario in manifest.scenarios {
        let temporary = TempDir::new().expect("temporary migration directory");
        let actual_path = temporary.path().join("actual.sqlite");
        let expected_path = temporary.path().join("expected.sqlite");
        fs::copy(fixture_root.join(&scenario.files.database), &actual_path)
            .expect("copy source fixture");
        fs::copy(
            fixture_root.join(&scenario.files.migrated_database),
            &expected_path,
        )
        .expect("copy TypeScript-migrated fixture");

        let mut actual = Connection::open(&actual_path).expect("open copied fixture");
        actual
            .execute_batch("PRAGMA foreign_keys = ON")
            .expect("enable foreign keys");
        run_migrations(&mut actual, Some(manifest.current_migration_id))
            .expect("Rust migrations through the TypeScript fixture boundary succeed");
        actual
            .execute(
                "UPDATE effect_sql_migrations SET created_at = ?",
                [FIXED_LEDGER_TIMESTAMP],
            )
            .expect("normalize migration timestamps");

        let expected = Connection::open_with_flags(expected_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open TypeScript-migrated fixture");

        assert_eq!(
            schema_snapshot(&actual),
            schema_snapshot(&expected),
            "schema mismatch for {}",
            scenario.id
        );
        assert_eq!(
            data_snapshot(&actual, &manifest.application_tables),
            data_snapshot(&expected, &manifest.application_tables),
            "logical row mismatch for {}",
            scenario.id
        );
    }
}

#[test]
fn migration_40_preserves_the_default_policy_for_a_version_39_project_projection() {
    let mut connection = Connection::open_in_memory().expect("open in-memory database");
    run_migrations(&mut connection, Some(39)).expect("migrate through version 39");
    connection
        .execute(
            "INSERT INTO projection_projects (project_id, title, workspace_root, default_model_selection_json, scripts_json, created_at, updated_at, deleted_at) VALUES ('project-legacy', 'Legacy', 'C:/legacy', NULL, '[]', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', NULL)",
            [],
        )
        .expect("insert version 39 projection");

    run_migrations(&mut connection, None).expect("migrate version 39 projection to current");

    let policy = connection
        .query_row(
            "SELECT worktree_discovery_json FROM projection_projects WHERE project_id = 'project-legacy'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("read migrated policy");
    assert_eq!(
        policy,
        r#"{"visibility":"hidden","initialPromptDismissedAt":null,"baselinePaths":[]}"#
    );
}

#[test]
fn migration_41_preserves_a_null_repository_pin_for_a_legacy_project() {
    let mut connection = Connection::open_in_memory().expect("open in-memory database");
    run_migrations(&mut connection, Some(40)).expect("migrate through version 40");
    connection
        .execute(
            "INSERT INTO projection_projects (project_id, title, workspace_root, default_model_selection_json, scripts_json, worktree_discovery_json, created_at, updated_at, deleted_at) VALUES ('project-legacy-pin', 'Legacy', 'C:/legacy', NULL, '[]', '{}', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', NULL)",
            [],
        )
        .expect("insert version 40 projection");

    run_migrations(&mut connection, None).expect("migrate version 40 projection to current");

    let repository_key = connection
        .query_row(
            "SELECT worktree_repository_key FROM projection_projects WHERE project_id = 'project-legacy-pin'",
            [],
            |row| row.get::<_, Option<String>>(0),
        )
        .expect("read migrated repository pin");
    assert_eq!(repository_key, None);
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/contracts/fixtures/persistence")
}

fn schema_snapshot(connection: &Connection) -> BTreeMap<String, Vec<Vec<String>>> {
    let table_names = query_snapshot(
        connection,
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        [],
    );
    let mut snapshot = BTreeMap::from([("tables".to_owned(), table_names.clone())]);
    for table_row in table_names {
        let table = table_row
            .first()
            .and_then(|value| value.strip_prefix("text:"))
            .expect("table name encoding");
        snapshot.insert(
            format!("table:{table}:columns"),
            query_snapshot(
                connection,
                "SELECT cid, name, type, `notnull`, dflt_value, pk, hidden FROM pragma_table_xinfo(?) ORDER BY cid",
                [table],
            ),
        );
        let indexes = query_snapshot(
            connection,
            "SELECT seq, name, `unique`, origin, partial FROM pragma_index_list(?) ORDER BY name",
            [table],
        );
        snapshot.insert(format!("table:{table}:indexes"), indexes.clone());
        for index_row in indexes {
            let index = index_row
                .get(1)
                .and_then(|value| value.strip_prefix("text:"))
                .expect("index name encoding");
            snapshot.insert(
                format!("index:{index}:columns"),
                query_snapshot(
                    connection,
                    "SELECT seqno, cid, name, `desc`, coll, key FROM pragma_index_xinfo(?) ORDER BY seqno",
                    [index],
                ),
            );
        }
        snapshot.insert(
            format!("table:{table}:foreign-keys"),
            query_snapshot(
                connection,
                "SELECT id, seq, `table`, `from`, `to`, on_update, on_delete, match FROM pragma_foreign_key_list(?) ORDER BY id, seq",
                [table],
            ),
        );
    }
    snapshot.insert(
        "objects".to_owned(),
        query_snapshot(
            connection,
            "SELECT type, name, tbl_name FROM sqlite_schema WHERE type IN ('trigger', 'view') ORDER BY type, name",
            [],
        ),
    );
    snapshot
}

fn data_snapshot(
    connection: &Connection,
    application_tables: &[String],
) -> BTreeMap<String, Vec<Vec<String>>> {
    let mut tables = application_tables.to_vec();
    tables.push("effect_sql_migrations".to_owned());
    if connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'sqlite_sequence')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .expect("sqlite_sequence inventory")
    {
        tables.push("sqlite_sequence".to_owned());
    }
    tables
        .into_iter()
        .map(|table| {
            let sql = format!("SELECT * FROM {}", quote_identifier(&table));
            (table, query_snapshot(connection, &sql, []))
        })
        .collect()
}

fn query_snapshot<P>(connection: &Connection, sql: &str, parameters: P) -> Vec<Vec<String>>
where
    P: Params,
{
    let mut statement = connection.prepare(sql).expect("prepare snapshot query");
    let column_count = statement.column_count();
    let mut rows = statement.query(parameters).expect("execute snapshot query");
    let mut snapshot = Vec::new();
    while let Some(row) = rows.next().expect("read snapshot row") {
        snapshot.push(
            (0..column_count)
                .map(|column| encode_value(row.get_ref(column).expect("snapshot value")))
                .collect(),
        );
    }
    snapshot.sort();
    snapshot
}

fn encode_value(value: ValueRef<'_>) -> String {
    match value {
        ValueRef::Null => "null".to_owned(),
        ValueRef::Integer(value) => format!("integer:{value}"),
        ValueRef::Real(value) => format!("real:{:016x}", value.to_bits()),
        ValueRef::Text(value) => format!("text:{}", String::from_utf8_lossy(value)),
        ValueRef::Blob(value) => format!("blob:{}", encode_hex(value)),
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}
