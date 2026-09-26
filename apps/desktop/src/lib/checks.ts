// In-app checks for the M4 gate, run in the real webview against the real backend (Task 11):
// `csp` probes the policy and the slide scope; `live` runs a lecture with hint, cancel, reload,
// a hidden window and polish. Each writes a report through `check_report`, then quits the app.
import { render } from "./markdown";
import { HOSTILE, problems } from "./hostile";
import type { Session } from "./session.svelte";
import type { Transport } from "./transport";

type Step = { at: string; step: string; ok: boolean; detail?: unknown };
const STATE_KEY = "lecturelive.m4.live";

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const stamp = () => new Date().toISOString().slice(11, 23);

async function until(what: string, test: () => boolean, ms = 120_000): Promise<void> {
  const end = Date.now() + ms;
  while (!test()) {
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
    await sleep(100);
  }
}

async function finish(t: Transport, name: string, report: unknown) {
  await t.call("check_report", { name, json: JSON.stringify(report, null, 2) });
  await t.call("exit_app");
}

/** The policy (remote image, inline handler, remote fetch), hostile Markdown, and the asset scope. */
export async function cspCheck(session: Session, t: Transport, dir: string) {
  const violations: { directive: string; blocked: string }[] = [];
  document.addEventListener("securitypolicyviolation", (e) => violations.push({ directive: e.effectiveDirective || e.violatedDirective, blocked: e.blockedURI }));
  const box = document.createElement("div");
  box.hidden = true;
  document.body.appendChild(box);
  // (a) a remote image; 192.0.2.1 is TEST-NET-1, so nothing leaves the machine even if the policy failed.
  const remote = document.createElement("img");
  remote.src = "http://192.0.2.1/m4.png";
  box.appendChild(remote);
  // (b) an inline handler written without the sanitiser: the page's script-src has a nonce and no unsafe-inline.
  const raw = document.createElement("div");
  raw.innerHTML = '<img src="/m4-missing.png" onerror="window.__m4 = 1">';
  box.appendChild(raw);
  // (c) a remote request.
  let fetched = "no";
  try {
    await fetch("http://192.0.2.1/m4");
    fetched = "yes";
  } catch (e) {
    fetched = `refused: ${e}`;
  }
  // (d) hostile Markdown through the real renderer and DOMPurify, in WebKit.
  await session.selectFolder(dir);
  const slides = new Set(session.slides.map((s) => s.path));
  const ctx = { notesDir: session.folder?.notes_dir ?? dir, slides, toUrl: (p: string) => t.assetUrl(p) };
  const probe = document.createElement("div");
  probe.innerHTML = render(HOSTILE, ctx);
  box.appendChild(probe);
  // (e) the asset scope: a registered slide loads, a file beside the slides folder does not.
  const load = (src: string) =>
    new Promise<string>((r) => {
      const img = new Image();
      img.onload = () => r("loaded");
      img.onerror = () => r("refused");
      img.src = src;
      setTimeout(() => r("timeout"), 5000);
    });
  const slide = session.slides[0]?.path ?? `${dir}/slides/missing.png`;
  const inside = await load(t.assetUrl(slide));
  const outside = await load(t.assetUrl(`${dir}/outside.png`));
  await sleep(3000);
  const report = {
    violations,
    inline_handler_ran: (window as { __m4?: number }).__m4 !== undefined,
    remote_fetch: fetched,
    hostile_render_problems: problems(probe),
    hostile_images: probe.querySelectorAll("img").length,
    slide_inside_scope: { path: slide, result: inside },
    file_outside_scope: { path: `${dir}/outside.png`, result: outside },
  };
  await finish(t, "csp", report);
}

type LiveState = { steps: Step[]; phase: string; before?: { segments: number[]; notes: string; revision: number }; visibility: string[] };

function load(): LiveState {
  try {
    return JSON.parse(sessionStorage.getItem(STATE_KEY) ?? "") as LiveState;
  } catch {
    return { steps: [], phase: "start", visibility: [] };
  }
}

function save(s: LiveState) {
  sessionStorage.setItem(STATE_KEY, JSON.stringify(s));
}

/** What the panes show: segment ids from the transcript pane, the notes pane's text. */
function view() {
  const segments = Array.from(document.querySelectorAll("section[aria-label=Transcript] p.said")).length;
  const notes = document.querySelector("section[aria-label=Notes]")?.textContent ?? "";
  return { segments, notes };
}

/** A lecture on a synthetic folder, with speech played into BlackHole by the person running the check. */
export async function liveCheck(session: Session, t: Transport, dir: string) {
  const s = load();
  const log = (step: string, ok: boolean, detail?: unknown) => {
    s.steps.push({ at: stamp(), step, ok, detail });
    save(s);
  };
  try {
    if (s.phase === "start") {
      log("key moved into the Keychain from .env", await session.importKey(), session.error);
      await session.selectFolder(dir);
      await session.start("loopback");
      log("started", session.status.phase === "running", { phase: session.status.phase, error: session.error });
      const first = session.segments.length;
      await until("three segments", () => session.segments.length >= first + 3, 180_000);
      log("three segments transcribed", true, session.segments.slice(-3).map((x) => x.text));

      let rev = session.revision;
      await session.snapshot("focus on the learning rate");
      await until("the hinted snapshot", () => session.revision > rev);
      log("hinted snapshot committed", session.revision === rev + 1, { revision: session.revision, notice: session.notices.at(-1) });

      rev = session.revision;
      // Speech that arrives after the commit, so the next snapshot has something new to write.
      let seen = session.segments.length;
      await until("new speech for the next snapshot", () => session.segments.length > seen, 120_000);
      await session.snapshot("");
      await until("the preview to start", () => session.previewShown.length > 0 || session.preview !== null, 180_000);
      await session.cancel();
      await until("the cancel", () => session.notices.some((n) => n.label === "Cancelled"), 30_000);
      await sleep(1000);
      log("cancel mid-stream wrote nothing", session.revision === rev, { revision: session.revision, notice: session.notices.at(-1), preview: session.previewShown.slice(0, 60) });

      await session.snapshot("");
      await until("the snapshot after cancel", () => session.revision > rev);
      log("snapshot after cancel committed", session.revision === rev + 1, { revision: session.revision });

      seen = session.segments.length;
      await until("more speech", () => session.segments.length > seen, 120_000);
      s.before = { segments: session.segments.map((x) => x.id), notes: view().notes, revision: session.revision };
      s.phase = "reloaded";
      save(s);
      location.reload();
      return;
    }

    if (s.phase === "reloaded") {
      await until("hydration", () => session.ready);
      await sleep(1500);
      const st = await t.state();
      const v = view();
      const kept = s.before!.segments.every((id) => session.segments.some((x) => x.id === id));
      log("reload restored the full view", kept && session.revision === st.revision && v.segments === st.segments.length && v.notes.includes(s.before!.notes.slice(0, 200)), {
        segments_before: s.before!.segments.length,
        segments_after: session.segments.length,
        state_segments: st.segments.length,
        pane_segments: v.segments,
        revision_before: s.before!.revision,
        revision_after: session.revision,
      });

      document.addEventListener("visibilitychange", () => {
        s.visibility.push(`${stamp()} ${document.visibilityState}`);
        save(s);
      });
      const rev = session.revision;
      const heard = session.segments.length;
      await until("new speech since the reload", () => session.segments.length > heard, 120_000);
      await session.snapshot("");
      await until("a preview to hide during", () => session.preview !== null, 180_000);
      await t.call("hide_window_for", { ms: 4000 });
      await sleep(2500);
      const st2 = await t.state();
      const v2 = view();
      log("hidden window: rehydrated on return", v2.segments === st2.segments.length && session.revision === st2.revision, {
        visibility_events: s.visibility,
        pane_segments: v2.segments,
        state_segments: st2.segments.length,
        revision: session.revision,
        state_revision: st2.revision,
        revision_before_hide: rev,
      });

      await until("the snapshot begun before hiding", () => session.revision > rev || session.notices.some((n) => n.label === "Snapshot" || n.label === "Snapshot failed"));
      const before = session.revision;
      await session.polish();
      await until("the polish", () => session.notices.some((n) => n.label === "Polished" || n.label.startsWith("Polish")), 300_000);
      await sleep(1500);
      const st3 = await t.state();
      log("polish re-fetched the document", session.notices.some((n) => n.label === "Polished") && session.revision === st3.revision && session.revision > before && session.document === st3.document, {
        revision_before: before,
        revision_after: session.revision,
        notice: session.notices.filter((n) => n.label.startsWith("Polish")),
      });

      await session.stop();
      await until("stopping", () => session.status.phase === "stopping" || session.status.phase === "ended", 10_000);
      log("first stop: stopping", true, session.status.phase);
      await sleep(1500);
      if (session.status.phase === "stopping") await session.stop();
      // The phase arrives as a status event after the command returns.
      await until("the second stop's phase", () => ["stopping_now", "ended"].includes(session.status.phase), 10_000).catch(() => {});
      log("second stop: stop waiting", ["stopping_now", "ended"].includes(session.status.phase), session.status.phase);
      await until("the end", () => session.status.phase === "ended", 300_000);
      log("ended", true, session.notices.slice(-4));
      s.phase = "done";
      save(s);
    }
  } catch (e) {
    log("failed", false, { error: String(e), phase: session.status.phase, busy: session.status.busy, notices: session.notices.slice(-6), segments: session.segments.length, revision: session.revision });
  }
  sessionStorage.removeItem(STATE_KEY);
  await finish(t, "live", { dir, steps: s.steps, visibility: s.visibility });
}

/** The study page from the folder's notes (spec §6.4, §9.1): typeset or re-rendered, timed. */
export async function pageCheck(session: Session, t: Transport, dir: string) {
  await session.selectFolder(dir);
  const t0 = Date.now();
  const path = await t.call<string>("open_page").catch((e) => `failed: ${e}`);
  await finish(t, "page", { dir, path, seconds: Math.round((Date.now() - t0) / 1000), notices: session.notices.slice(-3) });
}
