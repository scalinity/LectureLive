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

/** Embeds of each slide in the notes: the last snapshot takes every slide exactly once. */
function embeds(doc: string, n: number): number[] {
  return Array.from({ length: n }, (_, i) => doc.split(`![Slide ${i + 1}](`).length - 1);
}

/** Slide capture in the running app, on a deck window the check opens itself (M5 plan, Task 10): the first
 *  frame and a build captured, a cover that changes nothing, a minimised window that pauses, a replaced
 *  window that asks and captures nothing until chosen, a manual capture and a dropped image; then the
 *  last snapshot embeds each slide once. */
export async function captureCheck(session: Session, t: Transport, dir: string) {
  const steps: Step[] = [];
  const log = (step: string, ok: boolean, detail?: unknown) => steps.push({ at: stamp(), step, ok, detail });
  const deck = (action: string) => t.call<number | null>("check_deck", { action });
  const count = () => session.slides.length;
  const last = () => session.slides.at(-1);
  try {
    await session.selectFolder(dir);
    const id = (await deck("open"))!;
    log("deck window opened", true, { id });
    log("region chosen for the course", await session.captureSelect(id, { x: 0.02, y: 0.06, w: 0.96, h: 0.92 }), session.error);
    await session.start("loopback");
    log("started", session.status.phase === "running", { phase: session.status.phase, error: session.error });
    await until("watching", () => session.capture.state === "watching", 30_000);
    await until("the first slide", () => count() >= 1, 30_000);
    log("watching; the first frame is slide 1, auto", last()?.auto === true && !last()?.uncertain, { capture: session.capture, slide: last() });

    await deck("show:1");
    await until("the build", () => count() >= 2, 15_000);
    log("a build is slide 2, auto", last()?.auto === true, last());

    let n = count();
    await deck("cover");
    await sleep(3000);
    log("covered by the app's window: nothing new", count() === n, { slides: count() });
    await deck("show:3");
    await until("a slide taken while covered", () => count() > n, 15_000);
    log("a change behind the cover is still captured (the window's own buffer)", last()?.auto === true, last());
    await deck("uncover");

    await deck("minimize");
    await until("paused", () => session.capture.state === "paused", 15_000);
    log("minimised: paused", true, session.capture);
    n = count();
    await deck("show:5");
    await sleep(3000);
    log("nothing captured while minimised", count() === n, { slides: count() });
    await deck("unminimize");
    await until("watching again", () => session.capture.state === "watching", 15_000);
    await sleep(4000);
    log("back on screen: the change made meanwhile, once", count() === n + 1, { slides: count(), last: last() });

    n = count();
    const replaced = (await deck("replace"))!;
    await until("asking about the new window", () => session.capture.state === "asking" && session.capture.candidates.some((c) => c.id === replaced), 20_000);
    await sleep(3000);
    log("a replaced window asks, and nothing is captured from it", count() === n, { capture: session.capture, slides: count() });
    await session.captureWatch(replaced);
    await until("watching the new window", () => session.capture.state === "watching", 15_000);
    await deck("show:8");
    await until("a slide from the new window", () => count() > n, 15_000);
    log("after Watch it, the new window is captured", last()?.auto === true, { slides: count(), last: last() });

    n = count();
    await session.captureNow();
    await until("the manual slide", () => count() > n, 15_000);
    log("Capture takes a manual slide", last()?.auto === false, { error: session.error, last: last() });

    n = count();
    await session.importSlides([`${dir}/drop-me.png`]);
    await until("the dropped image", () => count() > n, 15_000);
    log("a dropped image is a manual slide", last()?.auto === false, last());

    const total = count();
    await session.stop();
    await until("the end", () => session.status.phase === "ended", 300_000);
    const st = await t.state();
    const each = embeds(st.document, total);
    log("the last snapshot embeds every slide exactly once", each.every((e) => e === 1), { slides: total, embeds: each, notices: session.notices.slice(-4) });
  } catch (e) {
    log("failed", false, { error: String(e), capture: session.capture, slides: count(), notices: session.notices.slice(-6) });
  }
  await finish(t, "capture", { dir, steps, slides: session.slides });
}

/** The Zoom recording (M5 plan, Task 11): waits for the person to choose Zoom's window, runs a lecture
 *  while the synthetic deck plays through Zoom, and reports every slide with its badges. */
export async function zoomCheck(session: Session, t: Transport, dir: string) {
  const steps: Step[] = [];
  const log = (step: string, ok: boolean, detail?: unknown) => steps.push({ at: stamp(), step, ok, detail });
  try {
    await session.selectFolder(dir);
    await until("Zoom's window to be chosen", () => session.capture.state === "ready", 24 * 3600_000); // the person comes when they can
    log("window chosen", true, session.capture);
    await session.start("loopback");
    log("started", session.status.phase === "running", { phase: session.status.phase, error: session.error });
    await until("watching Zoom", () => session.capture.state === "watching", 60_000);
    log("watching", true, session.capture);
    const end = Date.now() + 30 * 60_000;
    while (Date.now() < end) {
      await sleep(60_000);
      log("minute", true, { slides: session.slides.length, capture: session.capture.state });
    }
    await session.stop();
    await until("the end", () => session.status.phase === "ended", 300_000);
    log("ended", true, session.notices.slice(-4));
  } catch (e) {
    log("failed", false, { error: String(e), capture: session.capture, notices: session.notices.slice(-6) });
  }
  const manual = session.slides.filter((s) => !s.auto);
  await finish(t, "zoom", { dir, steps, slides: session.slides, manual, capture_notices: session.notices.filter((n) => n.kind === "slide" || n.label === "Capture") });
}

/** A recorded lecture played in a window the person chooses, measured locally (M5 plan, Task 11 as
 *  changed): only the detector's input is recorded; no lecture starts, nothing is registered or sent. */
export async function recordCheck(session: Session, t: Transport, dir: string) {
  const steps: Step[] = [];
  const log = (step: string, ok: boolean, detail?: unknown) => steps.push({ at: stamp(), step, ok, detail });
  try {
    await session.selectFolder(dir);
    await until("the window to be chosen", () => session.capture.state === "ready", 24 * 3600_000); // the person comes when they can
    log("window chosen", true, session.capture);
    log("recorded", true, await t.call("check_record", { minutes: 40 }));
  } catch (e) {
    log("failed", false, { error: String(e), capture: session.capture });
  }
  await finish(t, "record", { dir, steps });
}

/** The synthetic deck played in the app's own window and recorded as a fixture, no person needed. */
export async function deckRecordCheck(session: Session, t: Transport, dir: string) {
  const steps: Step[] = [];
  const log = (step: string, ok: boolean, detail?: unknown) => steps.push({ at: stamp(), step, ok, detail });
  try {
    await session.selectFolder(dir);
    const id = (await t.call<number>("check_deck", { action: "open" }))!;
    log("deck window opened", await session.captureSelect(id, { x: 0.02, y: 0.06, w: 0.96, h: 0.92 }), session.error);
    const recording = t.call("check_record", { minutes: 26 });
    await sleep(5000);
    await t.call("check_deck", { action: "start" });
    log("deck started", true);
    log("recorded", true, await recording);
  } catch (e) {
    log("failed", false, { error: String(e) });
  }
  await finish(t, "deck-record", { dir, steps });
}

/** Capture stays on a window that is full screen on its own desktop while the app's window is in front: the
 *  deck goes full screen, the main window comes back, and the deck changes slides where nobody sees it. Only
 *  the detector's input is recorded; nothing is registered or sent. */
export async function fullscreenCheck(session: Session, t: Transport, dir: string) {
  const steps: Step[] = [];
  const log = (step: string, ok: boolean, detail?: unknown) => steps.push({ at: stamp(), step, ok, detail });
  const deck = (action: string) => t.call<unknown>("check_deck", { action });
  try {
    await session.selectFolder(dir);
    const id = (await t.call<number>("check_deck", { action: "open" }))!;
    await deck("show:1");
    // The page number's corner left out: saved through the command as the picker saves it.
    log("deck window opened", await session.captureSelect(id, { x: 0.02, y: 0.06, w: 0.96, h: 0.92 }, [{ x: 0.9, y: 0.9, w: 0.1, h: 0.1 }]), await deck("info"));
    const saved = await session.captureSavedRegion();
    log("the part left out is saved", saved?.leave_out.length === 1, saved);
    const recording = t.call("check_record", { minutes: 2 });
    await sleep(5000);
    await deck("fullscreen");
    await sleep(3000);
    await deck("front");
    await sleep(8000); // the new size holds for a sample, then the slide is searched for
    const away = (await deck("info")) as { on_screen: boolean };
    log("full screen, the app's window in front", !away.on_screen, away); // macOS can decline full screen
    try {
      log("a still for the picker while unseen", true, await t.call("capture_preview", { id }));
    } catch (e) {
      log("a still for the picker while unseen", false, String(e));
    }
    for (const i of [12, 25, 40]) {
      await deck(`show:${i}`);
      await sleep(6000);
      log(`showed ${i} unseen`, true, await deck("info"));
    }
    // A new slide as the window leaves full screen: taken, not settled into the kept frame.
    await deck("show:45");
    await deck("windowed");
    await sleep(8000);
    log("windowed again, showing 45", true, await deck("info"));
    await deck("show:50");
    await sleep(6000);
    log("showed 50", true);
    log("recorded", true, await recording);
  } catch (e) {
    log("failed", false, { error: String(e) });
  }
  await finish(t, "fullscreen", { dir, steps });
}

/** The error table live (M6, Task 10): a loopback lecture on a synthetic folder that records what the person would
 *  see (phase, STT state, gaps, the offer, each notice) while the shell takes the network away, kills the app and
 *  fills the disk. It stops itself after `minutes`; a relaunch in the same folder writes a report of its own. */
export async function faultsCheck(session: Session, t: Transport, dir: string, minutes: number) {
  const name = `faults-${new Date().toISOString().slice(11, 19).replaceAll(":", "")}`;
  const rows: { at: string; phase: string; stt: string; gaps: number; input_gone: string | null; notice?: string }[] = [];
  let seen = "";
  const look = () => {
    const s = session.status;
    const n = session.notices.at(-1);
    const notice = n ? `${n.at} ${n.label}: ${n.detail}` : undefined;
    const key = JSON.stringify([s.phase, s.stt, s.gaps, s.input_gone, notice]);
    if (key !== seen) {
      seen = key;
      rows.push({ at: stamp(), phase: s.phase, stt: s.stt, gaps: s.gaps, input_gone: s.input_gone, notice });
    }
  };
  // Every notice, in full: the rows sample the latest one only every 500 ms.
  const notices = () => session.notices.map((n) => `${n.at} ${n.kind} ${n.label}: ${n.detail}`);
  const write = () => t.call("check_report", { name, json: JSON.stringify({ dir, minutes, error: session.error, rows, notices: notices() }, null, 2) });
  await session.selectFolder(dir);
  await session.start("loopback");
  const end = Date.now() + minutes * 60_000;
  let wrote = Date.now();
  while (Date.now() < end && session.status.phase !== "ended") {
    look();
    if (Date.now() - wrote > 5000) {
      await write();
      wrote = Date.now();
    }
    await sleep(500);
  }
  if (session.status.phase === "running") await session.stop();
  await until("the lecture to end", () => { look(); return session.status.phase === "ended"; }, 600_000).catch(() => {});
  look();
  await finish(t, name, { dir, minutes, error: session.error, rows, notices: notices() });
}
