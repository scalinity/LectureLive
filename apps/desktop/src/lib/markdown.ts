// Rendering the notes (spec §9.3): marked, then DOMPurify (no scripts, frames, handlers), then one
// pass that decides every URL: an image renders only when it is a registered slide, through the
// asset protocol; a link keeps only an in-page anchor. Nothing rendered can reach the network.
import DOMPurify from "dompurify";
import { Marked } from "marked";

export type SlideCtx = {
  notesDir: string;
  /** Absolute paths of the lecture's registered slides. */
  slides: Set<string>;
  toUrl: (abs: string) => string;
};

const parser = new Marked({ gfm: true, async: false });

const FORBID_TAGS = ["script", "iframe", "frame", "frameset", "object", "embed", "style", "link", "meta", "base", "form", "input", "button", "textarea", "select", "video", "audio", "source", "track", "picture", "svg", "math"];
const FORBID_ATTR = ["style", "srcset", "srcdoc", "action", "formaction", "poster", "background", "ping"];
/** Attributes that can name a URL; each is removed unless the pass below keeps it. */
const URL_ATTRS = ["src", "href", "xlink:href", "action", "cite", "data", "longdesc", "usemap", "manifest", "icon"];

/** A path under `base`, or null for anything that is a URL rather than a path. */
export function resolve(base: string, rel: string): string | null {
  if (/^[a-z][a-z0-9+.-]*:/i.test(rel) || rel.startsWith("//")) return null;
  let decoded: string;
  try {
    decoded = decodeURIComponent(rel);
  } catch {
    return null;
  }
  const parts: string[] = [];
  for (const p of (decoded.startsWith("/") ? decoded : `${base}/${decoded}`).split("/")) {
    if (p === "" || p === ".") continue;
    if (p === "..") parts.pop();
    else parts.push(p);
  }
  return "/" + parts.join("/");
}

export function render(md: string, ctx: SlideCtx): string {
  const html = parser.parse(md) as string;
  const frag = DOMPurify.sanitize(html, { USE_PROFILES: { html: true }, FORBID_TAGS, FORBID_ATTR, ALLOW_DATA_ATTR: false, RETURN_DOM_FRAGMENT: true });
  for (const el of Array.from(frag.querySelectorAll("*"))) {
    let keep: string | null = null;
    if (el.tagName === "IMG") {
      const src = el.getAttribute("src");
      const abs = src ? resolve(ctx.notesDir, src) : null;
      if (!abs || !ctx.slides.has(abs)) {
        el.remove();
        continue;
      }
      el.setAttribute("src", ctx.toUrl(abs));
      keep = "src";
    } else if (el.tagName === "A" && el.getAttribute("href")?.startsWith("#")) {
      keep = "href";
    }
    for (const a of URL_ATTRS) if (a !== keep) el.removeAttribute(a);
  }
  const box = document.createElement("div");
  box.appendChild(frag);
  return box.innerHTML;
}

const MARKER = /^<!-- (\d\d:\d\d:\d\d) -->\n?/gm;

/** The document split at its snapshot markers, each part with its time (the title part has none). */
export function chunks(doc: string): { time: string | null; md: string }[] {
  const out: { time: string | null; md: string }[] = [];
  let time: string | null = null;
  let from = 0;
  for (const m of doc.matchAll(MARKER)) {
    if (m.index! > from || time !== null) out.push({ time, md: doc.slice(from, m.index) });
    time = m[1];
    from = m.index! + m[0].length;
  }
  out.push({ time, md: doc.slice(from) });
  return out.filter((c) => c.time !== null || c.md !== "");
}

/** The preview's top-level blocks: all but the last are finished and never change again. */
export function previewBlocks(text: string): string[] {
  const out: string[] = [];
  for (const t of parser.lexer(text)) {
    if (t.type === "space" && out.length) out[out.length - 1] += t.raw;
    else out.push(t.raw);
  }
  return out;
}

/** Markdown or HTML that shows an image: the only chunks a newly registered slide can change. */
export function hasImage(md: string): boolean {
  return /!\[[^\]]*\]\(|<img\b/i.test(md);
}

/** A rendered chunk's cache key: a new slide re-renders only the chunks that hold an image (M4 minor M8). */
export function chunkKey(base: string, part: string, md: string, slides: number): string {
  return hasImage(md) ? `${base}:${part}:s${slides}` : `${base}:${part}`;
}
