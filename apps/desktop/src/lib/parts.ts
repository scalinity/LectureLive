// Parts of the slide region left out (spec §7.1): drawn on the window's still, kept as fractions of the
// region, so they stay on the camera they cover when the region is redrawn.
import type { Region } from "./wire";

/** `part` (fractions of the window) as fractions of `region`, cut to it; null when it lies outside. */
export function inRegion(part: Region, region: Region): Region | null {
  const x0 = Math.max(part.x, region.x);
  const y0 = Math.max(part.y, region.y);
  const x1 = Math.min(part.x + part.w, region.x + region.w);
  const y1 = Math.min(part.y + part.h, region.y + region.h);
  if (x1 <= x0 || y1 <= y0) return null;
  return { x: (x0 - region.x) / region.w, y: (y0 - region.y) / region.h, w: (x1 - x0) / region.w, h: (y1 - y0) / region.h };
}

/** A part kept as fractions of `region`, as fractions of the window again. */
export function inWindow(part: Region, region: Region): Region {
  return { x: region.x + part.x * region.w, y: region.y + part.y * region.h, w: part.w * region.w, h: part.h * region.h };
}
