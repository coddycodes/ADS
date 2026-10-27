#![cfg(unix)]

#[path = "support/executable_fixture.rs"]
mod executable_fixture;

use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use coddy_studio_server::persistence::{Database, run_migrations};
use coddy_studio_server::pull_requests::{
    ContextRead, PullRequestsService,
    host::HostCommandRunner,
    model::{Changed, Context, ListQuery, SubscribeInput},
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
};
use tokio_util::sync::CancellationToken;

const OPEN_MRS: u64 = 240;
const CLOSED_MRS: u64 = 35;
const MERGED_MRS: u64 = 80;
const PAGE_SIZE: usize = 30;
const SUGGESTION_NOTES: usize = 12;
const PLAIN_NOTES: usize = 8;
const MR_NUMBER: u64 = 42;

#[derive(Clone)]
struct Hit {
    method: String,
    path: String,
    status: u16,
    start_ms: u128,
    end_ms: u128,
}

struct Api {
    started: Instant,
    rtt_ms: AtomicU64,
    hits: Mutex<Vec<Hit>>,
    inflight: AtomicUsize,
    max_inflight: AtomicUsize,
    list_page: String,
    detail: String,
    timeline: String,
    metadata: String,
}

impl Api {
    fn snapshot_and_clear(&self) -> (Vec<Hit>, usize) {
        let hits = std::mem::take(&mut *self.hits.lock().expect("hit log"));
        let max_inflight = self.max_inflight.swap(0, Ordering::AcqRel);
        (hits, max_inflight)
    }
}

struct Phase {
    wall_ms: u128,
    http: usize,
    http_errors: usize,
    max_inflight: usize,
    cli: usize,
    paths: Vec<String>,
}

#[tokio::test]
async fn gitlab_list_and_merge_request_loads_record_requests_and_wall_time() {
    let rtt_ms = std::env::var("CODDY_STUDIO_GITLAB_HARNESS_RTT_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(200);
    let runs = std::env::var("CODDY_STUDIO_GITLAB_HARNESS_RUNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|runs: &usize| *runs > 0)
        .unwrap_or(5);
    let root = TempDir::new().expect("tempdir");
    let cwd = root.path().join("checkout");
    fs::create_dir(&cwd).expect("checkout dir");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("local addr");
    let api = Arc::new(Api {
        started: Instant::now(),
        rtt_ms: AtomicU64::new(rtt_ms),
        hits: Mutex::new(Vec::new()),
        inflight: AtomicUsize::new(0),
        max_inflight: AtomicUsize::new(0),
        list_page: list_page(),
        detail: detail_body(),
        timeline: timeline_body(),
        metadata: metadata_body(),
    });
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server = tokio::spawn(serve(listener, Arc::clone(&api), shutdown_rx));
    let log_path = root.path().join("glab-calls.jsonl");
    let glab = write_glab(root.path(), &format!("http://{address}"), &log_path);
    let gh = write_gh(root.path());
    let runner = HostCommandRunner::new(root.path().join("state")).with_commands(gh, glab, "git");
    let token = CancellationToken::new();
    runner
        .git(&cwd, &["init", "-q", "-b", "main"], &token)
        .await
        .expect("git init");
    runner
        .git(
            &cwd,
            &[
                "remote",
                "add",
                "origin",
                "ssh://git@git.acme.example/team/sub/repo.git",
            ],
            &token,
        )
        .await
        .expect("git remote");

    api.rtt_ms.store(0, Ordering::Release);
    let overhead = run_once(&runner, &cwd, &api, &log_path).await;
    api.rtt_ms.store(rtt_ms, Ordering::Release);
    let mut samples = Vec::with_capacity(runs);
    for _ in 0..runs {
        samples.push(run_once(&runner, &cwd, &api, &log_path).await);
    }
    let _ = shutdown_tx.send(());
    server.await.expect("server task");

    let list_walls = walls(&samples, |sample| sample.list.wall_ms);
    let mr_walls = walls(&samples, |sample| sample.mr.wall_ms);
    let report = json!({
        "measures": "PullRequestsService context+list, then get+timeline, on one fresh service per run. Wall time includes glab process startup and the injected server sleep. The 0 ms run is an overhead probe and is not part of the median.",
        "rttMs": rtt_ms,
        "runs": runs,
        "dataset": {
            "openMergeRequests": OPEN_MRS,
            "closedMergeRequests": CLOSED_MRS,
            "mergedMergeRequests": MERGED_MRS,
            "listPageSize": PAGE_SIZE,
            "suggestionNotes": SUGGESTION_NOTES,
            "plainNotes": PLAIN_NOTES,
        },
        "overheadProbeRtt0": {
            "listWallMs": overhead.list.wall_ms,
            "listHttp": overhead.list.http,
            "mergeRequestWallMs": overhead.mr.wall_ms,
            "mergeRequestHttp": overhead.mr.http,
        },
        "list": summarize(&samples, &list_walls, |sample| &sample.list),
        "mergeRequest": summarize(&samples, &mr_walls, |sample| &sample.mr),
        "medianRunPaths": {
            "list": sample_with_wall(&samples, list_walls[list_walls.len() / 2], |sample| sample.list.wall_ms).list.paths,
            "mergeRequest": sample_with_wall(&samples, mr_walls[mr_walls.len() / 2], |sample| sample.mr.wall_ms).mr.paths,
        },
    });
    let rendered = serde_json::to_string_pretty(&report).expect("report json");
    println!("HARNESS_JSON {rendered}");
    let report_path = std::env::var("CODDY_STUDIO_GITLAB_HARNESS_REPORT")
        .unwrap_or_else(|_| "/tmp/coddy-studio-gitlab-mr-load.json".to_owned());
    fs::write(&report_path, &rendered).expect("write report");

    for sample in &samples {
        assert_eq!(sample.list_rows, PAGE_SIZE, "list page");
        assert_eq!(sample.list_total, Some(OPEN_MRS), "open total");
        assert_eq!(sample.detail_number, MR_NUMBER, "detail number");
        assert!(sample.timeline_items >= SUGGESTION_NOTES, "timeline items");
        assert_eq!(
            sample.list.http_errors,
            0,
            "list http errors: {}",
            sample.list.paths.join(" ")
        );
        assert_eq!(
            sample.mr.http_errors,
            0,
            "mr http errors: {}",
            sample.mr.paths.join(" ")
        );
        assert_eq!(
            sample.list.http, samples[0].list.http,
            "list request count drifted"
        );
        assert_eq!(
            sample.mr.http, samples[0].mr.http,
            "mr request count drifted"
        );
    }
    assert_eq!(samples[0].list.http, 7, "list http requests");
    assert_eq!(samples[0].mr.http, 26, "merge request http requests");
    if rtt_ms >= 100 {
        let list_inflight = samples
            .iter()
            .map(|sample| sample.list.max_inflight)
            .min()
            .unwrap_or(0);
        let mr_inflight = samples
            .iter()
            .map(|sample| sample.mr.max_inflight)
            .min()
            .unwrap_or(0);
        assert!(
            list_inflight >= 7,
            "list host reads ran apart: max in flight {list_inflight}"
        );
        assert!(
            mr_inflight >= 8,
            "merge request host reads ran apart: max in flight {mr_inflight}"
        );
    }
}

#[tokio::test]
#[ignore = "waits through real 20 s poll ticks; run via scripts/measure-gitlab-merge-request-load.sh"]
async fn gitlab_subscribed_merge_request_polling_records_host_requests() {
    const RTT_MS: u64 = 200;
    let root = TempDir::new().expect("tempdir");
    let cwd = root.path().join("checkout");
    fs::create_dir(&cwd).expect("checkout dir");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("local addr");
    let api = Arc::new(Api {
        started: Instant::now(),
        rtt_ms: AtomicU64::new(RTT_MS),
        hits: Mutex::new(Vec::new()),
        inflight: AtomicUsize::new(0),
        max_inflight: AtomicUsize::new(0),
        list_page: list_page(),
        detail: detail_body(),
        timeline: timeline_body(),
        metadata: metadata_body(),
    });
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server = tokio::spawn(serve(listener, Arc::clone(&api), shutdown_rx));
    let log_path = root.path().join("glab-calls.jsonl");
    let glab = write_glab(root.path(), &format!("http://{address}"), &log_path);
    let gh = write_gh(root.path());
    let runner = HostCommandRunner::new(root.path().join("state")).with_commands(gh, glab, "git");
    let token = CancellationToken::new();
    runner
        .git(&cwd, &["init", "-q", "-b", "main"], &token)
        .await
        .expect("git init");
    runner
        .git(
            &cwd,
            &[
                "remote",
                "add",
                "origin",
                "ssh://git@git.acme.example/team/sub/repo.git",
            ],
            &token,
        )
        .await
        .expect("git remote");
    let database = open_database().await;
    let service = PullRequestsService::with_runner(runner.clone()).with_database(database);

    let detail_band = measure_detail_band(&service, &cwd).await;

    let _ = fs::write(&log_path, "");
    api.snapshot_and_clear();
    let empty_snapshot = service
        .read_snapshot(subscribe_input(&cwd, None), &CancellationToken::new())
        .await
        .expect("empty read snapshot");
    assert!(empty_snapshot.list.is_none());
    assert!(empty_snapshot.detail.is_none());
    assert_eq!(host_requests(&api, &log_path), 0, "empty read_snapshot");

    // The detail join belongs to the unary reads measured in the band; a
    // number the poller has never probed only names every family once.
    let cold_probe = subscribe_until(
        &service,
        &cwd,
        &api,
        &log_path,
        subscribe_input(&cwd, Some(MR_NUMBER)),
        Duration::from_secs(45),
        |sample| {
            count_glab_mr_list(sample) >= 1 && probe_hits(sample) >= 1 && sample.events.len() >= 2
        },
    )
    .await;
    assert_eq!(
        probe_hits(&cold_probe),
        1,
        "cold probe: {}",
        format_tick_sample(&cold_probe)
    );
    assert_eq!(
        detail_join_hits(&cold_probe),
        0,
        "the poller never runs the detail join: {}",
        format_tick_sample(&cold_probe)
    );
    assert!(
        cold_probe
            .events
            .iter()
            .any(|changed| changed.detail && changed.timeline && changed.checks),
        "an unprobed number names detail, timeline, and checks for the client to re-read"
    );

    let _ = fs::write(&log_path, "");
    api.snapshot_and_clear();
    let stored_snapshot = service
        .read_snapshot(
            subscribe_input(&cwd, Some(MR_NUMBER)),
            &CancellationToken::new(),
        )
        .await
        .expect("stored read snapshot");
    assert!(stored_snapshot.detail.is_some());
    assert_eq!(host_requests(&api, &log_path), 0, "stored read_snapshot");

    let seeded_list = subscribe_until(
        &service,
        &cwd,
        &api,
        &log_path,
        subscribe_input(&cwd, None),
        Duration::from_secs(45),
        |sample| count_glab_mr_list(sample) >= 1,
    )
    .await;
    assert_eq!(
        count_glab_mr_list(&seeded_list),
        1,
        "seed list tick: {}",
        format_tick_sample(&seeded_list)
    );
    assert_eq!(
        detail_join_hits(&seeded_list),
        0,
        "seed list tick detail join: {}",
        format_tick_sample(&seeded_list)
    );

    let unchanged_list = subscribe_until(
        &service,
        &cwd,
        &api,
        &log_path,
        subscribe_input(&cwd, None),
        Duration::from_secs(45),
        |sample| count_glab_mr_list(sample) >= 1,
    )
    .await;
    assert_eq!(
        count_glab_mr_list(&unchanged_list),
        1,
        "unchanged list tick: {}",
        format_tick_sample(&unchanged_list)
    );
    assert_eq!(
        detail_join_hits(&unchanged_list),
        0,
        "unchanged list tick detail join: {}",
        format_tick_sample(&unchanged_list)
    );

    let unchanged_detail = subscribe_until(
        &service,
        &cwd,
        &api,
        &log_path,
        subscribe_input(&cwd, Some(MR_NUMBER)),
        Duration::from_secs(45),
        |sample| count_glab_mr_list(sample) >= 1 && probe_hits(sample) >= 1,
    )
    .await;
    assert_eq!(
        count_glab_mr_list(&unchanged_detail),
        1,
        "unchanged detail tick list: {}",
        format_tick_sample(&unchanged_detail)
    );
    assert_eq!(
        probe_hits(&unchanged_detail),
        1,
        "unchanged detail tick probe: {}",
        format_tick_sample(&unchanged_detail)
    );
    assert_eq!(
        detail_join_hits(&unchanged_detail),
        0,
        "unchanged detail tick detail join: {}",
        format_tick_sample(&unchanged_detail)
    );

    let report = json!({
        "rttMs": RTT_MS,
        "detailBandMs": {
            "minWallMs": detail_band.min_wall_ms,
            "medianWallMs": detail_band.median_wall_ms,
            "maxWallMs": detail_band.max_wall_ms,
        },
        "coldProbeTick": {
            "tickWallMs": cold_probe.wall_ms,
            "listClassRequests": count_glab_mr_list(&cold_probe),
            "probeHits": probe_hits(&cold_probe),
            "detailJoinHits": detail_join_hits(&cold_probe),
        },
        "unchangedListTick": {
            "listClassRequests": count_glab_mr_list(&unchanged_list),
            "detailJoinHits": detail_join_hits(&unchanged_list),
        },
        "unchangedDetailTick": {
            "listClassRequests": count_glab_mr_list(&unchanged_detail),
            "probeHits": probe_hits(&unchanged_detail),
            "detailJoinHits": detail_join_hits(&unchanged_detail),
        },
    });
    println!(
        "HARNESS_SUBSCRIBE_JSON {}",
        serde_json::to_string(&report).unwrap()
    );

    let _ = shutdown_tx.send(());
    server.await.expect("server task");
}

struct DetailBand {
    min_wall_ms: u128,
    max_wall_ms: u128,
    median_wall_ms: u128,
}

struct TickSample {
    wall_ms: u128,
    hits: Vec<Hit>,
    glab_args: Vec<Vec<String>>,
    events: Vec<Changed>,
}

async fn open_database() -> Database {
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| Ok(run_migrations(connection, None)?))
        .await
        .expect("migrations");
    database
}

fn subscribe_input(cwd: &Path, number: Option<u64>) -> SubscribeInput {
    let mut value = serde_json::to_value(list_query(cwd)).expect("list query value");
    if let Some(object) = value.as_object_mut() {
        object.insert("number".into(), json!(number));
        object.insert("tab".into(), Value::Null);
    }
    serde_json::from_value(value).expect("subscribe input")
}

async fn measure_detail_band(service: &PullRequestsService, cwd: &Path) -> DetailBand {
    let mut walls = Vec::new();
    for _ in 0..3 {
        let started = Instant::now();
        let detail_service = service.clone();
        let detail_token = CancellationToken::new();
        let timeline_token = CancellationToken::new();
        let (detail, timeline) = tokio::join!(
            service.get(cwd, MR_NUMBER, &detail_token),
            detail_service.timeline(cwd, MR_NUMBER, &timeline_token),
        );
        detail.expect("detail rpc");
        timeline.expect("timeline rpc");
        walls.push(started.elapsed().as_millis());
    }
    walls.sort_unstable();
    DetailBand {
        min_wall_ms: walls[0],
        max_wall_ms: walls[walls.len() - 1],
        median_wall_ms: walls[walls.len() / 2],
    }
}

async fn subscribe_until(
    service: &PullRequestsService,
    _cwd: &Path,
    api: &Api,
    log_path: &Path,
    input: SubscribeInput,
    timeout: Duration,
    done: impl Fn(&TickSample) -> bool,
) -> TickSample {
    let _ = fs::write(log_path, "");
    api.snapshot_and_clear();
    let events = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&events);
    let emit_changed = move |event: Changed| recorded.lock().expect("events").push(event);
    let subscribe_token = CancellationToken::new();
    let subscribe_cancel = subscribe_token.clone();
    let subscribe_service = service.clone();
    let subscribe_input = input;
    let subscribe_handle = tokio::spawn(async move {
        subscribe_service
            .subscribe(subscribe_input, &emit_changed, &subscribe_token)
            .await
    });
    let started = Instant::now();
    let deadline = started + timeout;
    let mut tick_started = None;
    let mut idle_polls = 0_u32;
    while Instant::now() < deadline {
        let hits = api.hits.lock().expect("hit log").clone();
        let glab_args = read_glab_args(log_path);
        if !hits.is_empty() && tick_started.is_none() {
            tick_started = Some(Instant::now());
        }
        let sample = TickSample {
            wall_ms: tick_started
                .map(|tick| tick.elapsed().as_millis())
                .unwrap_or(0),
            hits,
            glab_args,
            events: events.lock().expect("events").clone(),
        };
        if api.inflight.load(Ordering::Acquire) == 0 {
            idle_polls += 1;
        } else {
            idle_polls = 0;
        }
        if done(&sample) && idle_polls >= 4 {
            subscribe_cancel.cancel();
            let _ = subscribe_handle.await;
            return sample;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    subscribe_cancel.cancel();
    let _ = subscribe_handle.await;
    panic!(
        "subscribe tick timed out after {} ms; hits={}",
        timeout.as_millis(),
        format_hits(&api.hits.lock().expect("hit log"))
    );
}

fn host_requests(api: &Api, log_path: &Path) -> usize {
    let hits = api.hits.lock().expect("hit log").len();
    let cli = read_glab_args(log_path).len();
    assert_eq!(hits, cli, "http hits and glab calls diverged");
    hits
}

fn read_glab_args(log_path: &Path) -> Vec<Vec<String>> {
    fs::read_to_string(log_path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            serde_json::from_str::<Value>(line).expect("glab log line")["args"]
                .as_array()
                .expect("glab args")
                .iter()
                .map(|value| value.as_str().expect("glab arg").to_owned())
                .collect()
        })
        .collect()
}

fn count_glab_mr_list(sample: &TickSample) -> usize {
    sample
        .glab_args
        .iter()
        .filter(|args| args.len() >= 2 && args[0] == "mr" && args[1] == "list")
        .count()
}

fn probe_hits(sample: &TickSample) -> usize {
    probe_hits_in(&sample.hits)
}

fn detail_join_hits(sample: &TickSample) -> usize {
    detail_join_hits_in(&sample.hits)
}

const PROJECT_PATH: &str = "/api/v4/projects/team%2Fsub%2Frepo";

fn merge_request_path(number: u64) -> String {
    format!("{PROJECT_PATH}/merge_requests/{number}")
}

fn is_probe_path(path: &str, number: u64) -> bool {
    path == merge_request_path(number)
}

fn is_detail_join_path(path: &str, number: u64) -> bool {
    let prefix = merge_request_path(number);
    path.starts_with(&prefix)
        && path != prefix
        && (path.contains("/approvals")
            || path.contains("/reviewers")
            || path.contains("/approval_state")
            || path.contains("/award_emoji")
            || path.contains("/closes_issues")
            || path.contains("/notes/")
            || path.contains("/pipelines")
            || path.contains("/resource_")
            || path.contains("/versions"))
        || path == "/api/v4/graphql"
}

fn probe_hits_in(hits: &[Hit]) -> usize {
    hits.iter()
        .filter(|hit| hit.method == "GET" && is_probe_path(&hit.path, MR_NUMBER))
        .count()
}

fn detail_join_hits_in(hits: &[Hit]) -> usize {
    hits.iter()
        .filter(|hit| is_detail_join_path(&hit.path, MR_NUMBER))
        .count()
}

fn format_hits(hits: &[Hit]) -> String {
    hits.iter()
        .map(|hit| format!("{} {}", hit.method, hit.path))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_tick_sample(sample: &TickSample) -> String {
    format!(
        "http=[{}] glab={}",
        format_hits(&sample.hits),
        sample
            .glab_args
            .iter()
            .map(|args| args.join(" "))
            .collect::<Vec<_>>()
            .join("; ")
    )
}

struct Sample {
    list: Phase,
    mr: Phase,
    list_rows: usize,
    list_total: Option<u64>,
    detail_number: u64,
    timeline_items: usize,
}

async fn run_once(runner: &HostCommandRunner, cwd: &Path, api: &Api, log_path: &Path) -> Sample {
    let _ = fs::write(log_path, "");
    api.snapshot_and_clear();
    let service = PullRequestsService::with_runner(runner.clone());
    let query = list_query(cwd);
    let started = Instant::now();
    let list_service = service.clone();
    let cwd_buf = cwd.to_path_buf();
    let context_token = CancellationToken::new();
    let list_token = CancellationToken::new();
    let (context, page) = tokio::join!(
        service.context(&cwd_buf, ContextRead::Open, &context_token),
        list_service.list(&cwd_buf, query, &list_token),
    );
    let list_wall = started.elapsed().as_millis();
    let context = context.expect("context rpc");
    assert!(
        matches!(context, Context::Available { .. }),
        "context unavailable: {context:?}"
    );
    let page = page.expect("list rpc");
    let list = phase(api, log_path, list_wall);

    let _ = fs::write(log_path, "");
    api.snapshot_and_clear();
    let started = Instant::now();
    let detail_service = service.clone();
    let detail_token = CancellationToken::new();
    let timeline_token = CancellationToken::new();
    let (detail, timeline) = tokio::join!(
        service.get(&cwd_buf, MR_NUMBER, &detail_token),
        detail_service.timeline(&cwd_buf, MR_NUMBER, &timeline_token),
    );
    let mr_wall = started.elapsed().as_millis();
    let detail = detail.expect("detail rpc");
    let timeline = timeline.expect("timeline rpc");
    let mr = phase(api, log_path, mr_wall);
    Sample {
        list_rows: page.rows.len(),
        list_total: page.total_count,
        detail_number: detail.number,
        timeline_items: timeline.items.len(),
        list,
        mr,
    }
}

fn phase(api: &Api, log_path: &Path, wall_ms: u128) -> Phase {
    let (hits, max_inflight) = api.snapshot_and_clear();
    let cli = fs::read_to_string(log_path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .count();
    Phase {
        wall_ms,
        http: hits.len(),
        http_errors: hits.iter().filter(|hit| hit.status != 200).count(),
        max_inflight,
        cli,
        paths: hits
            .iter()
            .map(|hit| {
                format!(
                    "{} {} @{}ms {}ms {}",
                    hit.method,
                    hit.status,
                    hit.start_ms,
                    hit.end_ms.saturating_sub(hit.start_ms),
                    hit.path
                )
            })
            .collect(),
    }
}

fn walls(samples: &[Sample], pick: impl Fn(&Sample) -> u128) -> Vec<u128> {
    let mut walls: Vec<_> = samples.iter().map(pick).collect();
    walls.sort_unstable();
    walls
}

fn sample_with_wall(samples: &[Sample], wall: u128, pick: impl Fn(&Sample) -> u128) -> &Sample {
    samples
        .iter()
        .find(|sample| pick(sample) == wall)
        .unwrap_or(&samples[0])
}

fn summarize(samples: &[Sample], sorted_walls: &[u128], pick: impl Fn(&Sample) -> &Phase) -> Value {
    let phase = pick(&samples[0]);
    json!({
        "medianWallMs": sorted_walls[sorted_walls.len() / 2],
        "minWallMs": sorted_walls[0],
        "maxWallMs": sorted_walls[sorted_walls.len() - 1],
        "httpRequests": phase.http,
        "maxInFlight": samples.iter().map(|sample| pick(sample).max_inflight).max().unwrap_or(0),
        "cliProcesses": phase.cli,
    })
}

fn list_query(cwd: &Path) -> ListQuery {
    serde_json::from_value(json!({
        "cwd": cwd,
        "state": "open",
        "search": null,
        "author": null,
        "assignee": null,
        "reviewer": null,
        "reviewStatus": null,
        "draft": null,
        "labels": [],
        "milestone": null,
        "targetBranch": null,
        "sort": "newest",
        "cursor": null,
    }))
    .expect("list query")
}

fn list_page() -> String {
    let row: Value = serde_json::from_str(include_str!("fixtures/pull_requests/gitlab_row.json"))
        .expect("row fixture");
    let rows: Vec<_> = (1..=PAGE_SIZE)
        .map(|number| {
            let mut row = row.clone();
            row["iid"] = json!(number);
            row["title"] = json!(format!("Merge request {number}"));
            row["web_url"] = json!(format!(
                "https://git.acme.example/team/sub/repo/-/merge_requests/{number}"
            ));
            row
        })
        .collect();
    serde_json::to_string(&rows).expect("list json")
}

fn detail_body() -> String {
    let mut detail: Value =
        serde_json::from_str(include_str!("fixtures/pull_requests/gitlab_detail.json"))
            .expect("detail fixture");
    detail["iid"] = json!(MR_NUMBER);
    detail["web_url"] = json!("https://git.acme.example/team/sub/repo/-/merge_requests/42");
    detail["description"] = json!("A realistic description body for the merge request.");
    serde_json::to_string(&detail).expect("detail json")
}

fn metadata_body() -> String {
    json!({
        "data": {
            "project": {
                "mergeRequest": {
                    "commitCount": 4,
                    "resolvableDiscussionsCount": SUGGESTION_NOTES,
                    "diffStatsSummary": {"additions": 120, "deletions": 40, "fileCount": 8},
                    "sourceProject": {"fullPath": "team/sub/repo"},
                    "userPermissions": {"createNote": true, "pushToSourceBranch": true}
                }
            }
        }
    })
    .to_string()
}

fn timeline_body() -> String {
    let mut notes = Vec::new();
    for index in 0..SUGGESTION_NOTES {
        notes.push(json!({
            "id": format!("gid://gitlab/Note/{}", 1000 + index),
            "body": "```suggestion\nreplacement\n```",
            "system": false,
            "resolvable": true,
            "resolved": false,
            "createdAt": "2026-09-20T01:00:00Z",
            "updatedAt": "2026-09-20T02:00:00Z",
            "author": {"username": "alice", "name": "Alice"},
            "position": {
                "newPath": "src/lib.rs",
                "oldPath": "src/lib.rs",
                "newLine": 2 + index,
                "oldLine": null,
                "filePath": "src/lib.rs"
            },
            "awardEmoji": {"nodes": [], "pageInfo": {"hasNextPage": false, "endCursor": null}}
        }));
    }
    let mut comments = Vec::new();
    for index in 0..PLAIN_NOTES {
        comments.push(json!({
            "id": format!("gid://gitlab/Note/{}", 2000 + index),
            "body": format!("Discussion note {index} on the merge request."),
            "system": false,
            "resolvable": false,
            "resolved": false,
            "createdAt": "2026-09-19T01:00:00Z",
            "updatedAt": "2026-09-19T01:00:00Z",
            "author": {"username": "alice", "name": "Alice"},
            "position": null,
            "awardEmoji": {"nodes": [], "pageInfo": {"hasNextPage": false, "endCursor": null}}
        }));
    }
    let page_info = json!({"hasNextPage": false, "endCursor": null});
    json!({
        "data": {
            "project": {
                "mergeRequest": {
                    "userPermissions": {"createNote": true},
                    "discussions": {
                        "pageInfo": page_info,
                        "nodes": [
                            {
                                "id": "gid://gitlab/Discussion/1",
                                "resolvable": true,
                                "resolved": false,
                                "notes": {"pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": notes}
                            },
                            {
                                "id": "gid://gitlab/Discussion/2",
                                "resolvable": false,
                                "resolved": false,
                                "notes": {"pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": comments}
                            }
                        ]
                    }
                }
            }
        }
    })
    .to_string()
}

fn write_glab(root: &Path, url: &str, log_path: &Path) -> PathBuf {
    let path = root.join("glab");
    let script = format!(
        r#"#!/usr/bin/env python3
import json, sys, traceback, http.client
from urllib.parse import urlsplit
URL = {url:?}
LOG = {log:?}

def log():
    with open(LOG, "a", encoding="utf-8") as handle:
        handle.write(json.dumps({{"args": sys.argv[1:]}}) + "\n")

def request(method, path, body=None):
    parts = urlsplit(URL)
    conn = http.client.HTTPConnection(parts.hostname, parts.port, timeout=60)
    headers = {{"Content-Type": "application/json"}} if body is not None else {{}}
    conn.request(method, path, body=body, headers=headers)
    resp = conn.getresponse()
    payload = resp.read()
    total = resp.getheader("X-Total")
    status = resp.status
    conn.close()
    return status, total, payload

def emit(status, total, payload, include):
    if status >= 400:
        sys.stderr.buffer.write(payload)
        sys.exit(1)
    if include:
        head = "HTTP/1.1 200 OK\r\nx-total: %s\r\n\r\n" % (total or "0")
        sys.stdout.buffer.write(head.encode() + payload)
    else:
        sys.stdout.buffer.write(payload)

def main():
    try:
        run()
    except Exception as exc:
        last = traceback.format_exc().strip().splitlines()[-1]
        sys.stderr.write("%s url=%r" % (last, URL))
        sys.exit(1)

def run():
    log()
    args = sys.argv[1:]
    if args[:1] == ["--version"]:
        print("glab 1.114.0")
        return
    if args[:2] == ["auth", "status"]:
        print("git.acme.example\n  \u2713 Logged in to git.acme.example as alice")
        return
    if args[:2] == ["mr", "list"]:
        state = "opened"
        page = "1"
        index = 2
        while index < len(args):
            flag = args[index]
            if flag == "--closed":
                state = "closed"
            elif flag == "--merged":
                state = "merged"
            elif flag == "--all":
                state = "all"
            elif flag == "--page" and index + 1 < len(args):
                page = args[index + 1]
                index += 1
            elif flag in ("--per-page", "--repo", "--order", "--sort", "-F") and index + 1 < len(args):
                index += 1
            index += 1
        path = "/api/v4/projects/team%2Fsub%2Frepo/merge_requests?state=" + state + "&per_page=30&page=" + page
        status, total, payload = request("GET", path)
        emit(status, total, payload, False)
        return
    if args[:1] != ["api"]:
        sys.stderr.write("unexpected glab command\n")
        sys.exit(64)
    method = "GET"
    include = False
    path = None
    body = None
    index = 1
    while index < len(args):
        flag = args[index]
        if flag == "-i":
            include = True
        elif flag == "--method" and index + 1 < len(args):
            method = args[index + 1]
            index += 1
        elif flag in ("--hostname", "-H") and index + 1 < len(args):
            index += 1
        elif flag == "--input" and index + 1 < len(args):
            with open(args[index + 1], "rb") as handle:
                body = handle.read()
            index += 1
        elif path is None and not flag.startswith("-"):
            path = flag
        elif flag.startswith("-"):
            sys.stderr.write("unexpected flag %s\n" % flag)
            sys.exit(64)
        index += 1
    if path is None:
        sys.exit(64)
    if not path.startswith("/"):
        path = "/api/v4/" + path
    status, total, payload = request(method, path, body)
    emit(status, total, payload, include)

if __name__ == "__main__":
    main()
"#,
        url = url,
        log = log_path.display().to_string(),
    );
    executable_fixture::write_executable(&path, script);
    path
}

fn write_gh(root: &Path) -> PathBuf {
    let path = root.join("gh");
    executable_fixture::write_executable(
        &path,
        "#!/bin/sh\ncase \"$1 $2\" in\n  '--version ') echo 'gh version 2.97.0' ;;\n  'auth status') echo '{\"hosts\":{}}' ;;\n  *) exit 1 ;;\nesac\n",
    );
    path
}

async fn serve(listener: TcpListener, api: Arc<Api>, mut shutdown: oneshot::Receiver<()>) {
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                let Ok((socket, _)) = accepted else { break };
                let api = Arc::clone(&api);
                tokio::spawn(async move {
                    let _ = handle(socket, api).await;
                });
            }
        }
    }
}

async fn handle(mut socket: tokio::net::TcpStream, api: Arc<Api>) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0_u8; 8192];
    let header_end = loop {
        let read = socket.read(&mut tmp).await?;
        if read == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..read]);
        if let Some(end) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
        if buf.len() > 1_048_576 {
            return Err(std::io::Error::new(ErrorKind::InvalidData, "header"));
        }
    };
    let header = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut body = buf[header_end + 4..].to_vec();
    let length = content_length(&header);
    while body.len() < length {
        let read = socket.read(&mut tmp).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..read]);
    }
    body.truncate(length);
    let request_line = header.lines().next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("").to_owned();
    let current = api.inflight.fetch_add(1, Ordering::AcqRel) + 1;
    api.max_inflight.fetch_max(current, Ordering::AcqRel);
    let start_ms = api.started.elapsed().as_millis();
    let rtt = api.rtt_ms.load(Ordering::Acquire);
    if rtt > 0 {
        tokio::time::sleep(Duration::from_millis(rtt)).await;
    }
    let (status, total, payload) = respond(&api, &method, &path, &body);
    let mut head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        if status == 200 { "OK" } else { "Not Found" },
        payload.len(),
    );
    if let Some(total) = total {
        head.push_str(&format!("X-Total: {total}\r\n"));
    }
    head.push_str("\r\n");
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(&payload).await?;
    let end_ms = api.started.elapsed().as_millis();
    api.hits.lock().expect("hit log").push(Hit {
        method,
        path,
        status,
        start_ms,
        end_ms,
    });
    api.inflight.fetch_sub(1, Ordering::AcqRel);
    Ok(())
}

fn content_length(header: &str) -> usize {
    header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())
                .flatten()
        })
        .unwrap_or(0)
}

fn respond(api: &Api, method: &str, path: &str, body: &[u8]) -> (u16, Option<u64>, Vec<u8>) {
    let (route, query) = path.split_once('?').unwrap_or((path, ""));
    let project = "/api/v4/projects/team%2Fsub%2Frepo";
    let mr = format!("{project}/merge_requests/{MR_NUMBER}");
    let payload = if method == "GET" && route == "/api/v4/user" {
        br#"{"username":"alice","name":"Alice","id":7}"#.to_vec()
    } else if method == "GET" && route == "/api/v4/version" {
        br#"{"version":"17.9.1"}"#.to_vec()
    } else if method == "GET" && route == project {
        include_bytes!("fixtures/pull_requests/gitlab_project.json").to_vec()
    } else if method == "GET" && route == format!("{project}/merge_requests") {
        return list_response(query, &api.list_page);
    } else if method == "GET" && route == mr {
        api.detail.as_bytes().to_vec()
    } else if method == "GET" && route == format!("{mr}/approvals") {
        include_bytes!("fixtures/pull_requests/gitlab_approvals.json").to_vec()
    } else if method == "GET" && route == format!("{mr}/reviewers") {
        include_bytes!("fixtures/pull_requests/gitlab_reviewers.json").to_vec()
    } else if method == "GET" && route == format!("{mr}/approval_state") {
        br#"{"rules":[]}"#.to_vec()
    } else if method == "GET"
        && (route == format!("{mr}/award_emoji")
            || route.starts_with(&format!("{mr}/closes_issues")))
    {
        b"[]".to_vec()
    } else if method == "GET" && route.starts_with(&format!("{mr}/notes/")) {
        br#"{"suggestions":[{"id":7,"appliable":true,"applied":false,"from_line":2,"to_line":2,"from_content":"old","to_content":"new"}]}"#.to_vec()
    } else if method == "GET" && route == format!("{mr}/resource_label_events") {
        br#"[{"id":1,"user":{"username":"alice","name":"Alice"},"action":"add","label":{"name":"bug"},"created_at":"2026-09-20T04:00:00Z"}]"#.to_vec()
    } else if method == "GET" && route == format!("{mr}/resource_milestone_events") {
        br#"[{"id":2,"user":{"username":"alice","name":"Alice"},"action":"add","milestone":{"title":"Next"},"created_at":"2026-09-20T04:00:00Z"}]"#.to_vec()
    } else if method == "GET" && route == format!("{mr}/resource_state_events") {
        br#"[{"id":3,"user":{"username":"alice","name":"Alice"},"state":"opened","created_at":"2026-09-01T00:00:00Z"}]"#.to_vec()
    } else if method == "POST" && route == "/api/v4/graphql" {
        let text = String::from_utf8_lossy(body);
        if text.contains("discussions(") {
            api.timeline.as_bytes().to_vec()
        } else if text.contains("diffStatsSummary") {
            api.metadata.as_bytes().to_vec()
        } else {
            return (404, None, br#"{"error":"unknown graphql"}"#.to_vec());
        }
    } else {
        return (
            404,
            None,
            format!(r#"{{"error":"unhandled","method":"{method}","path":"{path}"}}"#).into_bytes(),
        );
    };
    (200, None, payload)
}

fn list_response(query: &str, page: &str) -> (u16, Option<u64>, Vec<u8>) {
    let state = query_param(query, "state").unwrap_or("opened");
    let per_page = query_param(query, "per_page").unwrap_or("30");
    let total = match state {
        "opened" => OPEN_MRS,
        "closed" => CLOSED_MRS,
        "merged" => MERGED_MRS,
        "all" => OPEN_MRS + CLOSED_MRS + MERGED_MRS,
        _ => return (404, None, br#"{"error":"state"}"#.to_vec()),
    };
    if per_page == "1" {
        (200, Some(total), b"[]".to_vec())
    } else if state == "opened" {
        (200, Some(total), page.as_bytes().to_vec())
    } else {
        (200, Some(total), b"[]".to_vec())
    }
}

fn query_param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then_some(value)
    })
}
