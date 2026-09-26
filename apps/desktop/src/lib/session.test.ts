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
    t.emitStatus({ type: "slide", index: 1, file: "slides/slide_01_100251.png", path: "/l/slides/slide_01_100251.png", at: "10:02:51", auto: true, uncertain: false });
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
});
