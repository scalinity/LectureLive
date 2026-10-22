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
