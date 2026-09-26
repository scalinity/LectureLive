import { expect, test } from "vitest";
import { percentile } from "./bench";

test("percentiles are nearest-rank over the samples", () => {
  const xs = Array.from({ length: 100 }, (_, i) => i + 1);
  expect([percentile(xs, 50), percentile(xs, 95), percentile(xs, 100)]).toEqual([50, 95, 100]);
  expect(percentile([3], 95)).toBe(3);
});
