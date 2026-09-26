import { describe, expect, test } from "vitest";
import { inRegion, inWindow } from "./parts";
import type { Region } from "./wire";

function near(got: Region | null, want: Region) {
  expect(got).not.toBeNull();
  for (const k of ["x", "y", "w", "h"] as const) expect(got![k]).toBeCloseTo(want[k], 9);
}

describe("parts left out (final review, C1)", () => {
  const region: Region = { x: 0.1, y: 0.2, w: 0.8, h: 0.5 };

  test("a part drawn on the window is kept as fractions of the region, and drawn back where it was", () => {
    const camera: Region = { x: 0.74, y: 0.2, w: 0.16, h: 0.1 };
    const kept = inRegion(camera, region);
    near(kept, { x: 0.8, y: 0, w: 0.2, h: 0.2 });
    near(inWindow(kept!, region), camera);
  });

  test("a part reaching past the region is cut to it, and one outside it is dropped", () => {
    near(inRegion({ x: 0.85, y: 0.15, w: 0.1, h: 0.1 }, region), { x: 0.9375, y: 0, w: 0.0625, h: 0.1 });
    expect(inRegion({ x: 0, y: 0, w: 0.05, h: 0.05 }, region)).toBeNull();
  });
});
