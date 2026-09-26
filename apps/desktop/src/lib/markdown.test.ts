import { describe, expect, test } from "vitest";
import { chunkKey, chunks, hasImage, previewBlocks, render, resolve } from "./markdown";

const ctx = { notesDir: "/L/Week 1", slides: new Set(["/L/Week 1/slides/slide_01_100203.png"]), toUrl: (p: string) => "asset://localhost/" + encodeURIComponent(p) };
const REMOTE = /^(https?:|\/\/|javascript:|data:|vbscript:|file:)/i;

function inert(html: string) {
  const root = document.createElement("div");
  root.innerHTML = html;
  for (const el of Array.from(root.querySelectorAll("*"))) {
    expect(["SCRIPT", "IFRAME", "FRAME", "OBJECT", "EMBED", "STYLE", "LINK", "META", "BASE", "FORM", "SVG", "MATH", "VIDEO", "AUDIO", "SOURCE", "INPUT", "BUTTON", "TEXTAREA"]).not.toContain(el.tagName.toUpperCase());
    for (const a of Array.from(el.attributes)) {
      expect(a.name.toLowerCase().startsWith("on"), `${el.tagName} ${a.name}`).toBe(false);
      expect(["style", "srcset", "srcdoc", "action", "formaction", "poster", "background", "ping", "xlink:href"]).not.toContain(a.name.toLowerCase());
      if (a.name === "src" || a.name === "href") expect(a.value, `${el.tagName} ${a.name}`).not.toMatch(REMOTE);
    }
  }
  return root;
}

describe("rendered markdown", () => {
  test("hostile markdown renders inert", () => {
    const hostile = [
      "<script>alert(1)</script>",
      '<img src=x onerror="alert(1)">',
      "![remote](https://evil.example/a.png)",
      "![data](data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=)",
      "[js](javascript:alert(1)) [web](https://evil.example) [proto](//evil.example/x)",
      '<iframe src="https://evil.example"></iframe><object data="https://evil.example"></object><embed src="https://evil.example">',
      '<svg><script>alert(1)</script><image href="https://evil.example/a.png"/></svg><math><mi>x</mi></math>',
      '<style>@import url(https://evil.example/a.css);</style><link rel="stylesheet" href="https://evil.example/a.css">',
      '<div style="background:url(https://evil.example/a.png)">x</div>',
      '<meta http-equiv="refresh" content="0;url=https://evil.example"><base href="https://evil.example/">',
      '<form action="https://evil.example"><button formaction="https://evil.example">go</button><input value=1></form>',
      '<video src="https://evil.example/v.mp4" poster="https://evil.example/p.png"></video><img srcset="https://evil.example/a.png 1x">',
      "![Slide 1](../../../etc/passwd.png) ![Slide 2](slides/slide_02_999999.png)",
      '<a href="https://evil.example" ping="https://evil.example">x</a><details open ontoggle="alert(1)">d</details>',
    ].join("\n\n");
    const root = inert(render(hostile, ctx));
    expect(root.querySelectorAll("img")).toHaveLength(0);
  });

  test("a registered slide renders through the asset protocol and keeps its alt text", () => {
    const root = inert(render("## Rates\n\n![Slide 1](slides/slide_01_100203.png)\n\n- a [link](#rates)", ctx));
    const img = root.querySelector("img")!;
    expect(img.getAttribute("src")).toBe(ctx.toUrl("/L/Week 1/slides/slide_01_100203.png"));
    expect(img.getAttribute("alt")).toBe("Slide 1");
    expect(root.querySelector("a")!.getAttribute("href")).toBe("#rates");
    expect(root.querySelector("h2")!.textContent).toBe("Rates");
  });

  test("the document splits at its snapshot markers and keeps their times", () => {
    const doc = "# T\n\n<!-- 10:02:03 -->\n## A\n- a\n\n<!-- 10:07:40 -->\n## B\n";
    expect(chunks(doc)).toEqual([{ time: null, md: "# T\n\n" }, { time: "10:02:03", md: "## A\n- a\n\n" }, { time: "10:07:40", md: "## B\n" }]);
  });

  test("the preview splits into finished blocks and the one still growing", () => {
    expect(previewBlocks("## A\n- one\n- two\n\nSome par")).toEqual(["## A\n", "- one\n- two\n\n", "Some par"]);
  });

  test("paths resolve under the notes folder and URLs do not resolve", () => {
    expect(resolve("/L/Week 1", "slides/a.png")).toBe("/L/Week 1/slides/a.png");
    expect(resolve("/L/Week 1", "../x/../Week 1/slides/a.png")).toBe("/L/Week 1/slides/a.png");
    expect(resolve("/L/Week 1", "slides/slide%2001.png")).toBe("/L/Week 1/slides/slide 01.png");
    for (const u of ["https://a/b.png", "//a/b.png", "data:x", "asset://localhost/x"]) expect(resolve("/L", u)).toBeNull();
  });

  test("a new slide changes the key of chunks with an image only (M4 minor M8)", () => {
    const text = "## Momentum\n- keeps rolling\n";
    const img = "## Chart\n![Slide 1](slides/slide_01_100251.png)\n";
    expect(chunkKey("r2", "d1", text, 1)).toBe(chunkKey("r2", "d1", text, 2));
    expect(chunkKey("r2", "d2", img, 1)).not.toBe(chunkKey("r2", "d2", img, 2));
    expect(hasImage('<img src="slides/a.png">')).toBe(true);
  });
});
