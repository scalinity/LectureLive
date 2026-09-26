import { expect, test } from "vitest";
import { nextWords } from "./words";

const counter = () => { let n = 0; return () => ++n; };
const brief = (ws: { id: number; text: string; stable: boolean }[]) => ws.map((w) => [w.id, w.text, w.stable]);

test("an unchanged prefix keeps its ids, promotion keeps the id, replacements get new ones", () => {
  const fresh = counter();
  const a = nextWords([], "the rate", "sets", fresh);
  expect(brief(a)).toEqual([[1, "the", true], [2, "rate", true], [3, "sets", false]]);
  const b = nextWords(a, "the rate sets", "the step", fresh);
  expect(brief(b)).toEqual([[1, "the", true], [2, "rate", true], [3, "sets", true], [4, "the", false], [5, "step", false]]);
  const c = nextWords(b, "the rate sets", "the size", fresh);
  expect(c.map((w) => w.id)).toEqual([1, 2, 3, 4, 6]);
});

test("a shorter hypothesis drops the words it no longer holds", () => {
  const fresh = counter();
  const a = nextWords([], "", "one two three", fresh);
  expect(nextWords(a, "", "one two", fresh).map((w) => w.id)).toEqual([1, 2]);
});
