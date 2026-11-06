import { afterEach, expect, test } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import CaptureControls from "./CaptureControls.svelte";
import { FixtureTransport, emptyState } from "./fixture";
import { session } from "./session.svelte";

let view: ReturnType<typeof mount> | undefined;
afterEach(async () => {
  if (view) await unmount(view);
  view = undefined;
  session.dispose();
  document.body.replaceChildren();
});

test("capture controls wait for the acknowledged mode and keep Capture available without an auto frame", async () => {
  const state = emptyState("controls");
  state.status = { ...state.status, phase: "running", capture: { ...state.status.capture, window: "Zoom Meeting", state: "ready" }, capture_mode: { automatic: true, selected: true, shortcut: { state: "inactive" } } };
  const transport = new FixtureTransport(state);
  const frames: ((now: number) => void)[] = [];
  await session.init(transport, { schedule: (f) => frames.push(f) });
  view = mount(CaptureControls, { target: document.body, props: { onChoose: () => {}, showWindow: true } });
  flushSync();
  const button = (text: string) => [...document.querySelectorAll("button")].find((b) => b.textContent === text)!;
  expect(button("Capture").disabled).toBe(false);
  button("Pause auto-capture").click();
  await Promise.resolve();
  expect(transport.calls).toContainEqual(["set_auto_capture", { enabled: false }]);
  expect(button("Pause auto-capture")).toBeDefined();
  transport.emitStatus({ type: "status", ...state.status, capture_mode: { automatic: false, selected: true, shortcut: { state: "needs_permission" } } });
  frames.splice(0).forEach((f) => f(performance.now()));
  flushSync();
  expect(button("Resume auto-capture")).toBeDefined();
  expect(button("Capture").disabled).toBe(false);
  expect(document.body.textContent).toContain("Zoom Meeting");
  expect(document.body.textContent).toContain("Input Monitoring");
  expect(document.body.textContent).toContain("Use Capture or type capture");
  button("Capture").click();
  await Promise.resolve();
  expect(transport.calls).toContainEqual(["capture_now", undefined]);
});

test.each([
  { automatic: true, paused: false, expected: "Looking for a shared slide", absent: "Recording and transcription continue" },
  { automatic: false, paused: false, expected: "Automatic screenshots are paused", absent: "will keep checking" },
  { automatic: true, paused: true, expected: "Lecture paused", absent: "Recording and transcription continue" },
])("discovery respects screenshot and lecture pause: $automatic/$paused", async ({ automatic, paused, expected, absent }) => {
  const state = emptyState("discovery");
  state.status = { ...state.status, phase: "running", paused, capture_mode: { automatic, selected: false, shortcut: { state: "inactive" } }, capture: { state: "asking", window: "Zoom presentation", detail: "Looking for a shared slide", candidates: [], captured: false } };
  await session.init(new FixtureTransport(state));
  view = mount(CaptureControls, { target: document.body, props: { onChoose: () => {}, showWindow: true } });
  flushSync();
  expect(document.querySelector('[role="status"]')?.textContent).toContain(expected);
  expect(document.querySelector('[role="status"]')?.textContent).not.toContain(absent);
  expect([...document.querySelectorAll("button")].find((b) => b.textContent === "Capture")?.disabled).toBe(true);
});

test("compact controls expose permission recovery without starting recording", async () => {
  const state = emptyState("denial");
  state.status = { ...state.status, phase: "running", capture: { ...state.status.capture, state: "denied" } };
  const transport = new FixtureTransport(state);
  await session.init(transport);
  view = mount(CaptureControls, { target: document.body, props: { onChoose: () => {}, showWindow: true } });
  flushSync();
  [...document.querySelectorAll("button")].find((b) => b.textContent === "Open Screen Recording settings")!.click();
  await Promise.resolve();
  expect(transport.calls.some(([name]) => name === "open_screen_settings")).toBe(true);
  expect(transport.calls.some(([name]) => name === "start_lecture")).toBe(false);
});

test("lecture Pause still permits a deliberate screenshot of a selected region", async () => {
  const state = emptyState("paused-selected");
  state.status = { ...state.status, phase: "running", paused: true, capture_mode: { automatic: true, selected: true, shortcut: { state: "inactive" } }, capture: { ...state.status.capture, state: "paused", window: "Zoom Meeting" } };
  await session.init(new FixtureTransport(state));
  view = mount(CaptureControls, { target: document.body, props: { onChoose: () => {} } });
  flushSync();
  expect(document.querySelector('[role="status"]')?.textContent).toContain("automatic screenshots wait");
  expect([...document.querySelectorAll("button")].find((b) => b.textContent === "Capture")?.disabled).toBe(false);
});
