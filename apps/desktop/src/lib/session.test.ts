import { describe, expect, test } from "vitest";
import { Session } from "./session.svelte";
import { FixtureTransport, emptyState } from "./fixture";

function setup(state = emptyState("s1")) {
  const t = new FixtureTransport(state);
  const s = new Session();
  let clock = 0;
  const frames: ((now: number) => void)[] = [];
  const ready = s.init(t, { schedule: (f) => frames.push(f), now: () => clock });
  const frame = (advance = 16) => { clock += advance; const f = frames.splice(0); f.forEach((g) => g(clock)); };
  return { t, s, ready, frame, tick: (ms: number) => (clock += ms) };
}

describe("session store", () => {
  test("listeners attach before the state is read, and messages at or below its watermark are dropped", async () => {
    const st = emptyState("s1");
    st.seq = 6;
    const { t, s, ready, frame } = setup(st);
    t.holdState(); // the state answer waits until released
    await Promise.resolve();
    t.emitTranscript({ type: "segment", segment: { id: 0, at: "10:00:00", text: "old", recovered: false } }, 5);
    t.emitTranscript({ type: "segment", segment: { id: 0, at: "10:00:00", text: "new", recovered: false } }, 7);
    t.releaseState();
    await ready;
    frame();
    expect(t.order).toEqual(["listen", "attach", "state"]);
    expect(s.segments.map((x) => x.text)).toEqual(["new"]);
  });

  test("a message from another session or a repeated sequence is dropped", async () => {
    const { t, s, ready, frame } = setup();
    await ready;
    t.emitTranscript({ type: "segment", segment: { id: 0, at: "10:00:00", text: "a", recovered: false } });
    t.emitTranscript({ type: "segment", segment: { id: 1, at: "10:00:01", text: "b", recovered: false } }, undefined, "other");
    t.replayLast();
    frame();
    expect(s.segments.map((x) => x.text)).toEqual(["a"]);
  });

  test("a committed block replaces the preview; a revision already held is ignored and a jump rehydrates", async () => {
    const st = emptyState("s1");
    st.revision = 3;
    st.document = "# T\n";
    const { t, s, ready, frame } = setup(st);
    await ready;
    t.emitNotes({ type: "delta", op: 1, text: "## A" });
    frame();
    expect(s.preview?.text).toBe("## A");
    t.emitNotes({ type: "committed", op: 1, revision: 4, block: "\n<!-- 10:00:00 -->\n## A\n" });
    t.emitNotes({ type: "committed", op: 1, revision: 4, block: "\n<!-- 10:00:00 -->\n## A\n" });
    frame();
    expect(s.preview).toBeNull();
    expect(s.committed).toHaveLength(1);
    expect(s.revision).toBe(4);
    const before = t.stateReads;
    t.emitNotes({ type: "committed", op: 2, revision: 6, block: "x" });
    frame();
    await Promise.resolve();
    expect(t.stateReads).toBe(before + 1);
  });

  test("a reload mid-stream resumes the preview and the block lands once", async () => {
    const st = emptyState("s1");
    st.revision = 1;
    st.document = "# T\n";
    st.preview = { op: 2, text: "## Half an" };
    st.op = 2;
    st.seq = 10;
    const { t, s, ready, frame } = setup(st);
    t.setSeq(10);
    await ready;
    t.emitNotes({ type: "delta", op: 2, text: "swer" });
    frame();
    expect(s.preview?.text).toBe("## Half answer");
    t.emitNotes({ type: "committed", op: 2, revision: 2, block: "\n<!-- 10:00:00 -->\n## Half answer\n" });
    frame();
    expect(s.committed).toEqual(["\n<!-- 10:00:00 -->\n## Half answer\n"]);
  });

  test("a hole in segment ids rehydrates", async () => {
    const { t, s, ready, frame } = setup();
    await ready;
    t.emitTranscript({ type: "segment", segment: { id: 0, at: "10:00:00", text: "a", recovered: false } });
    t.emitTranscript({ type: "segment", segment: { id: 2, at: "10:00:02", text: "c", recovered: false } });
    const before = t.stateReads;
    frame();
    await Promise.resolve();
    expect(t.stateReads).toBe(before + 1);
    expect(s.segments.map((x) => x.id)).not.toContain(2);
  });

  test("while hidden nothing is applied; on return the store rehydrates", async () => {
    const { t, s, ready, frame } = setup();
    await ready;
    await s.setHidden(true);
    t.emitNotes({ type: "delta", op: 1, text: "## A" });
    frame();
    expect(s.preview).toBeNull();
    t.state_.preview = { op: 1, text: "## A and more" };
    t.state_.seq = t.lastSeq;
    await s.setHidden(false);
    expect(s.preview?.text).toBe("## A and more");
  });

  test("the preview is parsed at most every 100 ms however many deltas arrive", async () => {
    const { t, s, ready, frame } = setup();
    await ready;
    for (let i = 0; i < 500; i++) {
      t.emitNotes({ type: "delta", op: 1, text: "word " });
      if (i % 8 === 7) frame(16);
    }
    frame(16);
    expect(s.previewParses).toBeLessThanOrEqual(Math.ceil((63 * 16) / 100) + 1);
    expect(s.preview?.text.length).toBe(500 * 5);
  });

  test("an open utterance keeps word ids across updates and a live close clears it", async () => {
    const { t, s, ready, frame } = setup();
    await ready;
    t.emitTranscript({ type: "open", utterance: 1, stable: "the", tentative: "rate" });
    frame();
    const ids = s.open!.words.map((w) => w.id);
    t.emitTranscript({ type: "open", utterance: 1, stable: "the rate", tentative: "sets" });
    frame();
    expect(s.open!.words.slice(0, 2).map((w) => w.id)).toEqual(ids);
    t.emitTranscript({ type: "closed", utterance: 1, segment: { id: 0, at: "10:00:00", text: "the rate sets", recovered: false } });
    frame();
    expect(s.open).toBeNull();
    expect(s.segments.map((x) => x.text)).toEqual(["the rate sets"]);
  });

  test("choosing a region sends its parts left out, and without them the course keeps its own (final review, C1)", async () => {
    const { t, s, ready } = setup();
    await ready;
    const region = { x: 0.1, y: 0.1, w: 0.8, h: 0.8 };
    const camera = [{ x: 0.8, y: 0, w: 0.2, h: 0.2 }];
    t.calls.length = 0;
    await s.captureSelect(42, region, camera);
    await s.captureSelect(42, region);
    expect(t.calls).toEqual([["capture_select", { id: 42, region, leaveOut: camera }], ["capture_select", { id: 42, region }]]);
  });

  test("pause and resume reach the backend, and `pause` and `resume` on the command line are the same commands", async () => {
    const { t, s, ready, frame } = setup();
    await ready;
    t.emitStatus({ type: "status", ...t.state_.status, phase: "running" });
    frame();
    expect(s.canPause).toBe(true);
    await s.pause();
    await s.resume();
    await s.snapshot("pause");
    await s.snapshot(" Resume ");
    expect(t.calls).toEqual([["pause", undefined], ["resume", undefined], ["pause", undefined], ["resume", undefined]]);
  });

  test("a paused lecture is told apart from a running one, counted from the pause, and clear at the end", async () => {
    const { t, s, ready, frame } = setup();
    await ready;
    t.emitStatus({ type: "status", ...t.state_.status, phase: "running" });
    frame();
    expect([s.paused, s.pausedFor]).toEqual([false, null]);
    t.emitStatus({ type: "status", ...t.state_.status, phase: "running", paused: true });
    frame();
    expect(s.paused).toBe(true);
    expect(s.pausedFor).toBe(0);
    expect(s.stopLabel).toBe("Stop");
    expect(s.canSnapshot, "the notes still work while paused").toBe(true);
    t.emitStatus({ type: "status", ...t.state_.status, phase: "running", paused: false });
    frame();
    expect([s.paused, s.pausedFor]).toEqual([false, null]);
    t.emitStatus({ type: "status", ...t.state_.status, phase: "running", paused: true });
    t.emitStatus({ type: "status", ...t.state_.status, phase: "stopping", paused: false });
    frame();
    expect(s.pausedFor).toBeNull();
    expect(s.canPause, "a stopping lecture cannot be paused").toBe(false);
  });

  test("a page reloaded during a pause is told, and counts from the reload", async () => {
    const st = emptyState("s1");
    st.status = { ...st.status, phase: "running", paused: true };
    const { s, ready } = setup(st);
    await ready;
    expect(s.paused).toBe(true);
    expect(s.pausedFor).toBe(0);
  });

  test("a pause is not an error: the No signal that came before it is not shown in it, nor once the signal is back", async () => {
    const { t, s, ready, frame } = setup();
    await ready;
    const notice = (kind: "slide" | "warn" | "notes" | "done", label: string, detail: string) => t.emitStatus({ type: "notice", kind, label, detail, at: "12:00:00" });
    const status = (over: object) => t.emitStatus({ type: "status", ...t.state_.status, phase: "running", ...over });
    status({});
    notice("slide", "Slide 11", "taken automatically, into the next snapshot");
    // The break begins: ten quiet seconds warn, as they should while recording.
    status({ silence: true });
    notice("warn", "No signal", "10 s of silence on BlackHole");
    frame();
    expect(s.shownNotice?.label, "silent while recording: shown").toBe("No signal");
    // The person pauses: the backend clears the flag and adds its own notice, which the line above already says.
    status({ silence: false, paused: true });
    notice("notes", "Paused", "nothing is recorded or transcribed until you resume");
    frame();
    expect(s.shownNotice?.label, "the break is not an error").toBe("Slide 11");
    expect(s.notices.map((n) => n.label), "the history keeps all three").toEqual(["Slide 11", "No signal", "Paused"]);
    // Resumed, and the room is quiet again for ten seconds: a real warning, shown again.
    status({ paused: false });
    notice("done", "Resumed", "recording again, in a new file");
    frame();
    expect(s.shownNotice?.label, "resumed: the old warning does not come back by itself").toBe("Resumed");
    status({ silence: true });
    notice("warn", "No signal", "10 s of silence on BlackHole");
    frame();
    expect(s.shownNotice?.label).toBe("No signal");
    // The signal returns: the warning is no longer true.
    status({ silence: false });
    frame();
    expect(s.shownNotice?.label, "a warning that has passed is not shown").not.toBe("No signal");
  });

  test("a running step is told in words with its clock, and a snapshot waits until its first word streams", async () => {
    const { t, s, ready, frame } = setup();
    await ready;
    const status = (over: object) => t.emitStatus({ type: "status", ...emptyState("s1").status, phase: "running", ...over });
    status({});
    frame();
    expect([s.working, s.busyFor, s.snapshotWaiting], "nothing runs").toEqual([null, null, false]);
    status({ busy: "snapshot, 312 words to claude-opus-5-5" });
    frame();
    expect(s.working).toEqual({ label: "Snapshot", detail: "312 words sent to claude-opus-5-5, thinking" });
    expect([s.busyFor, s.snapshotWaiting, s.busyOp], "the clock starts with the step").toEqual([0, true, true]);
    t.emitNotes({ type: "delta", op: 1, text: "## Gradient descent " });
    frame();
    expect(s.working?.detail, "the first word has streamed").toBe("312 words sent to claude-opus-5-5, writing");
    expect(s.snapshotWaiting).toBe(false);
    status({ busy: "polishing 1420 words" });
    frame();
    expect(s.working).toEqual({ label: "Polish", detail: "rewriting 1420 words" });
    expect(s.snapshotWaiting, "a polish streams nothing and has no snapshot to wait for").toBe(false);
    status({ busy: "distilling lecture_notes.md, a few minutes" });
    frame();
    expect(s.working?.label).toBe("Study page");
    status({ busy: null });
    frame();
    expect([s.working, s.busyFor]).toEqual([null, null]);
  });

  test("the command line maps to the CLI's operations and the stop button to its levels", async () => {
    const { t, s, ready, frame } = setup();
    await ready;
    t.emitStatus({ type: "status", ...t.state_.status, phase: "running" });
    frame();
    expect(s.stopLabel).toBe("Stop");
    await s.snapshot("");
    await s.snapshot("  focus on momentum  ");
    await s.polish();
    await s.cancel();
    await s.stop();
    expect(t.calls).toEqual([["snapshot", { hint: "" }], ["snapshot", { hint: "focus on momentum" }], ["polish", undefined], ["cancel", undefined], ["stop", undefined]]);
    t.emitStatus({ type: "status", ...t.state_.status, phase: "stopping" });
    frame();
    expect(s.stopLabel).toBe("Stop waiting");
    t.emitStatus({ type: "status", ...t.state_.status, phase: "stopping_now" });
    frame();
    expect(s.stopLabel).toBeNull();
  });

  // Final review, I2: a committed message the state already holds still ends its preview.
  test("a committed revision the state already holds still ends its preview", async () => {
    const st = emptyState("s1");
    st.revision = 2;
    st.document = "# T\n\n<!-- 10:00:00 -->\n## A\n";
    st.preview = { op: 2, text: "## A and more " };
    st.op = 2;
    const { t, s, ready, frame } = setup(st);
    await ready;
    t.emitNotes({ type: "committed", op: 2, revision: 2, block: "\n<!-- 10:00:00 -->\n## A\n" });
    frame();
    expect(s.preview).toBeNull();
    expect(s.previewShown).toBe("");
    expect(s.committed).toEqual([]);
  });

  // Final review, M2 (graded Important): a closed segment the state already holds still closes the live line.
  test("a closed segment the state already holds still closes the live utterance", async () => {
    const st = emptyState("s1");
    st.segments = [{ id: 0, at: "10:00:00", text: "the rate sets", recovered: false }];
    st.open = { utterance: 1, stable: "the rate", tentative: "sets" };
    const { t, s, ready, frame } = setup(st);
    await ready;
    t.emitTranscript({ type: "closed", utterance: 1, segment: { id: 0, at: "10:00:00", text: "the rate sets", recovered: false } });
    frame();
    expect(s.open).toBeNull();
    expect(s.segments).toHaveLength(1);
  });

  // Final review, I3: once stopping, the command line takes nothing new (core would drop it).
  test("while stopping, the command line offers no snapshot or polish", async () => {
    const { t, s, ready, frame } = setup();
    await ready;
    t.emitStatus({ type: "status", ...t.state_.status, phase: "running" });
    frame();
    expect(s.canSnapshot).toBe(true);
    t.emitStatus({ type: "status", ...t.state_.status, phase: "stopping" });
    frame();
    expect(s.canSnapshot).toBe(false);
  });

  test("slides keep their badges, capture status arrives whole, and a drop imports its paths", async () => {
    const { t, s, ready, frame } = setup();
    await ready;
    t.emitStatus({ type: "slide", index: 1, file: "slides/slide_01_100251.png", path: "/l/slides/slide_01_100251.png", at: "10:02:51", auto: true, uncertain: false, noted: false });
    t.emitStatus({ type: "status", ...emptyState("s1").status, phase: "running", capture: { state: "watching", window: "Zoom Meeting", detail: null, candidates: [], captured: true } });
    frame();
    expect(s.slides[0].auto).toBe(true);
    expect(s.canCapture).toBe(true);
    t.emitDrop({ type: "enter", paths: ["/Desktop/board.png"] });
    expect(s.dragging).toBe(true);
    t.emitDrop({ type: "drop", paths: ["/Desktop/board.png"] });
    await Promise.resolve();
    expect(s.dragging).toBe(false);
    expect(t.calls.at(-1)).toEqual(["import_slides", { paths: ["/Desktop/board.png"] }]);
  });

  test("a slide can be removed until a snapshot has used it, and what the backend says about slides is applied", async () => {
    const { t, s, ready, frame } = setup();
    await ready;
    const status = (over: object) => t.emitStatus({ type: "status", ...emptyState("s1").status, phase: "running", ...over });
    const slide = (index: number) => ({ type: "slide" as const, index, file: `slides/slide_0${index}_10000${index}.png`, path: `/l/slides/slide_0${index}_10000${index}.png`, at: `10:00:0${index}`, auto: true, uncertain: false, noted: false });
    status({});
    [1, 2, 3].forEach((i) => t.emitStatus(slide(i)));
    frame();
    expect(s.slides.every((x) => s.canRemove(x)), "pending slides of a running lecture").toBe(true);
    await s.removeSlide(2);
    expect(t.calls.at(-1)).toEqual(["remove_slide", { index: 2 }]);
    expect(s.slides.map((x) => x.index), "the slide goes when the backend says so, not before").toEqual([1, 2, 3]);
    t.emitStatus({ type: "slide_removed", index: 2 });
    frame();
    expect(s.slides.map((x) => x.index)).toEqual([1, 3]);
    t.emitStatus({ type: "slides_noted", through: 1 });
    frame();
    expect(s.slides.map((x) => [x.index, x.noted])).toEqual([[1, true], [3, false]]);
    expect(s.canRemove(s.slides[0]), "the notes embed a slide a snapshot used").toBe(false);
    expect(s.canRemove(s.slides[1])).toBe(true);
    status({ busy: "snapshot, 12 words to claude-opus-5-5" });
    frame();
    expect(s.canRemove(s.slides[1]), "its batch may hold the slide").toBe(false);
    status({ busy: "polishing" });
    frame();
    expect(s.canRemove(s.slides[1])).toBe(false);
    status({ busy: null, phase: "stopping" });
    frame();
    expect(s.canRemove(s.slides[1]), "the last snapshot takes what is left").toBe(false);
  });

  test("when the single input goes, the other inputs are offered, and the one chosen is sent", async () => {
    const { t, s, ready, frame } = setup();
    await ready;
    t.answers.inputs = [{ name: "MacBook Pro Microphone", uid: "BuiltInMicrophoneDevice" }, { name: "Wireless Mic Rx", uid: "Rx" }, { name: "BlackHole 2ch", uid: "BlackHole2ch_UID" }];
    t.answers.loopback_status = { present: true, blackhole_present: true, mixed: true };
    t.emitStatus({ type: "status", ...t.state_.status, phase: "running", source: "Wireless Mic Rx", input_gone: "Rx" });
    frame();
    await new Promise((r) => setTimeout(r, 0));
    expect(s.fallbacks).toEqual([{ name: "MacBook Pro Microphone", uid: "BuiltInMicrophoneDevice" }, { name: "Zoom through LectureLive Loopback", uid: "BlackHole2ch_UID" }]);
    await s.useInput("BuiltInMicrophoneDevice");
    expect(t.calls.at(-1)).toEqual(["use_input", { uid: "BuiltInMicrophoneDevice" }]);
  });

  test("the microphone's state is read for the fix-it", async () => {
    const { t, s, ready } = setup();
    await ready;
    t.answers.microphone = "denied";
    expect(await s.microphone()).toBe("denied");
    await s.openMicrophoneSettings();
    expect(t.calls.at(-1)).toEqual(["open_microphone_settings", undefined]);
  });

  test("after the lecture, Polish is offered on the folder's notes while nothing else runs (decision D1)", async () => {
    const st = emptyState("s1");
    st.status = { ...st.status, phase: "ended", folder: { dir: "/L/Week 01", course: "ML", name: "Week 01", notes_dir: "/L/Week 01", page: null } };
    st.document = "# ML — Week 01\n\n## Gradients\n- They point uphill.\n";
    const { t, s, ready, frame } = setup(st);
    await ready;
    expect(s.canPolish).toBe(true);
    await s.polish();
    expect(t.calls.at(-1)).toEqual(["polish", undefined]);
    t.emitStatus({ type: "status", ...t.state_.status, busy: "polishing 9 words" });
    frame();
    expect(s.canPolish).toBe(false);
  });

  test("Stop is not offered while the lecture is starting (M4 minor M6)", async () => {
    const { t, s, ready, frame } = setup();
    await ready;
    t.emitStatus({ type: "status", ...t.state_.status, phase: "starting" });
    frame();
    expect(s.stopLabel).toBeNull();
  });

  test("a state that cannot be read shows as the error, not an unhandled rejection (M4 minor M11)", async () => {
    const t = new FixtureTransport(emptyState("s1"));
    t.failState = "the sidecar could not be read";
    const s = new Session();
    await s.init(t, { schedule: () => {}, now: () => 0 });
    expect(s.error).toContain("the sidecar could not be read");
  });

  test("a reload while the input is gone still offers the other inputs", async () => {
    const st = emptyState("s1");
    st.status = { ...st.status, phase: "running", source: "Wireless Mic Rx", input_gone: "Rx" };
    const t = new FixtureTransport(st);
    t.answers.inputs = [{ name: "MacBook Pro Microphone", uid: "BuiltInMicrophoneDevice" }, { name: "Wireless Mic Rx", uid: "Rx" }];
    t.answers.loopback_status = { present: true, blackhole_present: false, mixed: false };
    const s = new Session();
    await s.init(t, { schedule: () => {}, now: () => 0 });
    await new Promise((r) => setTimeout(r, 0));
    expect(s.fallbacks.map((i) => i.uid)).toEqual(["BuiltInMicrophoneDevice"]);
  });

  test("a command asked before the store is attached waits for it instead of failing", async () => {
    const t = new FixtureTransport(emptyState("s1"));
    t.answers.inputs = [{ name: "MacBook Pro Microphone", uid: "BuiltInMicrophoneDevice" }];
    const s = new Session();
    const early = s.inputs(); // the bar before the lecture asks as it appears, before the page has attached the store
    await s.init(t, { schedule: () => {}, now: () => 0 });
    expect(await early).toEqual([{ name: "MacBook Pro Microphone", uid: "BuiltInMicrophoneDevice" }]);
    expect(s.error).toBeNull();
  });
});
