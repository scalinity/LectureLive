import { afterEach, expect, test } from "vitest";
import { mount, unmount } from "svelte";
import SlideUndo from "./SlideUndo.svelte";
import { emptyState, FixtureTransport } from "./fixture";
import { session } from "./session.svelte";

let view: ReturnType<typeof mount> | undefined;
afterEach(async () => {
  if (view) await unmount(view);
  view = undefined;
  session.dispose();
  document.body.replaceChildren();
});

async function setup() {
  const state = emptyState("undo");
  state.status.phase = "running";
  state.removed_slides = 2;
  const transport = new FixtureTransport(state);
  await session.init(transport, { schedule: () => {} });
  session.drain(performance.now());
  view = mount(SlideUndo, { target: document.body });
  return { transport, frame: () => session.drain(performance.now()) };
}

function key(target: EventTarget = window, extra: KeyboardEventInit = {}) {
  const event = new KeyboardEvent("keydown", { key: "z", metaKey: true, bubbles: true, cancelable: true, ...extra });
  target.dispatchEvent(event);
  return event;
}

test("Command-Z restores one image per press and restored slides return in order", async () => {
  const { transport, frame } = await setup();
  const slide = (index: number) => ({ index, file: `slides/${index}.png`, path: `/lecture/slides/${index}.png`, at: "10:00:00", auto: true, uncertain: true, noted: false });
  transport.emitStatus({ type: "slide", ...slide(3) });
  frame();
  expect(key().defaultPrevented).toBe(true);
  await new Promise((resolve) => setTimeout(resolve, 0));
  await Promise.resolve();
  expect(transport.calls.at(-1)).toEqual(["undo_remove_slide", undefined]);
  expect(session.slides.map((s) => s.index), "only the backend acknowledges restoration").toEqual([3]);
  transport.emitStatus({ type: "slide_restored", ...slide(2) });
  frame();
  key();
  transport.emitStatus({ type: "slide_restored", ...slide(1) });
  frame();
  expect(session.slides.map((s) => s.index)).toEqual([1, 2, 3]);
  expect(session.slides.every((s) => s.auto && s.uncertain)).toBe(true);
  expect(session.removedSlides).toBe(0);
  expect(key().defaultPrevented).toBe(false);
  expect(transport.calls.filter(([c]) => c === "undo_remove_slide")).toHaveLength(2);
});

test("typing fields and open dialogs retain normal undo", async () => {
  const { transport } = await setup();
  for (const html of ['<input>', '<textarea></textarea>', '<select></select>', '<div contenteditable="true"><span>Text</span></div>', '<div role="textbox"><span>Text</span></div>']) {
    document.body.insertAdjacentHTML("beforeend", html);
    const target = document.body.lastElementChild!;
    expect(key(target.lastElementChild ?? target).defaultPrevented).toBe(false);
  }
  document.body.insertAdjacentHTML("beforeend", '<dialog open><button>Dialog</button></dialog>');
  expect(key().defaultPrevented).toBe(false);
  expect(transport.calls).toHaveLength(0);
});

test("redo, alternate chords, held keys and cancelled events do not undo a slide", async () => {
  const { transport } = await setup();
  for (const extra of [{ shiftKey: true }, { altKey: true }, { ctrlKey: true }, { metaKey: false }, { repeat: true }, { isComposing: true }, { key: "x" }]) expect(key(window, extra).defaultPrevented).toBe(false);
  const event = new KeyboardEvent("keydown", { key: "z", metaKey: true, cancelable: true });
  event.preventDefault();
  window.dispatchEvent(event);
  expect(transport.calls).toHaveLength(0);
});

test("undo remains available after class and reload, and waits for a snapshot or stopping", async () => {
  const { transport, frame } = await setup();
  transport.emitStatus({ type: "status", ...transport.state_.status, phase: "ended" });
  frame();
  expect(key().defaultPrevented).toBe(true);
  for (const over of [{ phase: "stopping" as const }, { phase: "running" as const, busy: "snapshot, 30 words" }, { phase: "ended" as const, busy: "polishing" }]) {
    transport.emitStatus({ type: "status", ...transport.state_.status, ...over });
    frame();
    expect(key().defaultPrevented).toBe(false);
  }
  expect(transport.calls.filter(([c]) => c === "undo_remove_slide")).toHaveLength(1);
});

test("a removal in flight can be undone immediately and a failed undo keeps the history", async () => {
  const { transport } = await setup();
  session.removedSlides = 0;
  const removing = session.removeSlide(3);
  expect(key().defaultPrevented).toBe(true);
  await removing;
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(transport.calls.map(([c]) => c)).toEqual(["remove_slide", "undo_remove_slide"]);
  session.removedSlides = 1;
  transport.effects.undo_remove_slide = () => { throw new Error("The removed slide's file is missing"); };
  await session.undoRemoval();
  expect(session.removedSlides).toBe(1);
  expect(session.error).toContain("missing");
});
