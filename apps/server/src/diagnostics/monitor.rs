use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex as SyncMutex, MutexGuard, PoisonError},
    time::Duration,
};

use thiserror::Error;
use time::OffsetDateTime;
use tokio::{
    sync::{RwLock, watch},
    task::JoinHandle,
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use super::{
    AttributedProcessSample, AttributedProcessSnapshot, ProcessResourceHistory, ProcessRow,
    ResourceSampler,
    history::{aggregate_history, trim_samples},
};

/// The sampler keeps running for this many intervals after the last read, so a client polling
/// at the interval never finds it stopped, and a server nobody reads samples nothing.
const READ_INTERVALS_KEPT_ALIVE: u32 = 3;
/// How long a read waits for the first sample when none has completed yet.
const FIRST_SAMPLE_WAIT: Duration = Duration::from_secs(1);
const NOT_SAMPLED_YET: &str = "Measuring resource usage.";

#[derive(Debug, Error)]
pub enum SamplingError {
    #[error("process diagnostics sampling failed: {0}")]
    Failed(String),
}

pub trait ProcessSampler: std::fmt::Debug + Send + Sync + 'static {
    fn sample(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ProcessRow>, SamplingError>> + Send + '_>>;
}

#[derive(Debug, Default)]
struct MonitorState {
    current: Option<Arc<AttributedProcessSnapshot>>,
    samples: VecDeque<Arc<AttributedProcessSample>>,
    last_error: Option<String>,
}

impl MonitorState {
    /// Without the fallback, a read before the first sample would show a healthy, idle host.
    fn error(&self) -> Option<String> {
        self.last_error
            .clone()
            .or_else(|| self.current.is_none().then(|| NOT_SAMPLED_YET.to_owned()))
    }
}

#[derive(Debug, Default)]
struct SamplerTask {
    last_read: Option<Instant>,
    /// `None` once the task stopped for lack of reads; a finished handle means it ended early.
    handle: Option<JoinHandle<()>>,
}

#[derive(Clone, Debug)]
pub struct CurrentProcessDiagnostics {
    pub snapshot: Option<Arc<AttributedProcessSnapshot>>,
    pub error: Option<String>,
}

trait HistoryAggregator: std::fmt::Debug + Send + Sync + 'static {
    fn aggregate(
        &self,
        retained: &[Arc<AttributedProcessSample>],
        error: Option<String>,
        read_at_ms: i128,
        window_ms: u64,
        bucket_ms: u64,
        interval: Duration,
    ) -> ProcessResourceHistory;
}

#[derive(Clone, Copy, Debug)]
struct DefaultHistoryAggregator;

impl HistoryAggregator for DefaultHistoryAggregator {
    fn aggregate(
        &self,
        retained: &[Arc<AttributedProcessSample>],
        error: Option<String>,
        read_at_ms: i128,
        window_ms: u64,
        bucket_ms: u64,
        interval: Duration,
    ) -> ProcessResourceHistory {
        aggregate_history(retained, error, read_at_ms, window_ms, bucket_ms, interval)
    }
}

/// Samples processes in one background task while clients read diagnostics. Reads return the
/// latest sample and never sample themselves, so a slow sample delays no request.
#[derive(Debug)]
pub struct DiagnosticsMonitor<S: ResourceSampler> {
    shared: Arc<Shared<S>>,
    history_aggregator: Arc<dyn HistoryAggregator>,
}

#[derive(Debug)]
struct Shared<S: ResourceSampler> {
    state: RwLock<MonitorState>,
    sampler: Arc<S>,
    interval: Duration,
    task: SyncMutex<SamplerTask>,
    /// Becomes `true` when the first sample attempt completes.
    sampled: watch::Sender<bool>,
    shutdown: CancellationToken,
}

impl<S: ResourceSampler> DiagnosticsMonitor<S> {
    pub fn new(sampler: Arc<S>, interval: Duration) -> Self {
        Self::with_aggregator(sampler, interval, Arc::new(DefaultHistoryAggregator))
    }

    fn with_aggregator(
        sampler: Arc<S>,
        interval: Duration,
        history_aggregator: Arc<dyn HistoryAggregator>,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: RwLock::new(MonitorState::default()),
                sampler,
                interval,
                task: SyncMutex::new(SamplerTask::default()),
                sampled: watch::Sender::new(false),
                shutdown: CancellationToken::new(),
            }),
            history_aggregator,
        }
    }

    pub async fn read_current(&self) -> CurrentProcessDiagnostics {
        self.shared.keep_sampling();
        self.shared.wait_for_first_sample().await;
        let state = self.shared.state.read().await;
        CurrentProcessDiagnostics {
            snapshot: state.current.clone(),
            error: state.error(),
        }
    }

    pub async fn read_history(&self, window_ms: u64, bucket_ms: u64) -> ProcessResourceHistory {
        self.shared.keep_sampling();
        self.shared.wait_for_first_sample().await;
        let now_ms = OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000;
        let (retained, error) = {
            let state = self.shared.state.read().await;
            (
                state.samples.iter().cloned().collect::<Vec<_>>(),
                state.error(),
            )
        };
        let window_ms = window_ms.max(1_000);
        let bucket_ms = bucket_ms.max(1_000);
        let interval = self.shared.interval;
        let aggregator = self.history_aggregator.clone();
        tokio::task::spawn_blocking(move || {
            aggregator.aggregate(&retained, error, now_ms, window_ms, bucket_ms, interval)
        })
        .await
        .unwrap_or_else(|error| {
            aggregate_history(
                &[],
                Some(format!("process history aggregation failed: {error}")),
                now_ms,
                window_ms,
                bucket_ms,
                interval,
            )
        })
    }

    /// Stops the sampler and waits for it; later reads return the retained sample.
    pub async fn shutdown(&self) {
        let handle = {
            let mut task = self.shared.lock_task();
            self.shared.shutdown.cancel();
            task.handle.take()
        };
        if let Some(handle) = handle
            && let Err(error) = handle.await
            && error.is_panic()
        {
            tracing::warn!("the process diagnostics sampler panicked before shutdown");
        }
    }
}

impl<S: ResourceSampler> Shared<S> {
    fn lock_task(&self) -> MutexGuard<'_, SamplerTask> {
        self.task.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records a read and starts the sampler if it is not running. The read itself never samples.
    fn keep_sampling(self: &Arc<Self>) {
        let mut task = self.lock_task();
        task.last_read = Some(Instant::now());
        if self.shutdown.is_cancelled()
            || task
                .handle
                .as_ref()
                .is_some_and(|handle| !handle.is_finished())
        {
            return;
        }
        if task.handle.is_some() {
            tracing::warn!("the process diagnostics sampler stopped unexpectedly; restarting it");
        }
        task.handle = Some(tokio::spawn(self.clone().run()));
    }

    async fn wait_for_first_sample(&self) {
        if *self.sampled.borrow() {
            return;
        }
        let mut sampled = self.sampled.subscribe();
        tokio::select! {
            _ = sampled.wait_for(|sampled| *sampled) => {}
            () = self.shutdown.cancelled() => {}
            () = tokio::time::sleep(FIRST_SAMPLE_WAIT) => {}
        }
    }

    async fn run(self: Arc<Self>) {
        loop {
            let sampled = tokio::select! {
                () = self.shutdown.cancelled() => return,
                sampled = self.sampler.sample() => sampled,
            };
            self.publish(sampled).await;
            tokio::select! {
                () = self.shutdown.cancelled() => return,
                () = tokio::time::sleep(self.interval) => {}
            }
            if self.stop_if_unread() {
                return;
            }
        }
    }

    async fn publish(&self, sampled: Result<AttributedProcessSnapshot, SamplingError>) {
        {
            let mut state = self.state.write().await;
            match sampled {
                Ok(snapshot) => {
                    let snapshot = Arc::new(snapshot);
                    state
                        .samples
                        .push_back(Arc::new(AttributedProcessSample::from(snapshot.as_ref())));
                    trim_samples(&mut state.samples, snapshot.sampled_at_ms);
                    state.current = Some(snapshot);
                    state.last_error = None;
                }
                Err(error) => {
                    let error = error.to_string();
                    if state.last_error.as_ref() != Some(&error) {
                        tracing::warn!(%error, "process diagnostics sample failed");
                    }
                    state.last_error = Some(error);
                }
            }
        }
        self.sampled.send_replace(true);
    }

    /// Decides under the task lock, so a read either sees the task running or starts a new one.
    fn stop_if_unread(&self) -> bool {
        let mut task = self.lock_task();
        let unread = task.last_read.is_none_or(|last_read| {
            last_read.elapsed() >= self.interval * READ_INTERVALS_KEPT_ALIVE
        });
        if unread {
            task.handle = None;
        }
        unread
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Barrier, Mutex as SyncMutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use tokio::sync::Notify;

    use crate::diagnostics::{
        AttributedProcessSnapshot, ProcessAttributionTotals, ProcessIdentity, ResourceSampler,
        UiCoverage,
    };

    use super::*;

    const INTERVAL: Duration = Duration::from_secs(2);
    /// Long enough for a paused-clock read to fail if it waited on a sample.
    const NO_WAIT: Duration = Duration::from_millis(10);

    #[derive(Clone, Debug)]
    enum FakeResponse {
        Snapshot(AttributedProcessSnapshot),
        Failure(&'static str),
    }

    #[derive(Debug)]
    struct FakeResourceSampler {
        calls: AtomicUsize,
        responses: SyncMutex<VecDeque<FakeResponse>>,
        block_next: AtomicBool,
        started: Notify,
        release: Notify,
    }

    impl FakeResourceSampler {
        fn new(responses: impl IntoIterator<Item = FakeResponse>) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                responses: SyncMutex::new(responses.into_iter().collect()),
                block_next: AtomicBool::new(false),
                started: Notify::new(),
                release: Notify::new(),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Acquire)
        }
    }

    impl ResourceSampler for FakeResourceSampler {
        fn sample(
            &self,
        ) -> Pin<
            Box<dyn Future<Output = Result<AttributedProcessSnapshot, SamplingError>> + Send + '_>,
        > {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::AcqRel);
                if self.block_next.swap(false, Ordering::AcqRel) {
                    self.started.notify_one();
                    self.release.notified().await;
                }
                // The last response repeats, so a background sampler never runs out.
                let response = {
                    let mut responses = self.responses.lock().expect("fake responses lock");
                    if responses.len() > 1 {
                        responses.pop_front()
                    } else {
                        responses.front().cloned()
                    }
                    .expect("fake response")
                };
                match response {
                    FakeResponse::Snapshot(snapshot) => Ok(snapshot),
                    FakeResponse::Failure(message) => {
                        Err(SamplingError::Failed(message.to_owned()))
                    }
                }
            })
        }
    }

    #[derive(Debug)]
    struct BlockingHistoryAggregator {
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
    }

    impl HistoryAggregator for BlockingHistoryAggregator {
        fn aggregate(
            &self,
            retained: &[Arc<AttributedProcessSample>],
            error: Option<String>,
            read_at_ms: i128,
            window_ms: u64,
            bucket_ms: u64,
            interval: Duration,
        ) -> ProcessResourceHistory {
            self.entered.wait();
            self.release.wait();
            aggregate_history(retained, error, read_at_ms, window_ms, bucket_ms, interval)
        }
    }

    fn snapshot(sampled_at_ms: i128) -> AttributedProcessSnapshot {
        AttributedProcessSnapshot {
            sampled_at_ms,
            server_identity: ProcessIdentity {
                pid: 10,
                started_at: 100,
            },
            native_rows: Arc::from([]),
            processes: Vec::new(),
            totals: ProcessAttributionTotals::default(),
            ui_coverage: UiCoverage::default(),
        }
    }

    fn monitor(sampler: Arc<FakeResourceSampler>) -> DiagnosticsMonitor<FakeResourceSampler> {
        DiagnosticsMonitor::new(sampler, INTERVAL)
    }

    fn sampled_at(current: &CurrentProcessDiagnostics) -> Option<i128> {
        current
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.sampled_at_ms)
    }

    /// Lets the paused clock run one interval, so the background sampler takes its next sample.
    async fn next_interval() {
        tokio::time::sleep(INTERVAL + Duration::from_millis(1)).await;
    }

    async fn wait_for_sample(monitor: &DiagnosticsMonitor<FakeResourceSampler>, at: i128) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while sampled_at(&monitor.read_current().await) != Some(at) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the background sample is published");
    }

    #[tokio::test(start_paused = true)]
    async fn construction_performs_zero_samples() {
        let sampler = Arc::new(FakeResourceSampler::new([FakeResponse::Snapshot(
            snapshot(1_000),
        )]));

        let _monitor = monitor(sampler.clone());
        tokio::time::sleep(INTERVAL * 10).await;

        assert_eq!(sampler.calls(), 0);
    }

    #[tokio::test]
    async fn first_current_read_waits_for_the_first_sample() {
        let sampler = Arc::new(FakeResourceSampler::new([FakeResponse::Snapshot(
            snapshot(1_000),
        )]));
        let monitor = monitor(sampler.clone());

        let current = monitor.read_current().await;

        assert_eq!(sampler.calls(), 1);
        assert_eq!(sampled_at(&current), Some(1_000));
        assert_eq!(current.error, None);
    }

    #[tokio::test]
    async fn concurrent_first_current_and_history_reads_share_one_sample() {
        let sampler = Arc::new(FakeResourceSampler::new([FakeResponse::Snapshot(
            snapshot(1_000),
        )]));
        sampler.block_next.store(true, Ordering::Release);
        let monitor = Arc::new(monitor(sampler.clone()));

        let current_task = tokio::spawn({
            let monitor = monitor.clone();
            async move { monitor.read_current().await }
        });
        sampler.started.notified().await;
        let history_task = tokio::spawn({
            let monitor = monitor.clone();
            async move { monitor.read_history(60_000, 1_000).await }
        });
        tokio::task::yield_now().await;

        assert_eq!(sampler.calls(), 1);
        sampler.release.notify_one();
        let current = current_task.await.expect("current task");
        let history = history_task.await.expect("history task");
        assert!(current.snapshot.is_some());
        assert_eq!(history.retained_sample_count, 1);
        assert_eq!(sampler.calls(), 1);
    }

    #[tokio::test]
    async fn reads_within_the_interval_reuse_the_latest_sample() {
        let sampler = Arc::new(FakeResourceSampler::new([FakeResponse::Snapshot(
            snapshot(1_000),
        )]));
        let monitor = monitor(sampler.clone());

        let _ = monitor.read_current().await;
        let _ = monitor.read_history(60_000, 1_000).await;
        let _ = monitor.read_current().await;

        assert_eq!(sampler.calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn each_interval_samples_again_without_a_request() {
        let sampler = Arc::new(FakeResourceSampler::new([
            FakeResponse::Snapshot(snapshot(1_000)),
            FakeResponse::Snapshot(snapshot(3_000)),
        ]));
        let monitor = monitor(sampler.clone());
        let _ = monitor.read_current().await;

        next_interval().await;

        assert_eq!(sampler.calls(), 2);
        assert_eq!(sampled_at(&monitor.read_current().await), Some(3_000));
        assert_eq!(sampler.calls(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn reads_during_a_background_sample_return_the_cached_snapshot_without_waiting() {
        let sampler = Arc::new(FakeResourceSampler::new([
            FakeResponse::Snapshot(snapshot(1_000)),
            FakeResponse::Snapshot(snapshot(3_000)),
        ]));
        let monitor = monitor(sampler.clone());
        assert_eq!(sampled_at(&monitor.read_current().await), Some(1_000));

        sampler.block_next.store(true, Ordering::Release);
        tokio::time::timeout(INTERVAL * 2, sampler.started.notified())
            .await
            .expect("the next sample starts on the interval without a request");
        for _ in 0..4 {
            let current = tokio::time::timeout(NO_WAIT, monitor.read_current())
                .await
                .expect("a current read never waits for an in-flight sample");
            assert_eq!(sampled_at(&current), Some(1_000));
            let history = tokio::time::timeout(NO_WAIT, monitor.read_history(60_000, 1_000))
                .await
                .expect("a history read never waits for an in-flight sample");
            assert_eq!(history.retained_sample_count, 1);
        }
        assert_eq!(sampler.calls(), 2);

        sampler.release.notify_one();
        wait_for_sample(&monitor, 3_000).await;
        assert_eq!(sampler.calls(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn sampling_stops_once_reads_stop_and_resumes_on_the_next_read() {
        let sampler = Arc::new(FakeResourceSampler::new([FakeResponse::Snapshot(
            snapshot(1_000),
        )]));
        let monitor = monitor(sampler.clone());
        let _ = monitor.read_current().await;

        tokio::time::sleep(INTERVAL * 10).await;
        let idle_calls = sampler.calls();
        assert!(
            idle_calls <= 4,
            "sampling continued without readers: {idle_calls} samples"
        );
        tokio::time::sleep(INTERVAL * 10).await;
        assert_eq!(sampler.calls(), idle_calls);

        let current = tokio::time::timeout(NO_WAIT, monitor.read_current())
            .await
            .expect("a read after an idle period answers from the cache");
        assert_eq!(sampled_at(&current), Some(1_000));
        tokio::time::sleep(INTERVAL / 2).await;
        assert_eq!(sampler.calls(), idle_calls + 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_first_sample_bounds_the_first_read_without_reporting_zeroes() {
        let sampler = Arc::new(FakeResourceSampler::new([FakeResponse::Snapshot(
            snapshot(1_000),
        )]));
        sampler.block_next.store(true, Ordering::Release);
        let monitor = monitor(sampler.clone());

        let current = tokio::time::timeout(Duration::from_secs(5), monitor.read_current())
            .await
            .expect("the first read is bounded");

        assert!(current.snapshot.is_none());
        assert_eq!(current.error.as_deref(), Some(NOT_SAMPLED_YET));
        sampler.release.notify_one();
        wait_for_sample(&monitor, 1_000).await;
        assert_eq!(sampler.calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_first_sample_bounds_the_first_history_read_and_says_why_it_is_empty() {
        let sampler = Arc::new(FakeResourceSampler::new([FakeResponse::Snapshot(
            snapshot(1_000),
        )]));
        sampler.block_next.store(true, Ordering::Release);
        let monitor = monitor(sampler.clone());

        let history =
            tokio::time::timeout(Duration::from_secs(5), monitor.read_history(60_000, 1_000))
                .await
                .expect("the first history read is bounded");

        assert_eq!(history.retained_sample_count, 0);
        assert_eq!(history.error.as_deref(), Some(NOT_SAMPLED_YET));
        sampler.release.notify_one();
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_stops_sampling_and_reads_keep_the_last_sample() {
        let sampler = Arc::new(FakeResourceSampler::new([FakeResponse::Snapshot(
            snapshot(1_000),
        )]));
        let monitor = monitor(sampler.clone());
        let _ = monitor.read_current().await;

        monitor.shutdown().await;
        let current = tokio::time::timeout(NO_WAIT, monitor.read_current())
            .await
            .expect("a read after shutdown answers from the cache");
        tokio::time::sleep(INTERVAL * 10).await;

        assert_eq!(sampled_at(&current), Some(1_000));
        assert_eq!(sampler.calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_does_not_wait_for_an_in_flight_sample() {
        let sampler = Arc::new(FakeResourceSampler::new([
            FakeResponse::Snapshot(snapshot(1_000)),
            FakeResponse::Snapshot(snapshot(3_000)),
        ]));
        let monitor = monitor(sampler.clone());
        let _ = monitor.read_current().await;
        sampler.block_next.store(true, Ordering::Release);
        tokio::time::timeout(INTERVAL * 2, sampler.started.notified())
            .await
            .expect("the next sample starts on the interval");

        tokio::time::timeout(NO_WAIT, monitor.shutdown())
            .await
            .expect("shutdown abandons the in-flight sample");
        sampler.release.notify_one();
        tokio::time::sleep(INTERVAL * 10).await;

        assert_eq!(sampled_at(&monitor.read_current().await), Some(1_000));
        assert_eq!(sampler.calls(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn reads_after_shutdown_never_start_the_sampler() {
        let sampler = Arc::new(FakeResourceSampler::new([FakeResponse::Snapshot(
            snapshot(1_000),
        )]));
        let monitor = monitor(sampler.clone());

        monitor.shutdown().await;
        let current = tokio::time::timeout(NO_WAIT, monitor.read_current())
            .await
            .expect("a read after shutdown does not wait for a first sample");
        tokio::time::sleep(INTERVAL * 10).await;

        assert!(current.snapshot.is_none());
        assert_eq!(current.error.as_deref(), Some(NOT_SAMPLED_YET));
        assert_eq!(sampler.calls(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn failed_refresh_retains_the_last_good_snapshot_and_history() {
        let sampler = Arc::new(FakeResourceSampler::new([
            FakeResponse::Snapshot(snapshot(1_000)),
            FakeResponse::Failure("refresh failed"),
        ]));
        let monitor = monitor(sampler.clone());
        let first = monitor.read_current().await;
        let retained_before = monitor.shared.state.read().await.samples.clone();

        next_interval().await;
        let failed = monitor.read_current().await;
        let retained_after = monitor.shared.state.read().await.samples.clone();

        assert_eq!(sampler.calls(), 2);
        assert_eq!(sampled_at(&failed), sampled_at(&first));
        assert_eq!(
            failed.error.as_deref(),
            Some("process diagnostics sampling failed: refresh failed")
        );
        assert_eq!(retained_after, retained_before);
    }

    #[tokio::test]
    async fn cached_current_read_does_not_wait_for_history_aggregation() {
        let sampler = Arc::new(FakeResourceSampler::new([FakeResponse::Snapshot(
            snapshot(1_000),
        )]));
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let monitor = Arc::new(DiagnosticsMonitor::with_aggregator(
            sampler.clone(),
            INTERVAL,
            Arc::new(BlockingHistoryAggregator {
                entered: entered.clone(),
                release: release.clone(),
            }),
        ));
        let current = monitor.read_current().await;
        assert!(current.snapshot.is_some());

        let history_task = tokio::spawn({
            let monitor = monitor.clone();
            async move { monitor.read_history(60_000, 1_000).await }
        });
        tokio::task::spawn_blocking(move || entered.wait())
            .await
            .expect("aggregation entry waiter");

        let cached = tokio::time::timeout(Duration::from_millis(100), monitor.read_current())
            .await
            .expect("cached current read should not wait for history aggregation");
        assert!(cached.snapshot.is_some());
        assert_eq!(sampler.calls(), 1);

        tokio::task::spawn_blocking(move || release.wait())
            .await
            .expect("aggregation release waiter");
        let _ = history_task.await.expect("history task");
    }

    #[tokio::test]
    async fn first_read_failure_returns_no_snapshot() {
        let sampler = Arc::new(FakeResourceSampler::new([FakeResponse::Failure(
            "initial failure",
        )]));
        let monitor = monitor(sampler.clone());

        let current = monitor.read_current().await;

        assert_eq!(sampler.calls(), 1);
        assert!(current.snapshot.is_none());
        assert_eq!(
            current.error.as_deref(),
            Some("process diagnostics sampling failed: initial failure")
        );
    }

    #[tokio::test]
    async fn first_history_read_failure_returns_no_measurement_buckets() {
        let sampler = Arc::new(FakeResourceSampler::new([FakeResponse::Failure(
            "initial failure",
        )]));
        let monitor = monitor(sampler.clone());

        let history = monitor.read_history(60_000, 1_000).await;

        assert_eq!(sampler.calls(), 1);
        assert_eq!(history.retained_sample_count, 0);
        assert!(history.buckets.is_empty());
        assert!(history.processes.is_empty());
        assert_eq!(
            history.error.as_deref(),
            Some("process diagnostics sampling failed: initial failure")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn history_read_failure_retains_prior_successful_measurements() {
        let sampler = Arc::new(FakeResourceSampler::new([
            FakeResponse::Snapshot(snapshot(1_000)),
            FakeResponse::Failure("refresh failed"),
        ]));
        let monitor = monitor(sampler.clone());
        let first = monitor.read_history(60_000, 1_000).await;

        next_interval().await;
        let stale = monitor.read_history(60_000, 1_000).await;

        assert_eq!(sampler.calls(), 2);
        assert_eq!(first.retained_sample_count, 1);
        assert_eq!(stale.retained_sample_count, 1);
        assert!(!stale.buckets.is_empty());
        assert_eq!(
            stale.error.as_deref(),
            Some("process diagnostics sampling failed: refresh failed")
        );
    }
}
