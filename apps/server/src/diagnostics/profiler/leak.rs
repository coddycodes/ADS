//! Leak and sustained-CPU rules over one process's 1-minute series (RESIN-style): a
//! 15-minute warm-up, a rolling 10-minute minimum so sawtooth peaks never count as growth,
//! and a baseline reset on step changes and sampling gaps.

use std::collections::VecDeque;

const WARMUP_MIN: u64 = 15;
const FLOOR_WINDOW_MIN: usize = 10;
const SUSPECT_WINDOW_MIN: usize = 60;
const LIKELY_WINDOW_MIN: usize = 360;
const GAP_RESET_MIN: u64 = 5;
const MB: f64 = 1024.0 * 1024.0;
const STEP_MIN_BYTES: f64 = 64.0 * MB;
const CPU_THRESHOLD: f64 = 0.8;
const CPU_SUSTAINED_SECS: u64 = 300;
/// Minute medians arrive 60 s apart, or 120 s when the throttled sampler skips a minute.
const CPU_MAX_GAP_SECS: u64 = 120;

/// One private-memory reading, on the sampler's monotonic clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemorySample {
    pub at_secs: u64,
    pub private_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LeakVerdict {
    None,
    Suspect { slope_mb_per_h: f64 },
    Likely { slope_mb_per_h: f64, r2: f64 },
}

#[derive(Default)]
pub struct LeakDetector {
    raw: VecDeque<(u64, f64)>,   // (minute, bytes) since last reset
    floor: VecDeque<(u64, f64)>, // rolling-minimum series
    started: Option<u64>,
    last_minute: Option<u64>,
    baseline: Option<f64>,
    consecutive_hits: u8,
    last_alert: Option<u64>,
}

impl LeakDetector {
    pub fn window_len(&self) -> usize {
        self.raw.len()
    }

    /// Feeds one 1-minute median. `minute` must increase strictly. Returns a verdict on the
    /// second consecutive hit, at most once per 6 h.
    pub fn push_minute(&mut self, minute: u64, private_bytes: u64) -> Option<LeakVerdict> {
        let value = private_bytes as f64;
        if self
            .last_minute
            .is_some_and(|last| minute.saturating_sub(last) > GAP_RESET_MIN)
        {
            self.reset(minute);
        }
        self.last_minute = Some(minute);
        let started = *self.started.get_or_insert(minute);
        self.raw.push_back((minute, value));
        while self.raw.len() > LIKELY_WINDOW_MIN {
            self.raw.pop_front();
        }
        let floor_value = self
            .raw
            .iter()
            .rev()
            .take(FLOOR_WINDOW_MIN)
            .map(|p| p.1)
            .fold(f64::INFINITY, f64::min);
        if minute - started < WARMUP_MIN {
            return None;
        }
        // A step change > 3σ of the recent floor resets the baseline (RESIN).
        if let Some(prev) = self.floor.back().map(|p| p.1) {
            let sigma = std_dev(
                self.floor
                    .iter()
                    .rev()
                    .take(SUSPECT_WINDOW_MIN)
                    .map(|p| p.1),
            );
            // A flat floor has sigma 0, so the minimum jump must not depend on sigma alone.
            // 64 MB per minute only resets for genuine steps; a 30 MB/h leak moves the
            // floor ~0.5 MB per minute and is never mistaken for a step.
            if self.floor.len() >= FLOOR_WINDOW_MIN
                && (floor_value - prev).abs() > (3.0 * sigma).max(STEP_MIN_BYTES)
            {
                self.reset_keep(minute, floor_value);
                return None;
            }
        }
        self.floor.push_back((minute, floor_value));
        while self.floor.len() > LIKELY_WINDOW_MIN {
            self.floor.pop_front();
        }
        let baseline = *self.baseline.get_or_insert(floor_value);
        let verdict = self.evaluate(baseline);
        match verdict {
            LeakVerdict::None => {
                self.consecutive_hits = 0;
                None
            }
            hit => {
                self.consecutive_hits = self.consecutive_hits.saturating_add(1);
                let cooled = self.last_alert.is_none_or(|t| minute - t >= 360);
                if self.consecutive_hits >= 2 && cooled {
                    self.last_alert = Some(minute);
                    Some(hit)
                } else {
                    None
                }
            }
        }
    }

    fn evaluate(&self, baseline: f64) -> LeakVerdict {
        if self.floor.len() < SUSPECT_WINDOW_MIN {
            return LeakVerdict::None;
        }
        let points: Vec<(f64, f64)> = self
            .floor
            .iter()
            .map(|(m, v)| (*m as f64 / 60.0, *v / MB))
            .collect();
        let recent = &points[points.len() - SUSPECT_WINDOW_MIN..];
        let slope = theil_sen(recent); // MB per hour
        let threshold = (10.0f64).max(0.02 * baseline / MB);
        let z = mann_kendall_z(&recent.iter().map(|p| p.1).collect::<Vec<_>>());
        let latest = points.last().map_or(0.0, |p| p.1);
        let growth = latest - baseline / MB;
        let r2 = r_squared(&points);
        // Two alternatives: 6 h of growth at the suspect rate that fits a line, or growth of
        // at least 50% and 200 MB since the baseline, which needs only the 1 h window.
        let fits_six_hours = points.len() >= LIKELY_WINDOW_MIN && slope >= threshold && r2 > 0.8;
        let grew_large = growth >= 200.0 && growth >= 0.5 * baseline / MB;
        if fits_six_hours || grew_large {
            return LeakVerdict::Likely {
                slope_mb_per_h: slope,
                r2,
            };
        }
        if slope >= threshold && z > 2.33 {
            LeakVerdict::Suspect {
                slope_mb_per_h: slope,
            }
        } else {
            LeakVerdict::None
        }
    }

    fn reset(&mut self, minute: u64) {
        self.raw.clear();
        self.floor.clear();
        self.started = Some(minute);
        self.baseline = None;
        self.consecutive_hits = 0;
    }

    fn reset_keep(&mut self, minute: u64, floor_value: f64) {
        self.floor.clear();
        self.floor.push_back((minute, floor_value));
        self.baseline = Some(floor_value);
        self.consecutive_hits = 0;
    }
}

/// Tracks how long a process has stayed above 80% of one core.
#[derive(Default)]
pub struct CpuDetector {
    above_since: Option<u64>,
    last_at: Option<u64>,
}

impl CpuDetector {
    /// True while the current run above the threshold has lasted at least 5 minutes. A gap
    /// in the samples ends the run: nothing proves the process stayed busy through it.
    pub fn push(&mut self, cpu_fraction_of_one_core: f64, at_secs: u64) -> bool {
        let continuous = self
            .last_at
            .is_some_and(|last| at_secs.saturating_sub(last) <= CPU_MAX_GAP_SECS);
        self.last_at = Some(at_secs);
        if cpu_fraction_of_one_core <= CPU_THRESHOLD || !continuous {
            self.above_since = None;
        }
        if cpu_fraction_of_one_core <= CPU_THRESHOLD {
            return false;
        }
        let start = *self.above_since.get_or_insert(at_secs);
        at_secs.saturating_sub(start) >= CPU_SUSTAINED_SECS
    }
}

pub fn theil_sen(points: &[(f64, f64)]) -> f64 {
    let mut slopes = Vec::with_capacity(points.len() * points.len() / 2);
    for i in 0..points.len() {
        for j in i + 1..points.len() {
            let dx = points[j].0 - points[i].0;
            if dx != 0.0 {
                slopes.push((points[j].1 - points[i].1) / dx);
            }
        }
    }
    median(&mut slopes)
}

pub fn mann_kendall_z(values: &[f64]) -> f64 {
    let n = values.len() as f64;
    if n < 3.0 {
        return 0.0;
    }
    let mut s = 0.0;
    for i in 0..values.len() {
        for j in i + 1..values.len() {
            // Not `signum`: it returns 1.0 for 0.0, which would count ties as increases.
            let d = values[j] - values[i];
            s += if d > 0.0 {
                1.0
            } else if d < 0.0 {
                -1.0
            } else {
                0.0
            };
        }
    }
    let var = n * (n - 1.0) * (2.0 * n + 5.0) / 18.0;
    if s > 0.0 {
        (s - 1.0) / var.sqrt()
    } else if s < 0.0 {
        (s + 1.0) / var.sqrt()
    } else {
        0.0
    }
}

pub fn r_squared(points: &[(f64, f64)]) -> f64 {
    let n = points.len() as f64;
    let (mx, my) = (
        points.iter().map(|p| p.0).sum::<f64>() / n,
        points.iter().map(|p| p.1).sum::<f64>() / n,
    );
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (x, y) in points {
        sxy += (x - mx) * (y - my);
        sxx += (x - mx).powi(2);
        syy += (y - my).powi(2);
    }
    if sxx == 0.0 || syy == 0.0 {
        return 0.0;
    }
    (sxy * sxy) / (sxx * syy)
}

/// The median, or 0.0 for an empty slice. Sorts `values` ascending.
pub(crate) fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_unstable_by(f64::total_cmp);
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

/// Population standard deviation, or 0.0 for fewer than two values.
fn std_dev(values: impl Iterator<Item = f64>) -> f64 {
    let values: Vec<f64> = values.collect();
    if values.len() < 2 {
        return 0.0;
    }
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    const MB: u64 = 1024 * 1024;

    fn feed(det: &mut LeakDetector, minutes: u64, f: impl Fn(u64) -> u64) -> Vec<LeakVerdict> {
        (0..minutes)
            .filter_map(|m| det.push_minute(m, f(m)))
            .collect()
    }

    #[test]
    fn steady_growth_becomes_suspect_after_warmup_plus_an_hour() {
        let mut det = LeakDetector::default();
        // 30 MB/h steady growth on a 500 MB base.
        let verdicts = feed(&mut det, 15 + 60 + 5, |m| 500 * MB + m * MB / 2);
        assert!(
            matches!(verdicts.first(), Some(LeakVerdict::Suspect { slope_mb_per_h }) if *slope_mb_per_h > 25.0)
        );
    }

    #[test]
    fn sawtooth_without_floor_growth_is_not_a_leak() {
        let mut det = LeakDetector::default();
        let verdicts = feed(&mut det, 240, |m| 500 * MB + (m % 10) * 40 * MB);
        assert!(verdicts.is_empty());
    }

    #[test]
    fn single_step_up_then_flat_resets_baseline() {
        let mut det = LeakDetector::default();
        let verdicts = feed(&mut det, 240, |m| if m < 60 { 500 * MB } else { 800 * MB });
        assert!(verdicts.is_empty());
    }

    #[test]
    fn slow_growth_below_threshold_is_ignored() {
        let mut det = LeakDetector::default();
        // 5 MB/h on 500 MB: below max(10 MB/h, 10 MB/h).
        let verdicts = feed(&mut det, 240, |m| 500 * MB + m * MB / 12);
        assert!(verdicts.is_empty());
    }

    #[test]
    fn a_sampling_gap_over_five_minutes_restarts_warmup() {
        let mut det = LeakDetector::default();
        for m in 0..50 {
            det.push_minute(m, 500 * MB + m * MB);
        }
        // Laptop slept 3 hours; growth across the gap must not count.
        assert!(det.push_minute(230, 900 * MB).is_none());
        assert_eq!(det.window_len(), 1);
    }

    #[test]
    fn statistics_helpers() {
        let pts: Vec<(f64, f64)> = (0..10).map(|i| (i as f64, 2.0 * i as f64 + 1.0)).collect();
        assert!((theil_sen(&pts) - 2.0).abs() < 1e-9);
        assert!((r_squared(&pts) - 1.0).abs() < 1e-9);
        let rising: Vec<f64> = (0..30).map(f64::from).collect();
        assert!(mann_kendall_z(&rising) > 2.33);
    }

    #[test]
    fn cpu_detector_needs_five_minutes_above_threshold() {
        let mut cpu = CpuDetector::default();
        assert!(!(0..290).step_by(5).any(|s| cpu.push(0.95, s)));
        assert!(cpu.push(0.95, 300));
        let mut cpu = CpuDetector::default();
        assert!(
            !(0..600)
                .step_by(5)
                .any(|s| cpu.push(if s % 60 < 30 { 0.95 } else { 0.1 }, s))
        );
    }

    #[test]
    fn slow_growth_on_a_large_process_never_becomes_likely() {
        let mut det = LeakDetector::default();
        // 6 MB/h on 4 GB: far below 2%/h, however well a line fits it.
        let verdicts = feed(&mut det, 500, |m| 4096 * MB + m * MB / 10);
        assert!(verdicts.is_empty());
    }

    #[test]
    fn large_growth_is_likely_as_soon_as_the_hour_window_exists() {
        let mut det = LeakDetector::default();
        // 1 GB/h on a 500 MB base passes 50% and 200 MB of growth long before 6 h.
        let first = (0..120).find_map(|m| {
            det.push_minute(m, 500 * MB + m * 1024 * MB / 60)
                .map(|verdict| (m, verdict))
        });
        assert!(
            matches!(first, Some((m, LeakVerdict::Likely { .. })) if m <= WARMUP_MIN + 60 + 1),
            "{first:?}"
        );
    }

    #[test]
    fn cpu_detector_restarts_after_a_sampling_gap() {
        let mut cpu = CpuDetector::default();
        assert!(!cpu.push(0.95, 0));
        assert!(!cpu.push(0.95, 60));
        // Nothing was observed for nine minutes, so the run starts over.
        assert!(!cpu.push(0.95, 600));
        assert!(!(660..900).step_by(60).any(|s| cpu.push(0.95, s)));
        assert!(cpu.push(0.95, 900));
    }
}
