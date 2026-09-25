import { describe, expect, test } from "vitest";
import { FixtureTransport, emptyState } from "./fixture";

describe("fixture transport", () => {
  test("stamps one rising sequence across streams and records calls", async () => {
    const t = new FixtureTransport(emptyState("s1"));
    const seen: number[] = [];
    await t.listenStatus((m) => seen.push(m.seq));
    await t.attach((m) => seen.push(m.seq), (m) => seen.push(m.seq));
    t.emitTranscript({ type: "open", utterance: 1, stable: "the", tentative: "rate" });
    t.emitNotes({ type: "delta", op: 1, text: "## A" });
    t.emitStatus({ type: "notice", kind: "done", label: "saved", detail: "", at: "10:00:00" });
    expect(seen).toEqual([1, 2, 3]);
    await t.call("snapshot", { hint: "focus" });
    expect(t.calls).toEqual([["snapshot", { hint: "focus" }]]);
    expect((await t.state()).session).toBe("s1");
  });
});
