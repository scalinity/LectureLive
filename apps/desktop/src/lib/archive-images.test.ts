import { afterEach, expect, test } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import Notes from "./Notes.svelte";
import SlidesStrip from "./SlidesStrip.svelte";
import { emptyState, FixtureTransport } from "./fixture";
import { session } from "./session.svelte";

const views: ReturnType<typeof mount>[] = [];
afterEach(async () => {
  for (const view of views.splice(0)) await unmount(view);
  session.dispose();
  document.body.replaceChildren();
});

test("notes and thumbnails switch to the HTML image after cleanup without changing the notes", async () => {
  const state = emptyState("archive");
  state.status.folder = { dir: "/lecture", notes_dir: "/lecture", course: "Test", name: "Week 1", page: "/lecture/Test.html" };
  state.document = "# Lecture\n\n![Slide 1](slides/slide_01_100000.png)";
  state.slides = [{ index: 1, file: "slides/slide_01_100000.png", path: "/lecture/slides/slide_01_100000.png", at: "10:00:00", auto: true, uncertain: false, noted: true }];
  const transport = new FixtureTransport(state);
  await session.init(transport, { schedule: () => {} });
  session.drain(performance.now());
  const toUrl = () => "asset://localhost/slide.png";
  views.push(mount(Notes, { target: document.body, props: { toUrl } }));
  views.push(mount(SlidesStrip, { target: document.body, props: { toUrl, onChoose: () => {} } }));
  flushSync();
  expect([...document.querySelectorAll("img")].map((el) => el.getAttribute("src"))).toEqual([toUrl(), toUrl()]);
  const uri = "data:image/jpeg;base64,/9j/2Q==";
  transport.state_.slides[0].embedded = uri;
  transport.emitStatus({ type: "slides_archived" });
  transport.state_.seq = transport.lastSeq;
  session.drain(performance.now());
  await session.hydrate();
  session.drain(performance.now());
  flushSync();
  expect([...document.querySelectorAll("img")].map((el) => el.getAttribute("src"))).toEqual([uri, uri]);
  expect(session.document).toBe(state.document);
});
