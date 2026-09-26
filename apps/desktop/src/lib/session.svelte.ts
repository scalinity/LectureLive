// The one session store (spec §9.2): attach first, then hydrate, then apply only what is newer than
// the state, once per animation frame. Runes only; no effects: every update comes from an event.
import { flushSync } from "svelte";
import { idleStatus } from "./fixture";
import type { Transport } from "./transport";
import type { CaptureView, Envelope, FolderView, InputView, LoopbackView, Notice, NotesMsg, PreviewShot, Region, SegmentView, SlideView, SpendSummary, Status, StatusMsg, TranscriptMsg, WindowView } from "./wire";
import { nextWords, type Word } from "./words";

type Stream = "status" | "transcript" | "notes";
type Queued = [Stream, Envelope<StatusMsg | TranscriptMsg | NotesMsg>];

/** Notices kept on screen and across a rehydration. */
const NOTICES = 50;
/** The preview is re-shown at most this often (spec §9.3). */
const PARSE_MS = 100;

/** The text up to its last whitespace: the unfinished trailing word waits for the next delta. */
function wholeWords(text: string): string {
  const i = Math.max(text.lastIndexOf(" "), text.lastIndexOf("\n"), text.lastIndexOf("\t"));
  return i < 0 ? "" : text.slice(0, i + 1);
}

export type Options = { schedule?: (f: (now: number) => void) => void; now?: () => number };

export class Session {
  ready = $state(false);
  status = $state.raw<Status>(idleStatus());
  notices = $state.raw<Notice[]>([]);
  segments = $state.raw<SegmentView[]>([]);
  open = $state.raw<{ utterance: number; words: Word[] } | null>(null);
  revision = $state(0);
  document = $state("");
  /** Blocks committed since `document` was read, in order. */
  committed = $state.raw<string[]>([]);
  /** The preview as last shown: whole words only, re-shown at most every 100 ms. */
  previewShown = $state("");
  slides = $state.raw<SlideView[]>([]);
  /** The live preview; not reactive, since only `previewShown` is drawn. */
  preview: { op: number; text: string } | null = null;
  previewParses = 0;
  /** Hears each frame's work in ms: applying its messages, flushing the DOM, and the panes' layout reads. */
  onDrain: ((ms: number) => void) | null = null;

  private t: Transport | null = null;
  private session = "";
  private lastSeq: Record<Stream, number> = { status: 0, transcript: 0, notes: 0 };
  private queue: Queued[] = [];
  private pending: Queued[] = [];
  private scheduled = false;
  private hydrating: Promise<void> | null = null;
  /** Until the first state is in, every message waits for it. */
  private hydrated = false;
  private hidden = false;
  private previewDirty = false;
  private lastParse = -Infinity;
  private wordIds = 0;
  private frameHooks = new Set<() => void>();
  private disposers: (() => void)[] = [];
  private schedule: (f: (now: number) => void) => void = (f) => requestAnimationFrame(f);
  private now: () => number = () => performance.now();

  get folder(): FolderView | null {
    return this.status.folder;
  }

  /** The stop button's label at each level (spec §9.1): null when there is nothing left to stop. */
  get stopLabel(): "Stop" | "Stop waiting" | null {
    const p = this.status.phase;
    return p === "running" || p === "starting" ? "Stop" : p === "stopping" ? "Stop waiting" : null;
  }

  /** Snapshots and polish are asked for only while the lecture runs: once it stops, the last snapshot takes what is left. */
  get canSnapshot(): boolean {
    return this.status.phase === "running";
  }

  /** Files are being dragged over the window (spec §7.3). */
  dragging = $state(false);

  /** Slide capture: the watched window and its state (spec §7). */
  get capture(): CaptureView {
    return this.status.capture;
  }

  /** Capture works once a real capture has succeeded, while the lecture runs and the window is watched (spec §7.4). */
  get canCapture(): boolean {
    const c = this.status.capture;
    return this.status.phase === "running" && c.captured && c.state === "watching";
  }

  /** A snapshot or a polish is running: the one thing Cancel stops. */
  get busyOp(): boolean {
    const b = this.status.busy ?? "";
    return b.startsWith("snapshot") || b.startsWith("polishing");
  }

  /** Seconds since the lecture started, updated once a second. */
  get elapsed(): number | null {
    const at = this.status.started_at;
    return at ? Math.max(0, Math.floor((this.clockNow - Date.parse(at)) / 1000)) : null;
  }

  /** The last command that failed, in the backend's words; cleared by the next one that succeeds. */
  error = $state<string | null>(null);
  private clockNow = $state(Date.now());

  private async act<T>(cmd: string, args?: Record<string, unknown>): Promise<T | undefined> {
    try {
      const r = await this.t!.call<T>(cmd, args);
      this.error = null;
      return r;
    } catch (e) {
      this.error = String(e);
      return undefined;
    }
  }

  /** The CLI's command line: ⏎ is a snapshot, a hint then ⏎ a hinted one, `polish` ⏎ a polish. */
  async snapshot(hint: string) {
    const h = hint.trim();
    if (h.toLowerCase() === "polish") return this.polish();
    await this.act("snapshot", { hint: h });
  }

  async polish() {
    await this.act("polish");
  }

  async cancel() {
    await this.act("cancel");
  }

  async stop() {
    await this.act("stop");
  }

  async start(source: string) {
    await this.act("start_lecture", { source });
    await this.hydrate();
  }

  async selectFolder(dir: string) {
    await this.act("select_folder", { dir });
    await this.hydrate();
  }

  spendSummary(): Promise<SpendSummary | undefined> {
    return this.act<SpendSummary>("spend_summary");
  }

  async openPage() {
    await this.act("open_page");
  }

  inputs(): Promise<InputView[]> {
    return this.act<InputView[]>("inputs").then((v) => v ?? []);
  }

  loopback(): Promise<LoopbackView | undefined> {
    return this.act<LoopbackView>("loopback_status");
  }

  /** The Capture button: the watched region now, as a manual slide. */
  async captureNow() {
    await this.act("capture_now");
  }

  async importSlides(paths: string[]) {
    await this.act("import_slides", { paths });
  }

  /** The windows the picker offers; undefined when they could not be listed (the error says why). */
  captureWindows(): Promise<WindowView[] | undefined> {
    return this.act<WindowView[]>("capture_windows");
  }

  capturePreview(id: number): Promise<PreviewShot | undefined> {
    return this.act<PreviewShot>("capture_preview", { id });
  }

  /** True when the window and region were saved for this course. */
  async captureSelect(id: number, region: Region): Promise<boolean> {
    await this.act("capture_select", { id, region });
    return this.error === null;
  }

  /** "Watch it": the window the strip asks about, through the course's saved region. */
  async captureWatch(id: number) {
    await this.act("capture_watch", { id });
  }

  async openScreenSettings() {
    await this.act("open_screen_settings");
  }

  keyStatus(): Promise<{ stored: boolean; env_available: boolean } | undefined> {
    return this.act("key_status");
  }

  /** True when the key was stored; the key is not kept anywhere in the frontend. */
  async saveKey(key: string): Promise<boolean> {
    await this.act("save_key", { key });
    return this.error === null;
  }

  async importKey(): Promise<boolean> {
    await this.act("import_key_from_env");
    return this.error === null;
  }

  /** Attaches the listener and both channels, then hydrates; messages meanwhile wait (spec §9.2). */
  async init(t: Transport, opts: Options = {}): Promise<void> {
    this.t = t;
    if (opts.schedule) this.schedule = opts.schedule;
    if (opts.now) this.now = opts.now;
    // Both are registered before the first await, so nothing sent while the state is read is lost.
    const listening = t.listenStatus((m) => this.accept("status", m));
    const attached = t.attach((m) => this.accept("transcript", m), (m) => this.accept("notes", m));
    const drops = t.listenDrops((e) => {
      this.dragging = e.type === "enter" || e.type === "over";
      if (e.type === "drop" && e.paths.length) void this.importSlides(e.paths);
    });
    this.disposers.push(await listening);
    await attached;
    this.disposers.push(await drops);
    const tick = setInterval(() => (this.clockNow = Date.now()), 1000);
    this.disposers.push(() => clearInterval(tick));
    if (typeof document !== "undefined") {
      const onVisibility = () => void this.setHidden(document.hidden);
      document.addEventListener("visibilitychange", onVisibility);
      this.disposers.push(() => document.removeEventListener("visibilitychange", onVisibility));
    }
    await this.hydrate();
    this.ready = true;
  }

  dispose() {
    this.disposers.splice(0).forEach((d) => d());
    this.frameHooks.clear();
  }

  /** Called after each frame's messages are applied: panes keep their scroll here. */
  onFrame(cb: () => void): () => void {
    this.frameHooks.add(cb);
    return () => this.frameHooks.delete(cb);
  }

  /** Replaces everything from `get_session_state`, then replays what arrived meanwhile above its watermark. */
  hydrate(): Promise<void> {
    if (!this.hydrating) {
      this.hydrating = this.readState().finally(() => {
        this.hydrating = null;
        this.hydrated = true;
        const later = this.pending.splice(0);
        later.forEach(([stream, m]) => this.accept(stream, m));
      });
    }
    return this.hydrating;
  }

  private async readState() {
    this.queue = [];
    const st = await this.t!.state();
    this.session = st.session;
    this.lastSeq = { status: st.seq, transcript: st.seq, notes: st.seq };
    this.status = st.status;
    this.notices = st.notices;
    this.segments = st.segments;
    this.open = st.open ? { utterance: st.open.utterance, words: nextWords([], st.open.stable, st.open.tentative, () => ++this.wordIds) } : null;
    this.revision = st.revision;
    this.document = st.document;
    this.committed = [];
    this.slides = st.slides;
    this.preview = st.preview ? { ...st.preview } : null;
    this.previewShown = this.preview ? wholeWords(this.preview.text) : "";
    this.previewDirty = false;
    this.wake(); // the panes' frame hooks run on the new state too
  }

  /** While hidden nothing is applied; on return the store rehydrates (spec §9.2). */
  async setHidden(hidden: boolean): Promise<void> {
    this.hidden = hidden;
    if (hidden) {
      this.queue = [];
      return;
    }
    await this.hydrate();
  }

  private accept(stream: Stream, m: Envelope<StatusMsg | TranscriptMsg | NotesMsg>) {
    if (this.hidden) return;
    if (this.hydrating || !this.hydrated) {
      this.pending.push([stream, m]);
      return;
    }
    if (m.session !== this.session) {
      // A new session (a lecture started, a folder opened) announces itself on the status stream.
      if (stream === "status") void this.hydrate();
      return;
    }
    if (m.seq <= this.lastSeq[stream]) return;
    this.lastSeq[stream] = m.seq;
    this.queue.push([stream, m]);
    this.wake();
  }

  private wake() {
    if (this.scheduled) return;
    this.scheduled = true;
    this.schedule((now) => this.drain(now));
  }

  /** One frame: apply what is queued, re-show the preview when due, then the panes' hooks. */
  drain(now: number) {
    const t0 = performance.now();
    this.scheduled = false;
    const q = this.queue;
    this.queue = [];
    for (const [stream, m] of q) {
      const ok = stream === "status" ? this.applyStatus(m as Envelope<StatusMsg>) : stream === "transcript" ? this.applyTranscript(m as Envelope<TranscriptMsg>) : this.applyNotes(m as Envelope<NotesMsg>);
      if (!ok) {
        void this.hydrate(); // what follows is in the state it reads
        break;
      }
    }
    if (this.previewDirty) {
      if (now - this.lastParse >= PARSE_MS) {
        this.previewShown = wholeWords(this.preview?.text ?? "");
        this.previewDirty = false;
        this.lastParse = now;
        this.previewParses++;
      } else {
        this.wake(); // the last deltas still get shown
      }
    }
    // The DOM first, so the hooks (pin to bottom) measure what is on screen now.
    flushSync();
    this.frameHooks.forEach((h) => h());
    this.onDrain?.(performance.now() - t0);
  }

  private applyStatus(m: Envelope<StatusMsg>): boolean {
    if (m.type === "status") {
      const { type: _t, session: _s, seq: _q, ...status } = m;
      this.status = status;
    } else if (m.type === "notice") {
      const { type: _t, session: _s, seq: _q, ...notice } = m;
      this.notices = [...this.notices, notice].slice(-NOTICES);
    } else {
      const { type: _t, session: _s, seq: _q, ...slide } = m;
      this.slides = [...this.slides, slide];
    }
    return true;
  }

  /** False when a segment id is missing: a notification was lost, so the state is read again. */
  private applyTranscript(m: Envelope<TranscriptMsg>): boolean {
    if (m.type === "open") {
      const same = this.open?.utterance === m.utterance ? this.open.words : [];
      this.open = { utterance: m.utterance, words: nextWords(same, m.stable, m.tentative, () => ++this.wordIds) };
      return true;
    }
    // The live utterance closes even when the state already held its segment.
    if (m.type === "closed" && this.open?.utterance === m.utterance) this.open = null;
    const expected = this.segments.length ? this.segments[this.segments.length - 1].id + 1 : 0;
    if (m.segment.id < expected) return true; // already in the state
    if (m.segment.id > expected) return false;
    this.segments = [...this.segments, m.segment];
    return true;
  }

  /** False on a revision jump or a polish: the document is read again. */
  private applyNotes(m: Envelope<NotesMsg>): boolean {
    switch (m.type) {
      case "delta":
        if (this.preview && m.op < this.preview.op) return true;
        if (!this.preview || this.preview.op !== m.op) this.preview = { op: m.op, text: "" };
        this.preview.text += m.text;
        this.previewDirty = true;
        return true;
      case "committed":
        if (m.revision <= this.revision) {
          this.endPreview(m.op); // the state already held this block; its preview ends all the same
          return true;
        }
        if (m.revision > this.revision + 1) return false;
        this.revision = m.revision;
        this.committed = [...this.committed, m.block];
        this.endPreview(m.op);
        return true;
      case "ended":
        this.endPreview(m.op);
        return true;
      case "polished":
        return m.revision <= this.revision;
    }
  }

  private endPreview(op: number) {
    if (this.preview && this.preview.op <= op) {
      this.preview = null;
      this.previewShown = "";
      this.previewDirty = false;
    }
  }
}

export const session = new Session();

if (import.meta.hot) import.meta.hot.dispose(() => session.dispose());
