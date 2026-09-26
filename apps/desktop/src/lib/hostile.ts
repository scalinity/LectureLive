// Hostile Markdown for the in-app sanitiser check (Task 11): the same cases as markdown.test.ts,
// run through the real engine, and the walk that proves the result inert.

export const HOSTILE = [
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

const FORBIDDEN_TAGS = ["SCRIPT", "IFRAME", "FRAME", "OBJECT", "EMBED", "STYLE", "LINK", "META", "BASE", "FORM", "SVG", "MATH", "VIDEO", "AUDIO", "SOURCE", "INPUT", "BUTTON", "TEXTAREA"];
const FORBIDDEN_ATTRS = ["style", "srcset", "srcdoc", "action", "formaction", "poster", "background", "ping", "xlink:href"];
const REMOTE = /^(https?:|\/\/|javascript:|data:|vbscript:|file:)/i;

/** Everything in `root` that could run or load: empty when the render is inert. */
export function problems(root: Element): string[] {
  const out: string[] = [];
  for (const el of Array.from(root.querySelectorAll("*"))) {
    if (FORBIDDEN_TAGS.includes(el.tagName.toUpperCase())) out.push(`tag ${el.tagName}`);
    for (const a of Array.from(el.attributes)) {
      const n = a.name.toLowerCase();
      if (n.startsWith("on") || FORBIDDEN_ATTRS.includes(n)) out.push(`${el.tagName} ${n}`);
      if ((n === "src" || n === "href") && REMOTE.test(a.value)) out.push(`${el.tagName} ${n}=${a.value}`);
    }
  }
  return out;
}
