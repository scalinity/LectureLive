// How the store reaches the backend: Tauri in the app, a fixture in tests, the bench and the
// browser preview.
import { Channel, convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { Envelope, NotesMsg, SessionState, StatusMsg, TranscriptMsg } from "./wire";

export type DropEvent = { type: "enter" | "over" | "drop" | "leave"; paths: string[] };

export interface Transport {
  listenStatus(cb: (m: Envelope<StatusMsg>) => void): Promise<() => void>;
  /** Hands the backend this page's two ordered streams, replacing any earlier page's. */
  attach(onTranscript: (m: Envelope<TranscriptMsg>) => void, onNotes: (m: Envelope<NotesMsg>) => void): Promise<void>;
  state(): Promise<SessionState>;
  call<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T>;
  /** Files dragged over the window and dropped on it (spec §7.3): the native event, which carries their paths. */
  listenDrops(cb: (e: DropEvent) => void): Promise<() => void>;
  /** An absolute path under an allowed folder, as a URL the webview can load. */
  assetUrl(path: string): string;
}

export function tauriTransport(): Transport {
  return {
    listenStatus: (cb) => listen<Envelope<StatusMsg>>("status", (e) => cb(e.payload)),
    attach: async (onTranscript, onNotes) => {
      const transcript = new Channel<Envelope<TranscriptMsg>>(onTranscript);
      const notes = new Channel<Envelope<NotesMsg>>(onNotes);
      await invoke("attach", { transcript, notes });
    },
    state: () => invoke<SessionState>("get_session_state"),
    call: <T>(cmd: string, args?: Record<string, unknown>) => invoke<T>(cmd, args),
    assetUrl: (path) => convertFileSrc(path),
    listenDrops: (cb) => getCurrentWebview().onDragDropEvent((e) => cb({ type: e.payload.type, paths: "paths" in e.payload ? e.payload.paths : [] })),
  };
}
