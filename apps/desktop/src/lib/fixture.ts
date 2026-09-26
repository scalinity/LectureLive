// A scripted backend: the store's tests, the frame-time bench and the browser preview drive the real
// store through it, with the adapter's session and sequence rules.
import type { DropEvent, Transport } from "./transport";
import type { Envelope, NotesMsg, SessionState, Status, StatusMsg, TranscriptMsg, WindowView } from "./wire";

export function idleStatus(): Status {
  return { phase: "idle", folder: null, source: null, level_dbfs: null, stt: "not started", stt_ok: false, busy: null, gaps: 0, started_at: null, spend_usd: 0, silence: false, capture: { state: "unbound", window: null, detail: null, candidates: [], captured: false } };
}

export function emptyState(session: string): SessionState {
  return { session, seq: 0, status: idleStatus(), notices: [], segments: [], open: null, revision: 0, document: "", preview: null, op: 1, slides: [], pending_segments: 0, pending_slides: 0 };
}

type Handler<T> = (m: Envelope<T>) => void;

export class FixtureTransport implements Transport {
  state_: SessionState;
  lastSeq: number;
  calls: [string, unknown][] = [];
  /** What `call` answers, by command. */
  answers: Record<string, unknown> = {};
  /** The order the store reached the backend in. */
  order: string[] = [];
  stateReads = 0;
  private held: (() => void)[] | null = null;
  private last: (() => void) | null = null;
  private onStatus: Handler<StatusMsg>[] = [];
  private onTranscript: Handler<TranscriptMsg> | null = null;
  private onNotes: Handler<NotesMsg> | null = null;
  private onDrop: ((e: DropEvent) => void) | null = null;

  constructor(state: SessionState) {
    this.state_ = state;
    this.lastSeq = state.seq;
  }

  private stamp<T>(msg: T, seq?: number, session?: string): Envelope<T> {
    this.lastSeq = seq ?? this.lastSeq + 1;
    return { ...msg, session: session ?? this.state_.session, seq: this.lastSeq };
  }

  async listenStatus(cb: Handler<StatusMsg>) {
    this.order.push("listen");
    this.onStatus.push(cb);
    return () => {
      this.onStatus = this.onStatus.filter((h) => h !== cb);
    };
  }

  async attach(onTranscript: Handler<TranscriptMsg>, onNotes: Handler<NotesMsg>) {
    this.order.push("attach");
    this.onTranscript = onTranscript;
    this.onNotes = onNotes;
  }

  async state(): Promise<SessionState> {
    this.order.push("state");
    this.stateReads++;
    if (this.held) await new Promise<void>((r) => this.held!.push(r));
    return structuredClone(this.state_);
  }

  /** State reads wait until `releaseState`. */
  holdState() {
    this.held = [];
  }

  releaseState() {
    const waiting = this.held ?? [];
    this.held = null;
    waiting.forEach((r) => r());
  }

  /** The next message is stamped `n + 1`. */
  setSeq(n: number) {
    this.lastSeq = n;
  }

  /** Delivers the last message of this session again, with its sequence number. */
  replayLast() {
    this.last?.();
  }

  async call<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    this.calls.push([cmd, args]);
    return this.answers[cmd] as T;
  }

  async listenDrops(cb: (e: DropEvent) => void) {
    this.onDrop = cb;
    return () => {
      this.onDrop = null;
    };
  }

  /** A file drag over the window, as the webview reports it. */
  emitDrop(e: DropEvent) {
    this.onDrop?.(e);
  }

  assetUrl(path: string) {
    return "asset://localhost/" + encodeURIComponent(path);
  }

  emitStatus(msg: StatusMsg, seq?: number, session?: string) {
    const m = this.stamp(msg, seq, session);
    this.deliver(() => this.onStatus.forEach((h) => h(m)), m.session);
  }

  emitTranscript(msg: TranscriptMsg, seq?: number, session?: string) {
    const m = this.stamp(msg, seq, session);
    this.deliver(() => this.onTranscript?.(m), m.session);
  }

  emitNotes(msg: NotesMsg, seq?: number, session?: string) {
    const m = this.stamp(msg, seq, session);
    this.deliver(() => this.onNotes?.(m), m.session);
  }

  private deliver(send: () => void, session: string) {
    if (session === this.state_.session) this.last = send;
    send();
  }
}

const LECTURE = [
  "Last week we set up the loss as a function of the weights, so today the question is how to move downhill.",
  "Gradient descent takes a step against the gradient, scaled by the learning rate.",
  "If the learning rate is too small you crawl, and if it is too large you overshoot the minimum and bounce around.",
  "Here is the update rule on the slide: w becomes w minus eta times the gradient of L.",
  "Notice that eta is the only thing we choose; the gradient comes from the data.",
  "A useful picture is a ball rolling in a bowl, where the bowl is the loss surface.",
  "Stochastic gradient descent uses one mini-batch at a time instead of the whole data set.",
  "That makes each step noisy but much cheaper, and the noise can even help escape shallow minima.",
  "Momentum keeps a running average of past gradients so the ball keeps rolling through flat stretches.",
  "This will be on the exam: be able to write the momentum update and say what beta does.",
];

const clock = (base: number, i: number) => {
  const s = base + i * 17;
  return [Math.floor(s / 3600), Math.floor((s % 3600) / 60), s % 60].map((n) => String(n).padStart(2, "0")).join(":");
};

/** The browser preview: a lecture in progress, then new speech every few seconds. */
export function demoTransport(): FixtureTransport {
  const st = emptyState("demo");
  st.status = { ...idleStatus(), phase: "running", source: "BlackHole 2ch", level_dbfs: -24, stt: "transcribing", stt_ok: true, started_at: new Date(Date.now() - 42 * 60_000).toISOString(), spend_usd: 0.14, folder: { dir: "/Lectures/Machine Learning/Weeks/Week 06 — Optimisation", course: "Machine Learning", name: "Week 06 — Optimisation", notes_dir: "/Lectures/Machine Learning/Weeks/Week 06 — Optimisation", page: null } };
  st.segments = LECTURE.slice(0, 6).map((text, id) => ({ id, at: clock(10 * 3600 + 2 * 60, id), text, recovered: id === 3 }));
  const t = new FixtureTransport(st);
  let id = 6;
  let utterance = 1;
  const speak = () => {
    const words = LECTURE[id % LECTURE.length].split(" ");
    let n = 0;
    const tick = () => {
      n = Math.min(words.length, n + 2);
      const stable = words.slice(0, Math.max(0, n - 3)).join(" ");
      const tentative = words.slice(Math.max(0, n - 3), n).join(" ");
      t.emitTranscript({ type: "open", utterance, stable, tentative });
      if (n < words.length) return setTimeout(tick, 450);
      setTimeout(() => {
        t.emitTranscript({ type: "closed", utterance, segment: { id, at: clock(10 * 3600 + 2 * 60, id), text: words.join(" "), recovered: false } });
        id++;
        utterance++;
        setTimeout(speak, 900);
      }, 500);
    };
    tick();
  };
  setTimeout(speak, 800);
  return t;
}

const DEMO_DIR = "/Lectures/Machine Learning/Weeks/Week 06 — Optimisation";

/** What the picker lists in the browser preview. */
const DEMO_WINDOWS: WindowView[] = [
  { id: 2621, app: "zoom.us", title: "Zoom Meeting", width: 1600, height: 900, on_screen: true },
  { id: 2598, app: "zoom.us", title: "Zoom Workplace", width: 960, height: 640, on_screen: false },
  { id: 69, app: "Google Chrome", title: "Week 6 slides", width: 1440, height: 900, on_screen: true },
  { id: 1433, app: "Terminal", title: "lecture", width: 900, height: 600, on_screen: true },
];
const DEMO_NOTES = `# Machine Learning — Week 06 — Optimisation — 2026-09-25

<!-- 10:04:12 -->
## Gradient descent
- The loss is a function of the weights; training moves downhill on it.
- Each step goes against the gradient, scaled by the **learning rate** η.
  - Too small: slow progress. Too large: overshoots and oscillates.

![Slide 1](slides/slide_01_100251.png)

| Variant | Gradient from | Cost per step |
|---|---|---|
| Batch | whole data set | high |
| Stochastic | one mini-batch | low |
`;

const DEMO_SNAPSHOT = `## Momentum
- Keeps a running average of past gradients: \`v ← βv + ∇L\`, then \`w ← w − ηv\`.
- The ball keeps rolling through flat stretches and damps zig-zags across narrow valleys.

**Exam:** be able to write the momentum update and say what β does.
`;

/** The browser preview with notes: a committed document, a slide, then a streamed snapshot. */
export function demoWithNotes(): FixtureTransport {
  const t = demoTransport();
  t.state_.status.folder = { dir: DEMO_DIR, course: "Machine Learning", name: "Week 06 — Optimisation", notes_dir: DEMO_DIR, page: null };
  t.state_.document = DEMO_NOTES;
  t.state_.revision = 2;
  t.state_.slides = [
    { index: 1, file: "slides/slide_01_100251.png", path: `${DEMO_DIR}/slides/slide_01_100251.png`, at: "10:02:51", auto: true, uncertain: false },
    { index: 2, file: "slides/slide_02_100418.png", path: `${DEMO_DIR}/slides/slide_02_100418.png`, at: "10:04:18", auto: false, uncertain: false },
    { index: 3, file: "slides/slide_03_100633.png", path: `${DEMO_DIR}/slides/slide_03_100633.png`, at: "10:06:33", auto: true, uncertain: true },
  ];
  t.state_.status.capture = { state: "watching", window: "Zoom Meeting", detail: null, candidates: [], captured: true };
  t.assetUrl = () => "/demo-slide.svg";
  t.answers = { capture_windows: DEMO_WINDOWS, capture_preview: { path: "/demo-slide.svg", width: 1600, height: 900 }, capture_saved_region: { x: 0.06, y: 0.1, w: 0.88, h: 0.8 }, capture_select: null, capture_now: null, import_slides: null, key_status: { stored: true, env_available: true }, inputs: [{ name: "MacBook Pro Microphone", uid: "BuiltInMicrophoneDevice" }, { name: "BlackHole 2ch", uid: "BlackHole2ch_UID" }], loopback_status: { present: true, blackhole_present: true }, spend_summary: { total: 3.8412, estimated: 0.4071, calls: 57, months: [{ key: "2026-08", label: "August 2026", total: 1.2033, courses: [["Machine Learning", 0.9021], ["Statistics", 0.3012]] }, { key: "2026-09", label: "September 2026", total: 2.6379, courses: [["Machine Learning", 1.9112], ["Statistics", 0.6021], ["Biology", 0.1246]] }], recent: [{ day: "2026-09-25", label: "25 Sep", course: "Machine Learning", lecture: "Week 06 — Optimisation", total: 0.4402, kinds: [["page", 0.2727], ["notes", 0.1021], ["transcribe", 0.0533], ["polish", 0.0121]] }, { day: "2026-09-24", label: "24 Sep", course: "Statistics", lecture: "Week 05 — Hypothesis tests", total: 0.3104, kinds: [["notes", 0.1902], ["transcribe", 0.1202]] }, { day: "2026-09-22", label: "22 Sep", course: "Biology", lecture: "Week 02", total: 0.004, kinds: [["notes", 0.004]] }] } };
  (globalThis as { __fixture?: FixtureTransport }).__fixture = t; // the preview's checks drive states through it
  const words = DEMO_SNAPSHOT.match(/\S+\s*/g) ?? [];
  setTimeout(() => {
    let i = 0;
    const tick = () => {
      t.emitNotes({ type: "delta", op: 1, text: words.slice(i, i + 2).join("") });
      i += 2;
      if (i < words.length) return setTimeout(tick, 60);
      setTimeout(() => t.emitNotes({ type: "committed", op: 1, revision: 3, block: `\n<!-- 10:07:40 -->\n${DEMO_SNAPSHOT}\n` }), 3000);
    };
    tick();
  }, 5000);
  return t;
}

// The gate's fixtures (spec §11): a 500-delta/s burst, and a two-hour lecture streaming live.

export type Fixture = { name: string; state: SessionState; run(t: FixtureTransport, done: (notes: string) => void): void };

const VOCAB = "the gradient of the loss points uphill so each step moves against it by the learning rate and momentum keeps a running average of past steps through flat stretches of the surface while noise from mini batches helps escape shallow minima near a saddle point".split(" ");
const word = (i: number) => VOCAB[i % VOCAB.length];
const text = (from: number, n: number) => Array.from({ length: n }, (_, i) => word(from + i)).join(" ");
const hms = (s: number) => [Math.floor(s / 3600) % 24, Math.floor((s % 3600) / 60), s % 60].map((n) => String(n).padStart(2, "0")).join(":");

/** Plays the live transcript: an open-utterance update every 500 ms, a closed segment every 5 s. */
function speaker(t: FixtureTransport, firstId: number, startSec: number) {
  let opens = 0;
  let closes = 0;
  return (elapsedMs: number) => {
    while (opens < Math.floor(elapsedMs / 500)) {
      opens++;
      const n = 2 + (opens % 10) * 2;
      t.emitTranscript({ type: "open", utterance: closes + 1, stable: text(opens, Math.max(0, n - 3)), tentative: text(opens + n, 3) });
    }
    while (closes < Math.floor(elapsedMs / 5000)) {
      closes++;
      t.emitTranscript({ type: "closed", utterance: closes, segment: { id: firstId + closes - 1, at: hms(startSec + closes * 5), text: text(closes * 7, 13), recovered: false } });
    }
  };
}

function running(session: string): SessionState {
  const st = emptyState(session);
  st.status = { ...idleStatus(), phase: "running", source: "BlackHole 2ch", level_dbfs: -24, stt: "transcribing", stt_ok: true, started_at: new Date().toISOString() };
  return st;
}

/** 10 s of notes deltas at 500 per second (word-sized, a heading every 40), with live speech. */
export function burstFixture(): Fixture {
  return {
    name: "bench-burst",
    state: running("bench"),
    run(t, done) {
      const start = performance.now();
      const speak = speaker(t, 0, 10 * 3600);
      let sent = 0;
      const timer = setInterval(() => {
        const el = performance.now() - start;
        const due = Math.min(5000, Math.floor((el / 1000) * 500));
        for (; sent < due; sent++) {
          const lead = sent % 40 === 0 ? `\n\n## Topic ${sent / 40 + 1}\n- ` : sent % 8 === 0 ? "\n- " : "";
          t.emitNotes({ type: "delta", op: 1, text: `${lead}${word(sent)} ` });
        }
        speak(el);
        if (sent >= 5000) {
          clearInterval(timer);
          t.emitNotes({ type: "committed", op: 1, revision: 1, block: "\n<!-- 10:00:10 -->\n## Burst\n- committed\n" });
          setTimeout(() => done(`5000 deltas in ${Math.round(el)} ms; ${Math.floor(el / 500)} open updates, ${Math.floor(el / 5000)} segments`), 500);
        }
      }, 4);
    },
  };
}

/** A two-hour lecture already on screen (1,440 segments, a 6,000-word document in 60 snapshots), then
 *  30 s live: speech, and one snapshot streamed at 60 deltas per second and committed. */
export function twoHourFixture(): Fixture {
  const st = running("bench");
  st.segments = Array.from({ length: 1440 }, (_, id) => ({ id, at: hms(10 * 3600 + id * 5), text: text(id, 12 + (id % 3)), recovered: id % 97 === 0 }));
  const chunks = Array.from({ length: 60 }, (_, i) => `\n<!-- ${hms(10 * 3600 + i * 120)} -->\n## Topic ${i + 1}\n${Array.from({ length: 5 }, (_, b) => `- ${text(i * 100 + b * 20, 20)}`).join("\n")}\n`);
  st.document = `# Machine Learning — Week 06 — Optimisation — 2026-09-25\n${chunks.join("")}`;
  st.revision = 60;
  return {
    name: "bench-twohour",
    state: st,
    run(t, done) {
      t.setSeq(0);
      const start = performance.now();
      const speak = speaker(t, 1440, 12 * 3600);
      let sent = 0;
      let committed = false;
      const timer = setInterval(() => {
        const el = performance.now() - start;
        speak(el);
        const due = el < 5000 ? 0 : Math.min(900, Math.floor(((el - 5000) / 1000) * 60));
        for (; sent < due; sent++) t.emitNotes({ type: "delta", op: 1, text: `${sent % 30 === 0 ? "\n- " : ""}${word(sent)} ` });
        if (sent >= 900 && !committed) {
          committed = true;
          t.emitNotes({ type: "committed", op: 1, revision: 61, block: `\n<!-- 12:00:21 -->\n## Live snapshot\n- ${text(0, 60)}\n` });
        }
        if (el >= 30000) {
          clearInterval(timer);
          done(`1440 segments and a ${st.document.split(/\s+/).length}-word document hydrated; 30 s live: ${Math.floor(el / 500)} open updates, ${Math.floor(el / 5000)} segments, 900 deltas at 60/s, one commit`);
        }
      }, 4);
    },
  };
}
