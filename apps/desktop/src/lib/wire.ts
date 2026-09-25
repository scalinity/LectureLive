// What the adapter sends (spec §3.6): mirrors apps/desktop/src-tauri/src/wire.rs exactly.

export type Envelope<T> = T & { session: string; seq: number };

export type SegmentView = { id: number; at: string; text: string; recovered: boolean };

export type TranscriptMsg =
  | { type: "open"; utterance: number; stable: string; tentative: string }
  | { type: "closed"; utterance: number; segment: SegmentView }
  | { type: "segment"; segment: SegmentView };

export type Outcome = "nothing_new" | "failed" | "cancelled";

export type NotesMsg =
  | { type: "delta"; op: number; text: string }
  | { type: "committed"; op: number; revision: number; block: string }
  | { type: "ended"; op: number; outcome: Outcome; message: string }
  | { type: "polished"; revision: number };

export type Phase = "idle" | "starting" | "running" | "stopping" | "stopping_now" | "ended";

export type FolderView = { dir: string; course: string; name: string; notes_dir: string; page: string | null };

export type Status = {
  phase: Phase;
  folder: FolderView | null;
  source: string | null;
  level_dbfs: number | null;
  stt: string;
  stt_ok: boolean;
  busy: string | null;
  gaps: number;
  started_at: string | null;
  spend_usd: number;
  silence: boolean;
};

export type NoticeKind = "notes" | "slide" | "page" | "done" | "warn";

export type Notice = { kind: NoticeKind; label: string; detail: string; at: string };

export type SlideView = { index: number; file: string; path: string };

export type StatusMsg = ({ type: "status" } & Status) | ({ type: "notice" } & Notice) | ({ type: "slide" } & SlideView);

export type OpenView = { utterance: number; stable: string; tentative: string };

export type PreviewView = { op: number; text: string };

export type SessionState = {
  session: string;
  seq: number;
  status: Status;
  notices: Notice[];
  segments: SegmentView[];
  open: OpenView | null;
  revision: number;
  document: string;
  preview: PreviewView | null;
  op: number;
  slides: SlideView[];
  pending_segments: number;
  pending_slides: number;
};

export type InputView = { name: string; uid: string };
export type LoopbackView = { present: boolean; blackhole_present: boolean };
