import { useEffect, useState } from "react";

const TICK_INTERVAL_MS = 120;
const FADE_OUT_DELAY_MS = 220;
const SEED_PERCENT = 4;
const ASYMPTOTE_PERCENT = 90;
const APPROACH_FACTOR = 0.08;
const MIN_INCREMENT = 0.5;

/**
 * Indeterminate progress simulator for the preview chrome's loading bar.
 * Animates 0 → 90% asymptotically while `loading` is true, snaps to 100%
 * on release, then resets after a short pause.
 *
 * The seed and the snap happen in the render that flips `loading`; interval
 * ticks advance from the latest progress through a functional update, so
 * `loading` alone owns the interval.
 */
export function useLoadingProgress(loading: boolean): number {
  const [progress, setProgress] = useState(loading ? SEED_PERCENT : 0);
  const [previousLoading, setPreviousLoading] = useState(loading);
  if (loading !== previousLoading) {
    setPreviousLoading(loading);
    if (loading) {
      setProgress(progress > 0 && progress < 95 ? progress : SEED_PERCENT);
    } else if (progress !== 0) {
      setProgress(100);
    }
  }

  useEffect(() => {
    if (!loading) return;
    const interval = window.setInterval(() => {
      setProgress((current) => {
        if (current >= ASYMPTOTE_PERCENT) return current;
        const remaining = ASYMPTOTE_PERCENT - current;
        const increment = Math.max(MIN_INCREMENT, remaining * APPROACH_FACTOR);
        return Math.min(ASYMPTOTE_PERCENT, current + increment);
      });
    }, TICK_INTERVAL_MS);
    return () => window.clearInterval(interval);
  }, [loading]);

  const completed = !loading && progress === 100;
  useEffect(() => {
    if (!completed) return;
    const timer = window.setTimeout(() => setProgress(0), FADE_OUT_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [completed]);

  return progress;
}
