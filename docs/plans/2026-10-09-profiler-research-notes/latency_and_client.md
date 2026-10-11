# Adaptive Slow-RPC Detection and Client-Side Responsiveness Measurement (WebSocket JSON-RPC + Tauri 2 web engines)

Scope: (1) per-method adaptive "this RPC was abnormally slow" detection on the Rust server, (2) human-perception thresholds to anchor absolute floors, (3) which browser performance APIs exist in each Tauri 2 engine (WebKitGTK on Linux, WKWebView on macOS, WebView2 on Windows) and in regular browsers, (4) end-to-end client-perceived latency attribution, (5) observer overhead.

Primary compat data was pulled directly from `@mdn/browser-compat-data` v8.1.5 (built 2026-10-08) via unpkg (https://unpkg.com/@mdn/browser-compat-data/data.json, repo https://github.com/mdn/browser-compat-data), and WebKit feature defaults were read from `Source/WTF/Scripts/Preferences/UnifiedWebPreferences.yaml` at the `webkitgtk-2.48.0`, `2.50.0`, `2.52.0`, `2.54.0` tags of https://github.com/WebKit/WebKit.

Repo note: `Cargo.lock` in this worktree contains none of `hdrhistogram`, `tdigest`, `sketches-ddsketch`, `metrics`, `metrics-util`, `quanta` (grep returned nothing), so any sketch crate would be a new dependency. The server already has a `apps/server/src/diagnostics/` module (monitor, history, trace, attribution) that the design should extend rather than duplicate.

---

## 1. Per-endpoint adaptive thresholds: algorithm and concrete parameters

### Takeaway

Production practice baselines latency on _percentiles of a distribution_, not on mean+k·σ, and Google's own tail-latency work uses "longer than the p95 expected for this class of request" as the trigger for corrective action. Recommended rule: per `(method, outcome)` keep a windowed log-bucketed histogram; once ≥100 samples exist, flag a call when `duration > max(3 × p95_window, floor_ms)` (floor 250 ms for interactive methods), plus an always-on absolute tier at 1 s / 10 s that does not adapt; evaluate in-flight, not only on completion.

### Cited Findings

- SRE book: collect "request counts bucketed by latencies (suitable for rendering a histogram), rather than actual latencies"; example buckets "between 0 ms and 10 ms, between 10 ms and 30 ms", "Distributing the histogram boundaries approximately exponentially (in this case by factors of roughly 3)" — [Google SRE Book, Monitoring Distributed Systems](https://sre.google/sre-book/monitoring-distributed-systems/)
- SRE book on means: "If you run a web service with an average latency of 100 ms … 1% of requests might easily take 5 seconds"; footnote: "the idea that most of your requests are near the mean is just hopeful thinking." — [SRE Book](https://sre.google/sre-book/monitoring-distributed-systems/)
- SRE book: "it's important to distinguish between the latency of successful requests and the latency of failed requests … a slow error is even worse than a fast error!" → track error latency separately rather than filtering errors out — [SRE Book](https://sre.google/sre-book/monitoring-distributed-systems/)
- SRE book: "Latency increases are often a leading indicator of saturation"; "Measuring your 99th percentile response time over some small window (e.g., one minute) can give a very early signal of saturation." — [SRE Book](https://sre.google/sre-book/monitoring-distributed-systems/)
- The Tail at Scale (primary PDF): hedged request is deferred "until the first request has been outstanding for more than the 95th-percentile expected latency for this class of requests. This approach limits the additional load to approximately 5% while substantially shortening the latency tail." — [Dean & Barroso, CACM 2013 (PDF)](https://www.barroso.org/publications/TheTailAtScale.pdf)
- The Tail at Scale: in a BigTable benchmark reading 1,000 keys across 100 servers, "sending a hedging request after a 10ms delay reduces the 99.9th-percentile latency for retrieving all 1,000 values from 1,800ms to 74ms while sending just 2% more requests." (Note: the 1,800→74 ms figure is p99.9 with a 10 ms trigger; p95 is the general trigger heuristic, not a measured result.) — [Tail at Scale PDF](https://www.barroso.org/publications/TheTailAtScale.pdf)
- The Tail at Scale: fan-out amplification — if each server is slow 1% of the time and a request touches 100 servers, "63% of user requests will take more than one second" — [Tail at Scale PDF](https://www.barroso.org/publications/TheTailAtScale.pdf)
- The Tail at Scale: "Latency-induced probation … intermediate servers sometimes detect situations where the system performs better by excluding a particularly slow machine … The source of the slowness is frequently temporary phenomena like interference from unrelated networking traffic or a spike in CPU activity" — [Tail at Scale PDF](https://www.barroso.org/publications/TheTailAtScale.pdf)
- DDSketch paper, Figure 2 caption: "The average latency of a web endpoint over time … is closer to the 75th percentile than it is to the 50th" (latency is skewed; mean/σ are poor baselines) — [Masson, Rim, Lee, PVLDB 12(12) 2019](https://www.vldb.org/pvldb/vol12/p2195-masson.pdf)
- DDSketch: rank-error sketches fail on heavy tails — with 0.005 rank accuracy, a p99 query can return anything between p98.5 and p99.5, "anywhere from 2 to 20 seconds" in their example; they define α-accurate quantiles as `|x̃q − xq| ≤ α·xq` (relative error) and claim "the first fully-mergeable, relative-error quantile sketching algorithm with formal guarantees" used "by Datadog at a wide-scale". In their Pareto experiment, ~900 bins sufficed for n=10^10 with a 2048-bin limit — [DDSketch PVLDB](https://www.vldb.org/pvldb/vol12/p2195-masson.pdf)
- t-digest: accuracy is "relative to max(q, 1-q)" (best at the tails), summaries merge "with no loss in accuracy", small size — [Dunning, Computing Extremely Accurate Quantiles Using t-Digests, arXiv:1902.04023](https://arxiv.org/abs/1902.04023)
- hdrhistogram crate (v7.6.0): "The memory footprint is fixed regardless of the number of data value samples recorded" and depends only on range and significant-figure precision; recording is constant time (3–6 ns on circa-2014 Intel cited); `value_at_quantile(0.999)`; `new()` auto-resizes, `new_with_max()` errors above bound; `sync` module/`SyncHistogram` for multi-thread recording; serialization module; no decaying/windowed histogram is provided — [docs.rs hdrhistogram](https://docs.rs/hdrhistogram/latest/hdrhistogram/)
- Prometheus: classic `histogram_quantile()` linearly interpolates assuming even spread within a bucket; "Error is limited by the width of the bucket the quantile is located in" — e.g. true p95 ≈220 ms in a 200–300 ms bucket estimates as 295 ms, native histogram ≈228 ms; summaries' precomputed quantiles cannot be meaningfully aggregated, histograms can — [Prometheus: Histograms and summaries](https://prometheus.io/docs/practices/histograms/)
- Datadog anomaly "basic" algorithm "uses a lagging rolling quantile" to set expected values, needs little data, adapts quickly, ignores seasonality; "agile" (robust SARIMA) and "robust" (seasonal-trend decomposition) need ≥3× the seasonality period of history (e.g. 3 weeks for weekly). The "3 standard deviations" wording appears only in an example query ("three standard deviations above the ordinary value"); the `bounds` unit is not formally defined on the page — [Datadog Anomaly Monitor docs](https://docs.datadoghq.com/monitors/types/anomaly/)
- Envoy outlier detection is about _success rate_, not latency (the arch overview describes no latency-based ejection). Defaults: `success_rate_stdev_factor` 1900 (=1.9), threshold "mean - (stdev * success_rate_stdev_factor)", `success_rate_minimum_hosts` 5, `success_rate_request_volume` 100, `consecutive_5xx` 5, `interval` 10 s, `base_ejection_time` 30 s, `max_ejection_percent` 10 — [Envoy OutlierDetection proto](https://www.envoyproxy.io/docs/envoy/latest/api-v3/config/cluster/v3/outlier_detection.proto); [Envoy outlier arch overview](https://www.envoyproxy.io/docs/envoy/latest/intro/arch_overview/upstream/outlier)

### Inferences

- **Choose a percentile baseline, not EWMA mean + k·σ.** SRE book and DDSketch Fig. 2 both show latency is right-skewed, so mean+kσ is inflated by the very outliers you want to detect and is not robust; Datadog's "basic" detector is also quantile-based. EWMA is fine only as a cheap secondary signal for drift.
- **Sketch choice for a single-process Rust server:** an hdrhistogram with 2 significant figures (≈1% relative error, comparable to DDSketch α=0.01) over 1 µs–1 h is the simplest fit: constant-time record, fixed memory, mature crate. DDSketch/t-digest's main advantage — cross-node merging — is irrelevant for one server process; they are equally valid if a crate is preferred. Do not use a rank-error sketch (GK) for tails (DDSketch's argument). A hand-rolled fixed log-bucket array (e.g. ~factor-1.25 buckets, ~100 u32 counters per method) is also sufficient for a p95 baseline and avoids a dependency — the SRE book's own example uses ×3 buckets, so precision needs are modest when the multiplier is 3.
- **Windowing (hdrhistogram has no decay):** use a ring of N interval histograms (e.g. 6 × 5 min = 30 min window) and compute p95 over their sum (`Histogram::add`), or a two-slot current/previous swap. This forgets old regimes (e.g. after a code change) while keeping enough samples. A "lagging" baseline, like Datadog basic, should exclude the current interval so a burst cannot raise its own threshold.
- **Recommended rule (all numbers are design choices justified below, not values from a single source):**
  - Key = `(method, outcome∈{ok,error,cancelled})` per SRE's separate-error-latency guidance; streaming/subscription methods excluded or keyed on time-to-first-item.
  - Warm-up: relative tier active only after **≥100 samples** in the window (Envoy's analogous volume gate is 100; at 20 samples the p95 is literally the 19th/20th value so it is noise; 50 is a defensible minimum if methods are rare).
  - Relative tier: `slow if duration > max(3 × p95, floor)`. Multiplier 3 aligns with the SRE ×3 bucket spacing and ensures only ~a few-in-a-thousand calls on a stable distribution trip it; p95 (not p99) because p99 needs ~10× more samples to stabilize and Tail at Scale uses p95 as its "abnormal for this class" trigger.
  - Floors: **250 ms** for interactive methods (above RAIL's 100 ms "immediate" and INP's 200 ms "good"; below which the user cannot perceive a problem so flagging is noise), **1 s** for background/batch methods.
  - Absolute tier (non-adaptive, always on, also during warm-up): warn at **1 s** (Nielsen flow limit) and severe at **10 s** (Nielsen attention limit). This prevents a chronically slow method from "normalizing" its own baseline.
  - In-flight detection: check outstanding requests periodically (e.g. on the existing diagnostics tick) and flag when `elapsed > threshold` before completion — the same mechanism as hedging in Tail at Scale; this catches hangs that never complete.
  - Exclude flagged samples from the baseline? No — exclusion biases p95 downward over time; the 3× multiplier already provides robustness. Instead cap per-window influence by the window ring.
  - Rate-limit reporting (e.g. one event per method per interval with a count) to avoid storms during saturation, when SRE says latency rises globally.
- **Global saturation vs per-method anomaly:** if many methods simultaneously exceed their thresholds, it is saturation (SRE) — report one aggregate event rather than N per-method events.

### Gaps

- No primary source gives a canonical "k × p95 after N samples" formula; the specific constants (3×, 100 samples, 250 ms floor) are engineering choices derived from the sources above, not quoted values.
- hdrhistogram exact memory for a given range/sigfig was not retrieved (docs only say fixed and range/precision-dependent); measure with `Histogram::len()`/size at implementation time.
- Datadog's internal parameterization of "basic" (window length, quantile used) is not documented publicly.
- Envoy has no latency outlier detection; its stdev rule is not precedent for latency, only for volume gating.

---

## 2. Human-perception thresholds

### Takeaway

Use 100 ms as "instant", 200 ms as the INP "good" ceiling, 500 ms as INP "poor", 1 s as flow-break and 10 s as attention-loss; main-thread work over 50 ms is a "long task"/"long animation frame"; 16 ms is a 60 Hz frame budget.

### Cited Findings

- Nielsen: "0.1 second is about the limit for having the user feel that the system is reacting instantaneously"; "1.0 second is about the limit for the user's flow of thought to stay uninterrupted"; "10 seconds is about the limit for keeping the user's attention focused"; percent-done indicators for operations over ~10 s; busy cursor/small counter for 2–10 s — [NN/g, Response Times: The 3 Important Limits](https://www.nngroup.com/articles/response-times-3-important-limits/)
- RAIL: Response — complete a transition within 100 ms, process input events within 50 ms; Animation — produce each frame in ≤10 ms (hard budget 16 ms); Idle — work in ≤50 ms chunks; Load — interactive in ≤5 s on mid-range mobile/slow 3G. Perception bands: 0–16 ms smooth motion, 0–100 ms immediate, 100–1000 ms continuous flow, ≥1000 ms loses focus, ≥10000 ms frustration. The page (last updated 2020-06-10) says Core Web Vitals is now "the recommended approach … over RAIL" — [web.dev RAIL](https://web.dev/articles/rail)
- INP: good ≤200 ms, needs improvement 200–500 ms, poor >500 ms at the 75th percentile of page loads; INP is the longest interaction ignoring one highest per 50 interactions; latency = input delay + processing duration + presentation delay (until next frame painted) — [web.dev INP](https://web.dev/articles/inp)
- Event Timing default `durationThreshold` is 104 ms: "user input that is not handled within 100ms is considered slow and 104ms is the first multiple of 8 greater than 100ms"; minimum threshold 16 ms; durations rounded to nearest 8 ms — [MDN PerformanceEventTiming](https://developer.mozilla.org/en-US/docs/Web/API/PerformanceEventTiming)
- Long animation frame: "a rendering update is delayed beyond 50 milliseconds"; `blockingDuration` = sum of the portion of each >50 ms task above 50 ms (plus render time on the longest) — [Chrome for Developers, LoAF](https://developer.chrome.com/docs/web-platform/long-animation-frames); [MDN PerformanceLongAnimationFrameTiming](https://developer.mozilla.org/en-US/docs/Web/API/PerformanceLongAnimationFrameTiming)
- Tail at Scale abstract: systems responding within about 100 ms "feel fluid" — [Google Research](https://research.google/pubs/the-tail-at-scale/)

### Inferences

- Map tiers to user-facing meaning: RPC round trip + render ≤100 ms "instant"; ≤200 ms acceptable (INP good); >500 ms clearly poor; >1 s needs a busy indicator; >10 s needs progress + cancel. These are the right absolute floors for the server and client tiers in section 1.
- Main-thread stall detection on the client should use 50 ms as the "long" threshold to match Long Tasks/LoAF semantics, so fallback measurements are comparable to native API data.

### Gaps

- None material; RAIL is explicitly superseded by Core Web Vitals but its perception bands remain cited.

---

## 3. Browser performance API availability matrix (2026)

### Takeaway

Only Chromium (Chrome, Edge, and Windows WebView2) has Long Tasks, LoAF, `performance.memory` and `measureUserAgentSpecificMemory`. Event Timing/INP is now cross-engine (Safari 26.2, Firefox 144 for `interactionId`) and is default-on in WebKitGTK ≥2.52, but **not** in WebKitGTK ≤2.50 or macOS WKWebView older than the Safari 26.2 WebKit. Paint timing, User Timing (`mark`/`measure`), `PerformanceObserver`, `supportedEntryTypes`, `timeOrigin` and `requestAnimationFrame` are universal. Always feature-detect with `PerformanceObserver.supportedEntryTypes`.

### Cited Findings

Matrix from MDN BCD v8.1.5 (2026-10-08) — [browser-compat-data](https://github.com/mdn/browser-compat-data) (values = first version supporting; ✗ = not supported):

| API                                                    | Chrome / Edge / WebView2 | Firefox   | Safari (macOS)          | WebKitGTK (from WebKit prefs, see below) |
| ------------------------------------------------------ | ------------------------ | --------- | ----------------------- | ---------------------------------------- |
| `PerformanceObserver` / `observe`                      | 52                       | 57        | 11                      | yes                                      |
| `PerformanceObserver.supportedEntryTypes`              | 73                       | 68        | 13                      | yes                                      |
| `performance.mark` / `measure` (User Timing)           | 28                       | 38        | 11                      | yes                                      |
| `performance.timeOrigin`                               | 62                       | 53        | 15                      | yes                                      |
| `'paint'` (FP/FCP, `PerformancePaintTiming`)           | 60                       | 84        | 14.1                    | yes                                      |
| `'longtask'` (`PerformanceLongTaskTiming`)             | 58                       | ✗         | ✗                       | ✗                                        |
| `'long-animation-frame'` (LoAF)                        | 123                      | ✗         | ✗                       | ✗                                        |
| `'event'` / `'first-input'` (`PerformanceEventTiming`) | 76                       | 89        | 26.2                    | 2.52+ default-on                         |
| `PerformanceEventTiming.interactionId` (INP)           | 96                       | 144       | 26.2                    | 2.52+                                    |
| `EventCounts`                                          | 85                       | 89        | 26.2                    | —                                        |
| `'largest-contentful-paint'`                           | 77                       | 122       | 26.2                    | 2.52+ default-on                         |
| `'element'` (Element Timing)                           | 77                       | ✗         | ✗                       | ✗                                        |
| `performance.memory` (non-standard)                    | 7                        | ✗         | ✗                       | ✗                                        |
| `performance.measureUserAgentSpecificMemory()`         | 89                       | ✗         | ✗                       | ✗                                        |
| `requestIdleCallback`                                  | 47                       | 55        | ✗ (preview behind flag) | ✗ (pref `testable`, default false)       |
| `scheduler` / `scheduler.yield()`                      | 94 / 129                 | 142 / 142 | ✗                       | ✗                                        |

- LoAF: shipped Chrome 123 (origin trial 116–122), Edge 123; "Firefox and Safari do not support it"; Long Tasks since Chrome 58; Long Tasks attribution only names the container frame, LoAF attributes scripts (>5 ms) with `invoker`, `sourceURL`, plus `renderStart`, `styleAndLayoutStart`, `firstUIEventTimestamp`, `blockingDuration`; buffer size 200 for both `long-animation-frame` and `longtask`, so use a `PerformanceObserver` rather than `getEntriesByType` — [Chrome for Developers, LoAF](https://developer.chrome.com/docs/web-platform/long-animation-frames)
- Firefox shipped `PerformanceEventTiming.interactionId` in Firefox 144 (Oct 2025), enabling INP — [Mozilla Performance blog](https://blog.mozilla.org/performance/2025/10/15/firefox-144-ships-interactionid-for-inp/)
- Safari 26.2 (released Dec 12, 2025) added Event Timing and LCP as part of Interop 2025; MDN marks INP-related APIs Baseline 2025 newly available — [Search Engine Journal (secondary)](https://www.searchenginejournal.com/apple-safari-update-enables-tracking-two-core-web-vitals-metrics/563465/); confirmed by BCD `safari: 26.2` for `PerformanceEventTiming` and `LargestContentfulPaint` — [BCD](https://github.com/mdn/browser-compat-data)
- WebKit `EventTimingEnabled` at tag `webkitgtk-2.50.0`: `status: testable`, defaults `WebKitLegacy: false, WebKit: false, WebCore: false` — [UnifiedWebPreferences.yaml @ webkitgtk-2.50.0](https://raw.githubusercontent.com/WebKit/WebKit/webkitgtk-2.50.0/Source/WTF/Scripts/Preferences/UnifiedWebPreferences.yaml)
- At `webkitgtk-2.52.0` and `webkitgtk-2.54.0`: `EventTimingEnabled` `status: stable`, `WebKitLegacy: false`, **`WebKit: true`**, `WebCore: true` (WebKitGTK and WKWebView use the modern `WebKit` layer, not WebKitLegacy); `LargestContentfulPaintEnabled` `status: stable`, default `true`. At `webkitgtk-2.48.0` neither flag exists; at 2.50 LCP flag absent — [UnifiedWebPreferences.yaml @ webkitgtk-2.52.0](https://raw.githubusercontent.com/WebKit/WebKit/webkitgtk-2.52.0/Source/WTF/Scripts/Preferences/UnifiedWebPreferences.yaml); [@ webkitgtk-2.54.0](https://raw.githubusercontent.com/WebKit/WebKit/webkitgtk-2.54.0/Source/WTF/Scripts/Preferences/UnifiedWebPreferences.yaml); [@ main](https://raw.githubusercontent.com/WebKit/WebKit/main/Source/WTF/Scripts/Preferences/UnifiedWebPreferences.yaml)
- WebKit `RequestIdleCallbackEnabled` on main: `status: testable`, `defaultValue: false` — [UnifiedWebPreferences.yaml @ main](https://raw.githubusercontent.com/WebKit/WebKit/main/Source/WTF/Scripts/Preferences/UnifiedWebPreferences.yaml)
- WebKit has no Long Tasks / LoAF preference at all in the prefs file (grep for `LongTask`, `LongAnimationFrame` found nothing), consistent with BCD ✗ — [UnifiedWebPreferences.yaml @ main](https://raw.githubusercontent.com/WebKit/WebKit/main/Source/WTF/Scripts/Preferences/UnifiedWebPreferences.yaml)
- WebView2 Evergreen runtime "updates automatically" and "receives the same Microsoft Edge updates"; the runtime "generally provides the same web platform capabilities and update release cadence as the Stable channel of the Microsoft Edge browser"; Evergreen is included in Windows 11; updates take effect only on app restart; admins can disable updates, so feature-detect; Fixed Version is pinned and >250 MB — [Microsoft Learn, WebView2 distribution](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/distribution)
- `measureUserAgentSpecificMemory`: "your document must be in a secure context and cross-origin isolated"; otherwise throws `SecurityError` — [MDN Performance.measureUserAgentSpecificMemory](https://developer.mozilla.org/en-US/docs/Web/API/Performance/measureUserAgentSpecificMemory)
- `performance.now()` resolution: MDN states 5 µs in cross-origin-isolated contexts and 100 µs otherwise (this matches Chromium) — [MDN performance.now()](https://developer.mozilla.org/en-US/docs/Web/API/Performance/now); **conflicts with** BCD's Firefox notes ("In Firefox 60 the accuracy was increased to 1 millisecond", after 20 µs in 57.0.4 and 2 ms in 59) — [BCD](https://github.com/mdn/browser-compat-data). A WebKit developer wrote in 2021 that WebKit had already reduced high-resolution time to 100 µs before Spectre and that "we can't allow 100μs precision timing measurements" to mitigate Spectre, implying coarser (likely ~1 ms) in non-isolated contexts, but not stating a number — [webkit-dev, March 2021](https://lists.webkit.org/pipermail/webkit-dev/2021-March/031743.html)
- Spec requires `performance.now()` to keep ticking across OS sleep, but "only browsers on Windows keep ticking during sleep" — [MDN performance.now()](https://developer.mozilla.org/en-US/docs/Web/API/Performance/now)

### Inferences

- **Tauri targets:**
  - _Windows (WebView2 Evergreen)_ = current Chromium column: everything available, including LoAF with script attribution and `performance.memory`. `measureUserAgentSpecificMemory` only if the app page is cross-origin isolated (COOP/COEP), which is unlikely for a Tauri custom-protocol page; treat as unavailable.
  - _macOS (WKWebView)_: BCD's `webview_ios` column is iOS, not macOS; the inference (not verified by a fetched source) is that macOS WKWebView uses the system WebKit framework, which tracks the installed Safari version. Users on a WebKit older than Safari 26.2 have **no Event Timing, no LCP**; all macOS versions lack Long Tasks, LoAF, `performance.memory`, rIC, `scheduler`. Paint timing (14.1+), User Timing, `timeOrigin` (15+) and rAF are available.
  - _Linux (WebKitGTK via webkit2gtk-4.1)_: version is whatever the distro ships. ≤2.50: no Event Timing, no LCP. ≥2.52: Event Timing + LCP on by default (engine default; not runtime-verified inside wry/Tauri). Never Long Tasks/LoAF/memory/rIC.
  - _Regular browsers_: Chrome/Edge full; Firefox has Event Timing (+`interactionId` 144), paint, LCP (122), `scheduler.yield` (142) but no Long Tasks/LoAF/memory; Safari like macOS WKWebView.
- **Fallbacks** when `longtask`/`long-animation-frame` are absent (all WebKit, all Firefox): (a) rAF frame-gap monitor — record `now - lastFrameTs`; gaps >50 ms ≈ long animation frame, >16.7 ms ≈ dropped frame; (b) a `setTimeout`/`MessageChannel` heartbeat measuring scheduling drift (>50 ms late ≈ main-thread block) that also works when no frames are being produced. Both must be suspended when `document.visibilityState === 'hidden'` because rAF stops and timers are throttled in background, otherwise they report false stalls. When `event` is absent (old WebKit): wrap the app's own interaction handlers — `performance.now()` at input, double-rAF after the resulting update — to approximate interaction latency.
- **Memory fallback**: on non-Chromium engines, no JS heap API exists; sample webview process RSS natively from the Rust side instead (the server already has a resource sampler).
- Detection code: `const types = PerformanceObserver.supportedEntryTypes ?? []` and gate each observer; report the capability set alongside measurements so data from different engines is not compared as if equivalent.

### Gaps

- No primary Apple doc fetched that states macOS WKWebView = installed Safari's WebKit; treated as inference.
- Did not verify which WebKitGTK version each major distro (Ubuntu 22.04/24.04, Fedora, Debian) currently ships; matters for how many Linux users have Event Timing.
- Did not run a live `supportedEntryTypes` probe inside a Tauri/wry WebKitGTK window to confirm no embedder override disables Event Timing.
- WebKit's current `performance.now()` resolution (WKWebView/WebKitGTK, non-isolated) was not confirmed from a primary source; if it is ~1 ms (as Firefox), then combined with Event Timing's 8 ms rounding, sub-millisecond client timings are meaningless on macOS/Linux Tauri — measure server handler time in Rust instead.
- Safari 17/18 explicitly: per BCD they lack Event Timing/LCP (added 26.2); no separate source fetched for "Safari 26.0/26.1" WKWebView behavior.

---

## 4. Measuring client-perceived latency end to end and attributing it

### Takeaway

Measure only _durations_ on each side with monotonic clocks and never subtract timestamps across machines: client measures `t_send → t_response_received → t_painted`; the server returns its own handler duration (monotonic, in the response envelope, Server-Timing style); then `network+queue ≈ client_rtt − server_ms`, `client_render ≈ t_painted − t_response_received`.

### Cited Findings

- `performance.now()` is monotonic; MDN gives 100 µs (non-isolated) / 5 µs (isolated) resolution (Firefox is 1 ms per BCD; WebKit unconfirmed, see section 3), and it does not advance through OS sleep except on Windows; `Date.now()` is subject to clock adjustments — [MDN performance.now()](https://developer.mozilla.org/en-US/docs/Web/API/Performance/now)
- React `<Profiler onRender>` provides `actualDuration`, `baseDuration`, `startTime`, `commitTime` ("shared between all profilers in a commit"); "Profiling adds some additional overhead, so it is disabled in the production build by default" and requires "a special production build with profiling enabled" — [react.dev Profiler](https://react.dev/reference/react/Profiler)
- INP decomposes interaction latency into input delay, processing duration and presentation delay up to next paint — the browser's own model of "until the user sees it" — [web.dev INP](https://web.dev/articles/inp)
- Event Timing entries' `duration` runs from `startTime` to the next rendering paint, rounded to 8 ms — [MDN PerformanceEventTiming](https://developer.mozilla.org/en-US/docs/Web/API/PerformanceEventTiming)
- LoAF exposes `renderStart` and `styleAndLayoutStart` within a frame (Chromium only) — [Chrome for Developers, LoAF](https://developer.chrome.com/docs/web-platform/long-animation-frames)
- SRE: the 99th percentile of one backend can become the median of the frontend — so per-hop attribution matters — [SRE Book](https://sre.google/sre-book/monitoring-distributed-systems/)

### Inferences

- **Protocol shape:** add `serverMs` (handler duration measured with `std::time::Instant` from decode to encode) and optionally `queuedMs` (time from frame receipt to handler start) to each RPC response. Client computes `rtt = performance.now()@receive − performance.now()@send`; `transport ≈ rtt − serverMs − queuedMs`. This is the same idea as HTTP `Server-Timing` but in-band, since WebSocket frames have no response headers. No clock synchronization needed because only intra-host durations are exchanged.
- **Render time:** after applying the response to state, mark `performance.mark('rpc:applied')`, then `requestAnimationFrame(() => requestAnimationFrame(markPainted))` — the second rAF callback runs after the frame containing the update has been produced (an approximation of presentation; inference, not a spec guarantee). `useEffect`/`useLayoutEffect` in the consuming component marks commit; Profiler's `commitTime` is only available in a profiling build, so production should use the effect + double-rAF approach.
- When the RPC is triggered by a user input and Event Timing is available, the `event` entry for that interaction already gives input→paint; correlate via `interactionId` and the RPC's send timestamp to split "waiting on server" vs "rendering".
- **Cross-machine timelines** (remote server over SSH/relay) only if needed: estimate offset with the NTP four-timestamp method (offset = ((T2−T1)+(Tc−Td))/2, delay = (Td−T1)−(Tc−T2), where T1 is client send, T2 server receive, Tc server send and Td client receive), taking the min-delay sample of several. Not needed for slow-RPC attribution, which works on durations.
- Discard samples where `document.visibilityState` went hidden during the measurement or where `performance.now()` deltas exceed a sanity bound (sleep/resume on macOS/Linux does not advance the clock, but a backgrounded tab delays rAF).
- Server-side flags (section 1) and client-side flags should use the same `requestId` so a slow server handler, slow transport, and slow render can be shown as one record.

### Gaps

- No primary source fetched for the "double rAF ≈ after paint" pattern; it is widely used folklore. The HTML event-loop spec would be the authority.
- RFC 5905 (NTP) offset formula cited from knowledge, not fetched.
- React docs' exact instructions for the profiling build (`react-dom/profiling` alias) not fetched (linked page: https://react.dev/reference/dev-tools/react-performance-tracks#using-profiling-builds).

---

## 5. Overhead of these observers in production

### Takeaway

No primary source quantifies the CPU/memory overhead of `PerformanceObserver` for `longtask`, `long-animation-frame` or `event`; the APIs are designed for always-on production RUM (they are the basis of Core Web Vitals field data), with built-in thresholds (50 ms, 104 ms default/16 ms min) and bounded buffers. The measurable costs are ones you add: React profiling builds, high-frequency rAF loops, and unbounded client-side entry retention.

### Cited Findings

- React profiling "adds some additional overhead, so it is disabled in the production build by default" — [react.dev Profiler](https://react.dev/reference/react/Profiler)
- Event Timing only emits entries above `durationThreshold` (default 104 ms, min 16 ms) and excludes continuous events (`mousemove`, `pointermove`, `wheel`, etc.) — [MDN PerformanceEventTiming](https://developer.mozilla.org/en-US/docs/Web/API/PerformanceEventTiming)
- LoAF/long-task performance-timeline buffers are capped at 200 entries; the Chrome doc does not discuss observer overhead — [Chrome for Developers, LoAF](https://developer.chrome.com/docs/web-platform/long-animation-frames)
- INP is computed from field data at p75 across page loads, i.e. these APIs are intended to run on real users' pages — [web.dev INP](https://web.dev/articles/inp)
- hdrhistogram record is constant time, 3–6 ns, fixed memory — server-side per-RPC recording cost is negligible relative to JSON-RPC decode — [docs.rs hdrhistogram](https://docs.rs/hdrhistogram/latest/hdrhistogram/)

### Inferences

- Keep native observers always on (they are cheap and filtered); keep the rAF frame-gap fallback **off by default** or only while a profiler/diagnostics view is open, because a perpetual rAF loop prevents the webview from idling (CPU/battery cost on WebKitGTK/WKWebView where it is the only stall signal). A 250–500 ms `setTimeout` heartbeat is a cheaper always-on stall detector (detects ≥50 ms drift with coarse sampling).
- Use `durationThreshold: 104` (default) for production, 16 ms only in an explicit profiling session.
- Aggregate client-side into small per-window summaries (counts, max, p75 via a small log-bucket array) and send them over the existing RPC channel periodically, rather than one message per entry; never retain raw entries unbounded.
- Do not ship a React profiling build to users; use Profiler only in a dedicated diagnostics build.

### Gaps

- No benchmark found quantifying PerformanceObserver overhead for `event`/`longtask`/`long-animation-frame` in any engine; this would need an in-app measurement.
- No source found on the overhead of rAF loops specifically in WebKitGTK compositing; inference only.
