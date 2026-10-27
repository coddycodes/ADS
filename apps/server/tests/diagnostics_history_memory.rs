//! The process-diagnostics history's memory once it holds its full hour (#100): the status
//! bar keeps the sampler running, so this is what an idle desktop carries for as long as it
//! runs. Its own binary, since it counts every allocation the process makes.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicIsize, AtomicU64, Ordering},
    },
    time::Duration,
};

use coddy_studio_server::diagnostics::{
    AttributedProcess, AttributedProcessSnapshot, AttributionConfidence, AttributionKind,
    AttributionScope, DiagnosticsMonitor, ProcessAttributionTotals, ProcessIdentity, ProcessRow,
    ResourceSampler, SamplingError, UiCoverage, UiCoverageStatus,
};

struct Counting;

static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);

// SAFETY: forwards to the system allocator unchanged and only counts sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE_BYTES.fetch_add(layout.size() as isize, Ordering::Relaxed);
        // SAFETY: the caller's layout contract carries over.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size() as isize, Ordering::Relaxed);
        // SAFETY: `ptr` came from `alloc` with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE_BYTES.fetch_add(
            new_size as isize - layout.size() as isize,
            Ordering::Relaxed,
        );
        // SAFETY: the caller's contract carries over.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const INTERVAL_MS: u64 = 2_000;
const SAMPLES_PER_HOUR: u64 = 3_600_000 / INTERVAL_MS;

/// An idle macOS desktop: the app (with the server in it) and its WebKit processes. Each
/// sample is built afresh, as the native sampler builds it.
#[derive(Debug, Default)]
struct IdleDesktop {
    taken: AtomicU64,
}

const PROCESSES: [(u32, &str); 5] = [
    (
        100,
        "/Applications/Coddy Studio.app/Contents/MacOS/coddy-studio-desktop",
    ),
    (
        101,
        "/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent",
    ),
    (
        102,
        "/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.Networking.xpc/Contents/MacOS/com.apple.WebKit.Networking",
    ),
    (
        103,
        "/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.GPU.xpc/Contents/MacOS/com.apple.WebKit.GPU",
    ),
    (
        104,
        "/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent",
    ),
];

impl ResourceSampler for IdleDesktop {
    fn sample(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<AttributedProcessSnapshot, SamplingError>> + Send + '_>>
    {
        let taken = self.taken.fetch_add(1, Ordering::SeqCst);
        let sampled_at_ms = 1_800_000_000_000 + i128::from(taken * INTERVAL_MS);
        let identity = |pid: u32| ProcessIdentity {
            pid,
            started_at: 1_800_000_000,
        };
        let rows = PROCESSES
            .iter()
            .map(|(pid, command)| ProcessRow {
                pid: *pid,
                started_at: 1_800_000_000,
                ppid: if *pid == 100 { 1 } else { 100 },
                pgid: None,
                status: "S".to_owned(),
                cpu_percent: 0.1,
                cpu_core_percent: None,
                rss_bytes: 50 << 20,
                elapsed: "01:00:00".to_owned(),
                command: (*command).to_owned(),
            })
            .collect::<Vec<_>>();
        let processes = PROCESSES
            .iter()
            .map(|(pid, _)| AttributedProcess {
                identity: identity(*pid),
                process_key: identity(*pid).key(),
                scope: AttributionScope::Core,
                kind: if *pid == 100 {
                    AttributionKind::Server
                } else {
                    AttributionKind::Ui
                },
                label: if *pid == 100 {
                    "Coddy Studio server".to_owned()
                } else {
                    "Coddy Studio UI".to_owned()
                },
                confidence: AttributionConfidence::Exact,
                cpu_percent: 0.1,
                rss_bytes: 50 << 20,
            })
            .collect();
        Box::pin(async move {
            Ok(AttributedProcessSnapshot {
                sampled_at_ms,
                server_identity: identity(100),
                native_rows: rows.into(),
                processes,
                totals: ProcessAttributionTotals::default(),
                ui_coverage: UiCoverage {
                    status: UiCoverageStatus::Available,
                    message: None,
                },
            })
        })
    }
}

/// Lets the monitor sample until `count` samples were taken, reading as the status bar does
/// so the sampler keeps running.
async fn sample_until(
    monitor: &DiagnosticsMonitor<IdleDesktop>,
    sampler: &IdleDesktop,
    count: u64,
) {
    while sampler.taken.load(Ordering::SeqCst) < count {
        let _ = monitor.read_current().await;
        tokio::task::yield_now().await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn an_hour_of_process_history_stays_small() {
    let sampler = Arc::new(IdleDesktop::default());
    let monitor = DiagnosticsMonitor::new(sampler.clone(), Duration::from_micros(1));
    sample_until(&monitor, &sampler, 10).await;
    let before = LIVE_BYTES.load(Ordering::Relaxed);
    sample_until(&monitor, &sampler, SAMPLES_PER_HOUR + 10).await;
    let after_one_hour = LIVE_BYTES.load(Ordering::Relaxed);
    sample_until(&monitor, &sampler, 2 * SAMPLES_PER_HOUR + 10).await;
    let after_two_hours = LIVE_BYTES.load(Ordering::Relaxed);
    monitor.shutdown().await;

    let hour = after_one_hour - before;
    eprintln!(
        "history: {hour} bytes for an hour ({} per sample), {} more in the second hour",
        hour / SAMPLES_PER_HOUR as isize,
        after_two_hours - after_one_hour,
    );
    assert!(
        (after_two_hours - after_one_hour).abs() < 64 * 1024,
        "the history stops growing once it holds its hour"
    );
    assert!(
        hour < 1024 * 1024,
        "an hour of history holds {hour} bytes, over 1 MiB"
    );
}
