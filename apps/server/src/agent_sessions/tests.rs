use std::{fs, path::Path, time::SystemTime};

use serde_json::{Value, json};

use super::*;

const CLAUDE_ID: &str = "0f8b5c2e-4d6a-4b1c-9e3f-2a7d8c9b1e40";
const CODEX_ID: &str = "0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";
const FALLBACK: &str = "2026-10-01T00:00:00Z";

fn lines(records: &[Value]) -> String {
    records
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse(provider: AgentSessionProvider, records: &[Value]) -> Option<ParsedSession> {
    let text = lines(records);
    let canonical = match provider {
        AgentSessionProvider::ClaudeAgent => HashSet::new(),
        AgentSessionProvider::Codex => {
            codex_canonical_response_users(RecordReader::new(text.as_bytes()))
        }
    };
    build_session(
        provider,
        RecordReader::new(text.as_bytes()),
        &canonical,
        CLAUDE_ID,
        FALLBACK,
    )
}

fn texts(session: &ParsedSession) -> Vec<(&str, &str)> {
    session
        .messages
        .iter()
        .map(|message| (message.role, message.text.as_str()))
        .collect()
}

fn claude_user(text: &str) -> Value {
    json!({"type":"user","sessionId":CLAUDE_ID,"cwd":"/repo","timestamp":"2026-10-02T10:00:00.500Z","message":{"role":"user","content":text}})
}

fn claude_assistant(text: &str) -> Value {
    json!({"type":"assistant","sessionId":CLAUDE_ID,"timestamp":"2026-10-02T10:00:01Z","message":{"role":"assistant","model":"claude-opus-4-8","content":[{"type":"text","text":text},{"type":"tool_use","id":"t","name":"Bash","input":{}}]}})
}

fn codex_meta(cwd: &str) -> Value {
    json!({"type":"session_meta","timestamp":"2026-10-02T09:00:00Z","payload":{"id":CODEX_ID,"cwd":cwd,"base_instructions":{"text":"long"}}})
}

fn codex_response(role: &str, text: &str, turn_id: Option<&str>) -> Value {
    let mut payload = json!({"type":"message","role":role,"content":[{"type": if role == "user" {"input_text"} else {"output_text"},"text":text}]});
    if let Some(turn_id) = turn_id {
        payload["internal_chat_message_metadata_passthrough"] = json!({"turn_id":turn_id});
    }
    json!({"type":"response_item","timestamp":"2026-10-02T09:00:01Z","payload":payload})
}

fn codex_event(text: &str) -> Value {
    json!({"type":"event_msg","timestamp":"2026-10-02T09:00:02Z","payload":{"type":"user_message","message":text}})
}

#[test]
fn claude_keeps_visible_text_and_skips_sidechain_meta_and_compact_records() {
    let session = parse(
        AgentSessionProvider::ClaudeAgent,
        &[
            json!({"type":"summary","summary":"old"}),
            json!({"type":"user","isMeta":true,"message":{"content":"<command>meta</command>"}}),
            claude_user("Fix the login bug\nwith details"),
            json!({"type":"user","isSidechain":true,"message":{"content":"subagent prompt"}}),
            json!({"type":"user","isCompactSummary":true,"message":{"content":"summary of earlier"}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}}),
            claude_assistant("Fixed it."),
            json!({"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"API error"}]}}),
        ],
    )
    .expect("session");
    assert_eq!(session.session_id, CLAUDE_ID);
    assert_eq!(session.title, "Fix the login bug");
    assert_eq!(session.model.as_deref(), Some("claude-opus-4-8"));
    assert_eq!(session.message_count, 3);
    assert_eq!(
        texts(&session),
        [
            ("user", "Fix the login bug\nwith details"),
            ("assistant", "Fixed it."),
            ("assistant", "API error"),
        ]
    );
    assert_eq!(session.messages[0].created_at, "2026-10-02T10:00:00.500Z");
    assert_eq!(session.messages[2].created_at, FALLBACK);
}

#[test]
fn claude_prefers_the_ai_title_and_discards_sessions_without_a_user_message() {
    let titled = parse(
        AgentSessionProvider::ClaudeAgent,
        &[
            claude_user("hello"),
            json!({"type":"ai-title","aiTitle":"  Login bug investigation "}),
        ],
    )
    .expect("session");
    assert_eq!(titled.title, "Login bug investigation");
    assert!(parse(AgentSessionProvider::ClaudeAgent, &[claude_assistant("hi")]).is_none());
}

#[test]
fn claude_session_ids_must_be_uuids_to_resume() {
    assert!(is_resumable(AgentSessionProvider::ClaudeAgent, CLAUDE_ID));
    assert!(is_resumable(
        AgentSessionProvider::ClaudeAgent,
        &CLAUDE_ID.to_uppercase()
    ));
    for invalid in [
        "agent-1234",
        "0f8b5c2e4d6a4b1c9e3f2a7d8c9b1e40",
        "{0f8b5c2e-4d6a-4b1c-9e3f-2a7d8c9b1e40}",
        "0f8b5c2e-4d6a-0b1c-9e3f-2a7d8c9b1e40",
        "0f8b5c2e-4d6a-4b1c-7e3f-2a7d8c9b1e40",
    ] {
        assert!(
            !is_resumable(AgentSessionProvider::ClaudeAgent, invalid),
            "{invalid}"
        );
    }
}

#[test]
fn codex_keeps_one_copy_of_a_prompt_written_as_response_item_and_event() {
    let session = parse(
        AgentSessionProvider::Codex,
        &[
            codex_meta("/repo"),
            json!({"type":"turn_context","payload":{"model":"gpt-5.4","cwd":"/repo"}}),
            codex_response("user", "Add tests", None),
            codex_event("Add tests"),
            codex_response("assistant", "Done.", None),
            codex_event("Thanks"),
            codex_response("user", "Thanks", None),
        ],
    )
    .expect("session");
    assert_eq!(session.session_id, CODEX_ID);
    assert_eq!(session.model.as_deref(), Some("gpt-5.4"));
    assert_eq!(session.title, "Add tests");
    assert_eq!(session.message_count, 3);
    assert_eq!(
        texts(&session),
        [
            ("user", "Add tests"),
            ("assistant", "Done."),
            ("user", "Thanks")
        ]
    );
}

#[test]
fn codex_matches_copies_of_a_prompt_longer_than_the_import_limit() {
    let prompt = "x".repeat(MAX_MESSAGE_BYTES + 1);
    for records in [
        [codex_response("user", &prompt, None), codex_event(&prompt)],
        [codex_event(&prompt), codex_response("user", &prompt, None)],
    ] {
        let mut records = records.to_vec();
        records.insert(0, codex_meta("/repo"));
        let session = parse(AgentSessionProvider::Codex, &records).expect("session");
        assert_eq!(session.message_count, 1);
        assert!(session.messages[0].text.ends_with(TRUNCATION_NOTE));
    }
}

#[test]
fn codex_suppresses_generated_setup_text_in_a_turn_with_a_verbatim_event() {
    let session = parse(
        AgentSessionProvider::Codex,
        &[
            codex_meta("/repo"),
            codex_response(
                "user",
                "<environment_context>cwd</environment_context>",
                Some("turn-1"),
            ),
            codex_response("user", "Real prompt", Some("turn-1")),
            codex_event("Real prompt"),
            codex_response("assistant", "Answer", None),
        ],
    )
    .expect("session");
    assert_eq!(
        texts(&session),
        [("user", "Real prompt"), ("assistant", "Answer")]
    );
}

#[test]
fn codex_needs_session_metadata_for_a_resumable_id() {
    assert!(
        parse(
            AgentSessionProvider::Codex,
            &[
                codex_event("hello"),
                codex_response("assistant", "hi", None)
            ]
        )
        .is_none()
    );
}

#[test]
fn long_transcripts_keep_the_first_user_message_and_the_latest_messages() {
    let mut records = vec![claude_user("first prompt")];
    for index in 0..300 {
        records.push(claude_assistant(&format!("reply {index}")));
    }
    let session = parse(AgentSessionProvider::ClaudeAgent, &records).expect("session");
    assert_eq!(session.message_count, 301);
    assert_eq!(session.messages.len(), MAX_IMPORTED_THREAD_MESSAGES);
    assert_eq!(session.messages[0].text, "first prompt");
    assert_eq!(session.messages[1].text, "reply 101");
    assert_eq!(session.messages.last().unwrap().text, "reply 299");
}

#[test]
fn oversized_messages_keep_their_start_and_a_truncation_note() {
    let text = "é".repeat(MAX_MESSAGE_BYTES);
    let truncated = truncate_message(&text);
    assert!(truncated.ends_with(TRUNCATION_NOTE));
    assert!(truncated.len() <= MAX_MESSAGE_BYTES + TRUNCATION_NOTE.len());
    assert_eq!(truncate_message("short"), "short");
}

fn write(path: &Path, records: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, lines(records) + "\n").unwrap();
}

fn age(path: &Path, days: u64) {
    let modified = SystemTime::now() - Duration::from_secs(days * 24 * 60 * 60);
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
}

struct Homes {
    _temp: tempfile::TempDir,
    workspace: PathBuf,
    other: PathBuf,
    claude: PathBuf,
    codex: PathBuf,
}

fn homes() -> Homes {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("repo");
    let other = temp.path().join("other");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&other).unwrap();
    Homes {
        workspace: fs::canonicalize(&workspace).unwrap(),
        other: fs::canonicalize(&other).unwrap(),
        claude: temp.path().join("claude"),
        codex: temp.path().join("codex"),
        _temp: temp,
    }
}

fn sources(homes: &Homes) -> Vec<SessionSource> {
    vec![
        SessionSource {
            provider: AgentSessionProvider::ClaudeAgent,
            home: homes.claude.clone(),
        },
        SessionSource {
            provider: AgentSessionProvider::Codex,
            home: homes.codex.clone(),
        },
    ]
}

fn claude_session(id: &str, cwd: &Path, prompt: &str) -> Vec<Value> {
    vec![
        json!({"type":"user","sessionId":id,"cwd":cwd,"message":{"content":prompt}}),
        json!({"type":"assistant","sessionId":id,"cwd":cwd,"message":{"content":[{"type":"text","text":"ok"}]}}),
    ]
}

#[test]
fn scan_lists_recent_sessions_of_this_folder_newest_first() {
    let homes = homes();
    let claude_dir = homes.claude.join("projects/-repo");
    let current = claude_dir.join(format!("{CLAUDE_ID}.jsonl"));
    write(
        &current,
        &claude_session(CLAUDE_ID, &homes.workspace, "claude prompt"),
    );
    age(&current, 2);
    // Another folder, a stale session and a non-resumable session are left out.
    let elsewhere_id = "1f8b5c2e-4d6a-4b1c-9e3f-2a7d8c9b1e40";
    write(
        &homes
            .claude
            .join(format!("projects/-other/{elsewhere_id}.jsonl")),
        &claude_session(elsewhere_id, &homes.other, "elsewhere"),
    );
    let stale_id = "2f8b5c2e-4d6a-4b1c-9e3f-2a7d8c9b1e40";
    let stale = claude_dir.join(format!("{stale_id}.jsonl"));
    write(&stale, &claude_session(stale_id, &homes.workspace, "stale"));
    age(&stale, 31);
    write(
        &claude_dir.join("agent-abc.jsonl"),
        &claude_session("agent-abc", &homes.workspace, "not resumable"),
    );
    let rollout = homes.codex.join(format!(
        "sessions/2026/10/07/rollout-2026-10-07T10-00-00-{CODEX_ID}.jsonl"
    ));
    write(
        &rollout,
        &[
            codex_meta(homes.workspace.to_str().unwrap()),
            codex_event("codex prompt"),
            codex_response("assistant", "done", None),
        ],
    );
    age(&rollout, 1);

    let result = scan(
        &sources(&homes),
        &homes.workspace,
        SystemTime::now(),
        &HashSet::new(),
        &CancellationToken::new(),
    );

    assert!(!result.truncated);
    assert!(
        result
            .candidates
            .iter()
            .all(|candidate| candidate.session.messages.is_empty()),
        "a scan keeps no transcript bodies"
    );
    let listed = result
        .candidates
        .iter()
        .map(|candidate| {
            (
                candidate.provider,
                candidate.session.session_id.as_str(),
                candidate.session.title.as_str(),
                candidate.session.message_count,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        listed,
        [
            (AgentSessionProvider::Codex, CODEX_ID, "codex prompt", 2),
            (
                AgentSessionProvider::ClaudeAgent,
                CLAUDE_ID,
                "claude prompt",
                2
            ),
        ]
    );
    // Sessions a Coddy Studio thread already runs are not offered.
    let owned = HashSet::from([CODEX_ID.to_owned()]);
    let result = scan(
        &sources(&homes),
        &homes.workspace,
        SystemTime::now(),
        &owned,
        &CancellationToken::new(),
    );
    assert_eq!(result.candidates.len(), 1);
    assert_eq!(result.candidates[0].session.session_id, CLAUDE_ID);
    // A cancelled scan (its RPC was interrupted) stops before reading transcripts.
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let result = scan(
        &sources(&homes),
        &homes.workspace,
        SystemTime::now(),
        &HashSet::new(),
        &cancelled,
    );
    assert!(result.candidates.is_empty());
}

#[test]
fn load_rechecks_the_folder_and_finds_codex_rollouts_by_id() {
    let homes = homes();
    let rollout = homes.codex.join(format!(
        "sessions/2026/10/07/rollout-2026-10-07T10-00-00-{CODEX_ID}.jsonl"
    ));
    write(
        &rollout,
        &[
            codex_meta(homes.workspace.to_str().unwrap()),
            codex_event("codex prompt"),
        ],
    );
    let codex = &sources(&homes)[1];
    let loaded = load(codex, CODEX_ID, &homes.workspace, SystemTime::now()).expect("loads");
    assert_eq!(loaded.messages.len(), 1);
    assert_eq!(
        load(codex, CODEX_ID, &homes.other, SystemTime::now()).unwrap_err(),
        LoadFailure::DifferentDirectory
    );
    assert_eq!(
        load(codex, "missing", &homes.workspace, SystemTime::now()).unwrap_err(),
        LoadFailure::NotFound
    );
    let claude = &sources(&homes)[0];
    assert_eq!(
        load(claude, "agent-abc", &homes.workspace, SystemTime::now()).unwrap_err(),
        LoadFailure::NotResumable
    );
}

#[test]
fn a_cancelled_scan_stops_reading_a_transcript_between_records() {
    let text = lines(&[
        json!({"type": "user", "sessionId": CLAUDE_ID, "message": {"content": "first"}}),
        json!({"type": "assistant", "sessionId": CLAUDE_ID, "message": {"content": "second"}}),
    ]);
    let cancellation = CancellationToken::new();
    let mut live = RecordReader::new(text.as_bytes()).with_cancellation(&cancellation);
    assert!(live.next().is_some());
    cancellation.cancel();
    assert!(live.next().is_none());
    assert!(live.failed, "a cancelled read is not a complete transcript");
}
