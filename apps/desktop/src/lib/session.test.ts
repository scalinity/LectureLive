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
});
