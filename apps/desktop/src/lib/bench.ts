// Frame work (spec §11): per animation frame, the time to apply its messages, flush the DOM and read
// the panes' layout. Measured in the app's own engine, the Tauri webview, driven by a fixture.
import type { Session } from "./session.svelte";

/** Nearest-rank percentile. */
export function percentile(xs: number[], p: number): number {
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.max(0, Math.ceil((p / 100) * s.length) - 1)];
}

export type Report = { name: string; engine: string; frames: number; p50: number; p95: number; max: number; over_16_7: number; hydrate_ms: number; notes: string };

const round = (x: number) => Math.round(x * 100) / 100;

export function measure(session: Session, name: string, hydrateMs: number) {
  const samples: number[] = [];
  session.onDrain = (ms) => samples.push(ms);
  return {
    stop(notes: string): Report {
      session.onDrain = null;
      return {
        name,
        engine: navigator.userAgent,
        frames: samples.length,
        p50: round(percentile(samples, 50)),
        p95: round(percentile(samples, 95)),
        max: round(Math.max(...samples)),
        over_16_7: samples.filter((x) => x > 16.7).length,
        hydrate_ms: round(hydrateMs),
        notes,
      };
    },
  };
}
