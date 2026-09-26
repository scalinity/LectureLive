// A scripted backend: the store's tests, the frame-time bench and the browser preview drive the real
// store through it, with the adapter's session and sequence rules.
import type { Transport } from "./transport";
import type { Envelope, NotesMsg, SessionState, Status, StatusMsg, TranscriptMsg } from "./wire";

export function idleStatus(): Status {
  return { phase: "idle", folder: null, source: null, level_dbfs: null, stt: "not started", stt_ok: false, busy: null, gaps: 0, started_at: null, spend_usd: 0, silence: false };
}

export function emptyState(session: string): SessionState {
  return { session, seq: 0, status: idleStatus(), notices: [], segments: [], open: null, revision: 0, document: "", preview: null, op: 1, slides: [], pending_segments: 0, pending_slides: 0 };
}

type Handler<T> = (m: Envelope<T>) => void;

export class FixtureTransport implements Transport {
  state_: SessionState;
  lastSeq: number;
  calls: [string, unknown][] = [];
  /** The order the store reached the backend in. */
  order: string[] = [];
  stateReads = 0;
  private held: (() => void)[] | null = null;
  private last: (() => void) | null = null;
  private onStatus: Handler<StatusMsg>[] = [];
  private onTranscript: Handler<TranscriptMsg> | null = null;
  private onNotes: Handler<NotesMsg> | null = null;

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
    return undefined as T;
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
  t.state_.slides = [{ index: 1, file: "slides/slide_01_100251.png", path: `${DEMO_DIR}/slides/slide_01_100251.png` }];
  t.assetUrl = () => "/demo-slide.svg";
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
