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
