"""Live lecture transcription (Grok Voice Transcribe) with snapshot notes (grok-4.7).

Run inside the lecture's folder (`lecture --help` lists everything):
  lecture         record; resumes if today's notes are already there
  lecture page    typeset today's study page from the notes, without recording
  lecture spend   what the tool has cost, by month, course and lecture
While recording: Enter takes a snapshot, a hint + Enter adds a focus hint, polish + Enter
rewrites the notes into a study document and its HTML page, Ctrl+C stops.

Writes into the current directory: one notes document per day that grows across
snapshots and restarts, its HTML page named after the lecture, a timestamped transcript,
and slides/. Screenshots taken
while running (or images dropped into slides/) are placed in the timeline at the
moment they were captured. Bookkeeping and polish backups live in .live_notes/.
Every paid call is logged in spend.jsonl next to this file.

Configuration comes from .env next to this file (see .env.example).
"""

import argparse
import base64
import codecs
import collections
import hashlib
import io
import json
import os
import queue
import re
import shutil
import subprocess
import sys
import tempfile
import termios
import threading
import time
import tty
import wave
from datetime import datetime
from html import escape, unescape
from pathlib import Path

import httpx
import numpy as np
import sounddevice as sd

API = "https://api.x.ai/v1"
NOTES_MODEL = "grok-4.7"
SAMPLE_RATE = 16000
BLOCK_SECONDS = 0.5
MIN_CHUNK_SECONDS = 8
MAX_CHUNK_SECONDS = 30
# a block this far below the chunk's average loudness is treated as a pause
QUIET_RATIO = 0.35
SLIDE_MAX_PX = 1600
IMAGE_SUFFIXES = {".png", ".jpg", ".jpeg"}
DOC_CONTEXT_CHARS = 40000

# slides embedded in the page: under half the PNG's size, with slide subscripts still sharp
EMBED_JPEG_QUALITY = 80
PAGE_EFFORT = "medium"
# the footer's audio trace covers this many blocks; exact digital silence this long means no signal
TRACE_BLOCKS = 24
NO_SIGNAL_SECONDS = 15

TICKS_PER_USD = 10**10
# published rates, used only when a response does not report what it was billed
STT_USD_PER_SECOND = 0.10 / 3600
CHAT_USD_PER_TOKEN = {"input": 2.20e-6, "cached": 0.55e-6, "output": 6.60e-6}
LONG_PROMPT_TOKENS = 200_000  # above this grok-4.7 bills the whole request at twice the rate

HERE = Path(__file__).resolve().parent
HTML_TEMPLATE = HERE / "notes_template.html"
SPEND_LOG = HERE / "spend.jsonl"
LINE_RE = re.compile(r"^\[(\d\d:\d\d:\d\d)\] (.+)$")
SLIDE_RE = re.compile(r"^slide_(\d+)_(\d{6})\.(png|jpe?g)$", re.I)
EMBED_RE = re.compile(r"!\[Slide \d+\]\([^)]+\)")
SLIDE_EMBED_RE = re.compile(r"!\[Slide (\d+)\]\(([^)]+)\)")
# the template owns all styling and behaviour; generated fragments may not bring their own
UNSAFE_RE = re.compile(r"(?is)<(script|style)\b.*?</\1\s*>|\s(?:on\w+|style)\s*=\s*(?:\"[^\"]*\"|'[^']*'|[^\s>]+)")


def notes_system(course):
    return f"""You are the note-taker for a lecture in the course "{course}".
You maintain ONE running Markdown notes document for today's lecture. Each call you receive the document so far plus new material: a timestamped speech-to-text transcript and slide screenshots marked at the moment they were shown. You write only the new notes to append.
Rules:
- Continue the existing heading structure: `##` for topics, `###` for subtopics. If the new material continues the last section, continue it without repeating its heading. Never add a document title or "snapshot" headings.
- Concise bullets: key points, definitions, formulas, examples, and anything the lecturer emphasised or flagged as examinable.
- Slides are the authority on terminology, formulas and figures; fix obvious speech-recognition errors from context. Do not add material that was not said or shown.
- Words after a ">>> Slide N shown" marker were said while that slide was up. Every embed line you are given MUST appear in your output exactly once, verbatim, on its own line: directly under the heading whose content the slide illustrates, followed by one line saying what the slide shows. If nothing in the new material relates to a slide, put it under its own `### Slide N` heading with that one-line description.
- Do not repeat what the document already says; add only what is new.
- Output Markdown only: no preamble, no code fences."""


def polish_system(course):
    return f"""You turn raw, incrementally written lecture notes from the course "{course}" into one clean study document.
You receive the notes as written during the lecture and the full transcript. Produce the complete replacement document in Markdown:
- Title line, then a 3-5 sentence summary of the lecture.
- Sections by topic in lecture order (`##` / `###`), merging duplicates and removing snapshot or session headings.
- Keep every substantive point, definition, formula and example; use the transcript to fill gaps and fix speech-recognition errors. Do not invent content.
- Keep every `![Slide N](...)` embed line exactly once, verbatim, next to the content it illustrates.
- End with "Key takeaways", a short "Glossary" of terms introduced, and "Questions / follow-ups" if anything is unclear.
Output Markdown only: no preamble, no code fences."""


def page_system(course, budget, max_slides):
    return f"""You make the high-yield study page for a lecture in the course "{course}". You receive the lecture's complete notes and its slide screenshots. The notes are long; the page must not be. Its reader is a student revising for a quiz or exam who wants everything that will matter and nothing that will not. Condensing is the whole job: a short page that keeps what matters beats a complete one.

Budget: at most {budget} words of visible text in total, counting the summary, every section, the glossary, the takeaways and the questions with their answers; math counts as words. Spend about {round(budget * 0.65)} on the topic sections, {round(budget * 0.15)} on the glossary and takeaways, and {round(budget * 0.2)} on the questions. This is a ceiling, not a target.

Choose before you write. What earns space, in order:
1. What the lecturer flagged as examinable or stressed, and what a quiz is likely to test.
2. The formulas and decision rules needed to solve problems: what each method is for, when to use it rather than its alternatives, and its assumptions.
3. At most one worked example per method, cut to the steps that carry the method, with the lecture's own numbers.
4. Terms introduced in this lecture.
Cut: reviews of earlier weeks beyond a line, digressions, logistics, repetition, the lecturer's asides and hedges, step-by-step software walkthroughs (keep only which function does what), and anything you would not put on a one-page exam sheet. Merge overlapping topics. Prefer a table, a situation-to-method map or a short list over prose; one idea per bullet; no sentence that restates another.

Slides: redraw at most {max_slides}, choosing those whose figure, table, diagram or formula block teaches faster than words, and redraw only the part that matters, not the slide's text. Text-only and title slides are never redrawn; their substance, if it earns space, goes in the text. Copy numbers and symbols exactly.

Output exactly this, in order, and nothing else: no code fences, no <script> or <style>, no style or event attributes.
<div class="keystone">…</div>: the single formula (\\[ … \\]) or idea (<p>) to remember if nothing else.
<p class="lede">…</p>: two or three sentences on what the lecture covered and what it lets you do.
4 to 7 <section class="part"> blocks, one per topic, each opening with an <h2>.
<section class="part"><h2>Glossary</h2><dl class="glossary">…</dl></section>: at most 12 terms, one line each.
<section class="part"><h2>Key takeaways</h2><ul class="takeaways">…</ul></section>: at most 5.
<ol class="quiz">…</ol>: 4 or 5 questions, each <p class="q">…</p><div class="answer">…</div>, answers brief with the key working.

Markup inside sections: <h3>, <p>, <ul>, <ol>, <li>, <strong>, <em>, and <table> with <thead> and <tbody>. Math is TeX, \\( … \\) inline and \\[ … \\] displayed, never Unicode symbols or entities and never inside <code>. R code goes in <pre><code>, function names in <code>.
- A formula to memorise: <div class="formula" data-name="short plain-text name">\\[ … \\]</div>
- A point flagged as examinable, at most 4 on the page: <aside class="flag">…</aside>
- A worked example: <div class="example"><h4>its title</h4><p>the setup</p><ol class="steps"><li>one step each, the last stating the result</li></ol></div>
- Situations mapped to methods: <dl class="map"><dt>situation</dt><dd>method</dd></dl>
- A slide redraw: <figure class="redraw" data-slide="N">…<figcaption>one sentence</figcaption></figure> holding display math, a <table> (cells of a highlighted group class="a", a second group class="b"), or one inline <svg viewBox="0 0 640 H"> without width or height attributes.
SVG: no fill, stroke, color, style or font-family attributes; draw only with these classes. Lines and text are ink by default; "muted" for axes, grid lines and secondary labels; "a" and "b" for two series or groups; "area-a" and "area-b" for shaded regions such as rejection regions; "dot" for filled points; "thick" and "dash" for emphasis and reference lines. Text is <text> with a font-size from 13 to 18, kept inside the viewBox."""


def revise_system(budget):
    return f"""You shorten a study page that is over its length budget. Rewrite it to at most {budget} words of visible text, math counting as words, by cutting the lowest-yield material first: second examples, secondary detail, lesser glossary terms, extra questions, restatements. Keep its order, its markup and components exactly as they are used, every formula a problem needs, and every figure you keep unchanged. Output the complete page in the same format and nothing else."""


def hms(ts):
    return datetime.fromtimestamp(ts).strftime("%H:%M:%S")


def write_atomic(path, text):
    tmp = path.with_name(path.name + ".tmp")
    with open(tmp, "w") as f:
        f.write(text)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)


# ---- terminal ---------------------------------------------------------------

COLOR = sys.stdout.isatty() and "NO_COLOR" not in os.environ
TRUECOLOR = os.environ.get("COLORTERM") in ("truecolor", "24bit")
STYLES = {
    "bold": "1",
    "dim": "2",
    # the study page's signal red and teal, so the tool and its page read as one thing
    "red": "38;2;242;118;107" if TRUECOLOR else "31",
    "teal": "38;2;93;184;192" if TRUECOLOR else "36",
}
MARKS = {"slide": ("▣", "teal"), "notes": ("◆", "teal"), "page": ("✦", "teal"), "done": ("✓", "teal"), "warn": ("▲", "red")}
TRACE = "▁▂▃▄▅▆▇█"
SPINNER = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
KEY_HINTS = "⏎ snapshot   a hint ⏎   polish ⏎   ^C stop"


def paint(text, *styles):
    if not COLOR or not styles or not text:
        return text
    return f"\x1b[{';'.join(STYLES[s] for s in styles)}m{text}\x1b[0m"


def fit(parts, width):
    """Join (text, *styles) parts, dropping whole parts from the end so the line never wraps."""
    out, used = [], 0
    for text, *styles in parts:
        if used + len(text) > width:
            break
        out.append(paint(text, *styles))
        used += len(text)
    return "".join(out)


class Screen:
    """Output scrolls above a two-line footer pinned to the bottom: live status, then the line being typed."""

    def __init__(self):
        self.lock = threading.RLock()
        self.active = False
        self.shown = False
        self.typed = ""
        self.status = None
        self.busy = {}
        self.saved = None

    def start(self, status, on_line):
        if not (sys.stdin.isatty() and sys.stdout.isatty()):
            threading.Thread(target=lambda: [on_line(line.rstrip("\n")) for line in sys.stdin], daemon=True).start()
            return
        fd = sys.stdin.fileno()
        self.saved = termios.tcgetattr(fd)
        # keys arrive one at a time and the footer echoes them, so a status redraw never eats typing
        tty.setcbreak(fd)
        self.status, self.active = status, True
        threading.Thread(target=self._read_keys, args=(fd, on_line), daemon=True).start()
        threading.Thread(target=self._tick, daemon=True).start()
        self.refresh()

    def stop(self):
        with self.lock:
            self._erase()
            self.active = False
            if self.saved:
                termios.tcsetattr(sys.stdin.fileno(), termios.TCSADRAIN, self.saved)
                self.saved = None
            sys.stdout.flush()

    def write(self, text):
        with self.lock:
            self._erase()
            print(text)
            self._draw()
            sys.stdout.flush()

    def refresh(self):
        with self.lock:
            self._erase()
            self._draw()
            sys.stdout.flush()

    def _erase(self):
        if self.shown:
            sys.stdout.write("\r\x1b[2K\x1b[1A\x1b[2K")
            self.shown = False

    def _draw(self):
        if not self.active:
            return
        width = shutil.get_terminal_size().columns - 1
        typed = [(self.typed[-(width - 3):],)] if self.typed else [(KEY_HINTS, "dim")]
        sys.stdout.write(self.status(width) + "\n" + fit([(" › ", "teal"), *typed], width))
        self.shown = True

    def _tick(self):
        while self.active:
            time.sleep(0.25)
            self.refresh()

    def _read_keys(self, fd, on_line):
        decode = codecs.getincrementaldecoder("utf-8")(errors="ignore").decode
        escape = ""
        while data := os.read(fd, 64):
            for ch in decode(data):
                if ch == "\x1b" or escape:
                    # arrow keys and the like arrive as escape sequences and have no use here
                    escape += ch
                    if len(escape) == 2 and ch not in "[O" or len(escape) > 2 and "@" <= ch <= "~":
                        escape = ""
                    continue
                line = None
                with self.lock:
                    if ch == "\n":
                        line, self.typed = self.typed, ""
                    elif ch in "\x7f\b":
                        self.typed = self.typed[:-1]
                    elif ch == "\x15":
                        self.typed = ""
                    elif ch.isprintable():
                        self.typed += ch
                    self.refresh()
                if line is not None:
                    on_line(line)


screen = Screen()


def say(kind, label, detail=""):
    """One event line: its mark, what it is, and what happened."""
    mark, colour = MARKS[kind]
    screen.write(f"  {paint(mark, colour)} {paint(label, 'bold')}  {detail}".rstrip())


def progress(key, text=None):
    """Long work shows in the footer while recording, or on one updating line otherwise."""
    if screen.active:
        if text:
            screen.busy[key] = text
        else:
            screen.busy.pop(key, None)
        screen.refresh()
    elif sys.stdout.isatty():
        sys.stdout.write(f"\r\x1b[2K  {paint(MARKS['page'][0], 'teal')} {text}" if text else "\r\x1b[2K")
        sys.stdout.flush()


# ---- spend ------------------------------------------------------------------


class Spend:
    """Every paid call, one JSON line each in spend.jsonl, with running totals for the footer."""

    def __init__(self):
        self.lock = threading.Lock()
        self.context = {}
        self.lecture = 0.0
        self.by_kind = collections.defaultdict(float)

    def open(self, course, lecture):
        self.context = {"course": course, "lecture": lecture}
        today = datetime.now().strftime("%Y-%m-%d")
        self.lecture = sum(
            e["usd"] for e in read_spend()
            if e.get("course") == course and e.get("lecture") == lecture and e["at"].startswith(today)
        )

    def add(self, what, usd, billed, **extra):
        entry = {"at": datetime.now().isoformat(timespec="seconds"), **self.context,
                 "what": what, "usd": round(usd, 6), "billed": billed, **extra}
        with self.lock:
            self.lecture += usd
            self.by_kind[what] += usd
            with open(SPEND_LOG, "a") as f:
                f.write(json.dumps(entry) + "\n")


spend = Spend()


def read_spend():
    if not SPEND_LOG.exists():
        return []
    entries = []
    for line in SPEND_LOG.read_text().splitlines():
        try:
            entries.append(json.loads(line))
        except ValueError:
            pass  # a line cut short by a crash
    return entries


def reported_cost(body):
    """The billed cost when xAI reports it with the response, else None."""
    ticks = (body.get("usage") or {}).get("cost_in_usd_ticks")
    return None if ticks is None else ticks / TICKS_PER_USD


def chat_cost(body):
    """What a chat call cost, and whether that is xAI's billed figure or worked out from the rates."""
    billed = reported_cost(body)
    if billed is not None:
        return billed, True
    usage = body.get("usage") or {}
    prompt = usage.get("prompt_tokens", 0)
    cached = (usage.get("prompt_tokens_details") or {}).get("cached_tokens", 0)
    rate = CHAT_USD_PER_TOKEN
    usd = (prompt - cached) * rate["input"] + cached * rate["cached"] + usage.get("completion_tokens", 0) * rate["output"]
    return usd * (2 if prompt > LONG_PROMPT_TOKENS else 1), False


def money(usd):
    return "<$0.01" if 0 < usd < 0.005 else f"${usd:,.2f}"


def bar(fraction, width):
    eighths = round(max(0.0, min(1.0, fraction)) * width * 8)
    return ("█" * (eighths // 8) + " ▏▎▍▌▋▊▉"[eighths % 8]).rstrip().ljust(width)


def show_spend():
    entries = read_spend()
    if not entries:
        print(f"\n  Nothing spent yet. Every paid call is logged in {SPEND_LOG} from the next run.\n")
        return
    width = min(shutil.get_terminal_size().columns, 92) - 2
    months, lectures = {}, {}
    for e in entries:
        course = e.get("course", "?")
        by_course = months.setdefault(e["at"][:7], {})
        by_course[course] = by_course.get(course, 0.0) + e["usd"]
        kinds = lectures.setdefault((e["at"][:10], course, e.get("lecture", "?")), {})
        kinds[e["what"]] = kinds.get(e["what"], 0.0) + e["usd"]
    total = sum(e["usd"] for e in entries)
    estimated = sum(e["usd"] for e in entries if not e.get("billed"))
    name_w = min(34, max(len(c) for m in months.values() for c in m))

    def row(left, right, left_style=(), right_style=(), fill=""):
        # every amount ends at the same right edge; fill (a bar) sits between name and amount
        room = width - len(left) - len(fill) - len(right)
        print(paint(left, *left_style) + paint(fill, "teal") + paint(right.rjust(room + len(right)), *right_style))

    print()
    row("  Spend", f"all time {money(total)}", ("bold",), ("dim",))
    for month in sorted(months)[-3:]:
        courses = months[month]
        print()
        row("  " + datetime.strptime(month, "%Y-%m").strftime("%B %Y"), money(sum(courses.values())), ("bold",))
        top = max(courses.values())
        for course, usd in sorted(courses.items(), key=lambda kv: -kv[1]):
            row(f"    {course[:name_w]:<{name_w}}  ", money(usd), fill=bar(usd / top, 20))
    print()
    row("  Recent lectures", "", ("bold",))
    for (day, course, lecture), kinds in sorted(lectures.items(), reverse=True)[:8]:
        when = datetime.strptime(day, "%Y-%m-%d").strftime("%-d %b")
        amount = money(sum(kinds.values()))
        row(f"    {when:<7} " + f"{course}  ›  {lecture}"[:width - 22], amount)
        parts = "   ".join(f"{k} {money(v)}" for k, v in sorted(kinds.items(), key=lambda kv: -kv[1]))
        print(paint(f"            {parts}", "dim"))
    print()
    share = round(100 * estimated / total) if total else 0
    note = "all billed by xAI" if not estimated else f"{share}% estimated from published rates, the rest billed by xAI"
    print(paint(f"  {len(entries)} paid calls; {note}.", "dim"))
    print()


def initial_state(old, notes_existed, transcript_path, slides):
    data = transcript_path.read_bytes()
    if "noted_through" in old:
        # one-time migration from the old timestamp checkpoint
        cut = old["noted_through"]
        offset, pos = len(data), 0
        for raw in data.splitlines(keepends=True):
            m = LINE_RE.match(raw.decode(errors="replace").rstrip("\n"))
            if m and m.group(1) >= cut:
                offset = pos
                break
            pos += len(raw)
        slide_index = max([s[1] for s in slides if s[0] < cut], default=0)
    elif notes_existed:
        offset, slide_index = len(data), max([s[1] for s in slides], default=0)
    else:
        offset, slide_index = 0, 0
    return {"transcript_offset": offset, "slide_index": slide_index}


def recover_commit(state, notes_path):
    """Finish or undo a snapshot append that was interrupted by a crash."""
    c = state.pop("commit", None)
    if not c:
        return
    block = c["block"].encode()
    data = notes_path.read_bytes()
    before, tail = c["before"], data[c["before"]:]
    if len(data) >= before and tail == block:
        state["transcript_offset"], state["slide_index"] = c["transcript_offset"], c["slide_index"]
        say("done", "recovered", "the last snapshot was fully written")
    elif len(data) > before and block.startswith(tail):
        os.truncate(notes_path, before)
        say("warn", "recovered", "removed a half-written snapshot; its material is queued again")
    elif len(data) != before:
        say("warn", "recovered", "the notes changed during an interrupted snapshot; left as they are, material queued again")


def load_env():
    values = {}
    env_file = HERE / ".env"
    if env_file.exists():
        for line in env_file.read_text().splitlines():
            if "=" in line and not line.lstrip().startswith("#"):
                k, v = line.split("=", 1)
                values[k.strip()] = v.strip().strip('"').strip("'")
    values.update({k: os.environ[k] for k in ("GROK_API_KEY", "LECTURE_COURSE", "LECTURE_DEVICE") if k in os.environ})
    if not values.get("GROK_API_KEY"):
        sys.exit(f"GROK_API_KEY is not set. Put it in {env_file} (see .env.example).")
    return values


def course_from_path(path):
    """The course folder in <course>/Weeks/<lecture>, so each course names itself."""
    parts = path.parts
    for i in range(len(parts) - 1, 0, -1):
        if parts[i] == "Weeks":
            return parts[i - 1]
    return None


def to_wav(audio):
    pcm = (np.clip(audio, -1, 1) * 32767).astype(np.int16)
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(SAMPLE_RATE)
        w.writeframes(pcm.tobytes())
    return buf.getvalue()


def transcribe(client, key, wav, seconds, keyterms):
    data = {"language": "en", "format": "true"}
    if keyterms:
        data["keyterm"] = keyterms
    tries = failures = 0
    while True:
        tries += 1
        delay = min(2 * tries, 20)
        try:
            r = client.post(
                f"{API}/stt",
                headers={"Authorization": f"Bearer {key}"},
                files={"file": ("chunk.wav", wav, "audio/wav")},
                data=data,
                timeout=60,
            )
            if r.status_code == 200:
                body = r.json()
                billed = reported_cost(body)
                spend.add("transcribe", seconds * STT_USD_PER_SECOND if billed is None else billed,
                          billed is not None, audio_s=round(seconds, 1))
                return body.get("text", "").strip()
            say("warn", f"transcribe {r.status_code}", r.text[:160])
            # rate limits and server errors pass; any other status fails the same way every time
            transient = r.status_code == 429 or r.status_code >= 500
        except httpx.TransportError as e:
            # connection down: later chunks queue behind this one until it is back
            say("warn", "offline", f"{e}; holding the audio, retrying in {delay}s")
            transient = True
        except httpx.HTTPError as e:
            say("warn", "transcribe", str(e))
            transient = False
        if not transient:
            failures += 1
            if failures == 3:
                say("warn", "transcribe", "gave up on one chunk after it was rejected 3 times")
                return ""
        time.sleep(delay)


def screenshot_dir():
    out = subprocess.run(
        ["defaults", "read", "com.apple.screencapture", "location"],
        capture_output=True, text=True,
    )
    p = Path(out.stdout.strip()).expanduser()
    return p if out.returncode == 0 and p.is_dir() else Path.home() / "Desktop"


def shrink_if_large(path):
    out = subprocess.run(
        ["sips", "-g", "pixelWidth", "-g", "pixelHeight", str(path)],
        capture_output=True, text=True,
    ).stdout
    dims = [int(line.split(":")[1]) for line in out.splitlines() if "pixel" in line]
    # sips -Z scales up as well as down, so only call it when there is something to shrink
    if dims and max(dims) > SLIDE_MAX_PX:
        subprocess.run(["sips", "-Z", str(SLIDE_MAX_PX), str(path)], capture_output=True)


def image_part(path):
    mime = "image/jpeg" if path.suffix.lower() in (".jpg", ".jpeg") else "image/png"
    b64 = base64.b64encode(path.read_bytes()).decode()
    return {"type": "image_url", "image_url": {"url": f"data:{mime};base64,{b64}", "detail": "high"}}


def existing_slides(slides_dir):
    out = []
    for p in slides_dir.iterdir():
        m = SLIDE_RE.match(p.name)
        if m:
            t = m.group(2)
            out.append((f"{t[:2]}:{t[2:4]}:{t[4:]}", int(m.group(1)), p.resolve()))
    return sorted(out)


def embed_md(slide, notes_dir):
    return f"![Slide {slide[1]}]({os.path.relpath(slide[2], notes_dir)})"


def timeline(segments, slides, notes_dir):
    events = [(s[0], f"[{s[0]}] {s[1]}") for s in segments]
    events += [(s[0], f"[{s[0]}] >>> Slide {s[1]} shown (embed: {embed_md(s, notes_dir)})") for s in slides]
    return "\n".join(line for _, line in sorted(events))


def clean_output(text, strip_title):
    text = text.strip()
    if text.startswith("```"):
        lines = text.splitlines()[1:]
        if lines and lines[-1].strip().startswith("```"):
            lines = lines[:-1]
        text = "\n".join(lines).strip()
    while strip_title and text.startswith("# "):
        text = text.split("\n", 1)[1].lstrip() if "\n" in text else ""
    return text


def chat(client, key, system, content, what, effort=None, timeout=600):
    body = {"model": NOTES_MODEL, "messages": [
        {"role": "system", "content": system},
        {"role": "user", "content": content},
    ]}
    if effort:
        body["reasoning_effort"] = effort
    r = client.post(
        f"{API}/chat/completions",
        headers={"Authorization": f"Bearer {key}"},
        json=body,
        timeout=timeout,
    )
    r.raise_for_status()
    body = r.json()
    spend.add(what, *chat_cost(body))
    return body["choices"][0]["message"]["content"]


def make_notes(client, key, course, doc, segments, slides, hint, notes_dir):
    doc_tail = doc[-DOC_CONTEXT_CHARS:]
    if len(doc) > DOC_CONTEXT_CHARS:
        doc_tail = "[... earlier part of the document omitted ...]\n" + doc_tail
    user = f"Notes document so far:\n<<<\n{doc_tail}\n>>>\n\n"
    user += "New material since the last snapshot, in chronological order:\n<<<\n"
    user += timeline(segments, slides, notes_dir) + "\n>>>\n\n"
    if slides:
        user += "Slide images are attached in the same order as their markers. These embed lines are mandatory, each exactly once, verbatim:\n"
        user += "\n".join(embed_md(s, notes_dir) for s in slides) + "\n\n"
    if hint:
        user += f"Focus hint from the student: {hint}\n\n"
    user += "Write only the new notes to append."
    content = [{"type": "text", "text": user}] + [image_part(s[2]) for s in slides]
    return clean_output(chat(client, key, notes_system(course), content, "notes"), strip_title=True)


def polish_notes(client, key, course, doc, transcript, title):
    user = (
        f"Use this exact title line: {title}\n\n"
        f"Notes as written during the lecture:\n<<<\n{doc}\n>>>\n\n"
        f"Full transcript:\n<<<\n{transcript}\n>>>\n\n"
        "Write the complete replacement document."
    )
    return clean_output(chat(client, key, polish_system(course), user, "polish"), strip_title=False)


def notes_words(doc):
    return len(SLIDE_EMBED_RE.sub("", doc).split())


def page_budget(doc):
    """Visible words the study page may use: 30% of the notes. A page that kept everything ran to
    1.7 times its notes, so this is under a fifth of that."""
    return max(600, min(2500, round(notes_words(doc) * 0.3 / 50) * 50))


def visible_words(html):
    """Words a reader sees, the measure the budget is checked against; drawings are not counted."""
    return len(unescape(re.sub(r"(?is)<svg\b.*?</svg>|<[^>]+>", " ", html)).split())


def typeset(client, key, system, content):
    # medium effort: at the default (high) one call over a whole lecture ran past 10 minutes;
    # choosing against stated rules and a budget needs judgment, not the deepest deliberation
    call = dict(effort=PAGE_EFFORT, timeout=1200)
    # one retry: a failed call would otherwise cost the whole page
    try:
        out = chat(client, key, system, content, "page", **call)
    except httpx.HTTPError:
        out = chat(client, key, system, content, "page", **call)
    return UNSAFE_RE.sub("", clean_output(out, strip_title=False))


def typeset_page(client, key, notes_path, doc, system, budget):
    """One call over the whole lecture, so it can rank what matters across all of it, and one more
    to cut if the draft overshoots: word counts are checked, not trusted."""
    embeds = SLIDE_EMBED_RE.findall(doc)
    user = f"The complete notes ({notes_words(doc)} words):\n<<<\n{doc}\n>>>\n\n"
    if embeds:
        user += "Slide images are attached in this order: " + ", ".join(f"Slide {n}" for n, _ in embeds) + ".\n\n"
    user += f"Write the study page in at most {budget} words."
    content = [{"type": "text", "text": user}] + [image_part(notes_path.parent / p) for _, p in embeds]
    progress("page", f"distilling {notes_words(doc):,} words into at most {budget:,}")
    try:
        out = typeset(client, key, system, content)
        words = visible_words(out)
        if words > budget * 1.1:
            progress("page", f"cutting the draft from {words:,} words to {budget:,}")
            revise = f"The page, {words} words:\n<<<\n{out}\n>>>\n\nWrite it in at most {budget} words."
            out = typeset(client, key, revise_system(budget), revise)
            words = visible_words(out)
    finally:
        progress("page")
    keystone = re.search(r'(?s)<div class="keystone">.*?</div>', out)
    lede = re.search(r'(?s)<p class="lede">.*?</p>', out)
    quiz = re.search(r'(?s)<ol class="quiz">.*</ol>', out)
    body = out
    for m in (keystone, lede, quiz):
        if m:
            body = body.replace(m.group(0), "")
    # re-cut at every <h2>, so the page's structure never depends on the model's own wrappers
    body = re.sub(r"(?i)</?section\b[^>]*>", "", body)
    sections = [f'<section class="part">\n{c.strip()}\n</section>' for c in re.split(r"(?i)(?=<h2[\s>])", body) if c.strip()]
    if quiz:
        sections.append(f'<section class="part check">\n<h2>Check yourself</h2>\n{quiz.group(0)}\n</section>')
    fills = {
        "keystone": keystone.group(0) if keystone else "",
        "summary": lede.group(0) if lede else "",
        "content": "\n".join(sections),
        "slides": json.dumps(dict(embeds)).replace("</", "<\\/"),
    }
    return fills, words


def page_path(notes_path, lecture_name):
    """The page is named after the lecture, e.g. "Statistical Analysis Methods.html"; the date is
    added only when the folder holds more than one day's notes, so two lectures never share a page."""
    title = lecture_name.partition(" — ")[2] or lecture_name
    name = re.sub(r"[/:]", "-", title).strip() or notes_path.stem
    stamp = re.search(r"\d{8}", notes_path.stem)
    if stamp and len(list(notes_path.parent.glob("lecture_notes_*.md"))) > 1:
        name += f" ({datetime.strptime(stamp.group(0), '%Y%m%d'):%-d %b})"
    return notes_path.parent / f"{name}.html"


def slide_uri(path):
    """A slide as a JPEG data URI, or None if the file is gone. The page carries its own slides
    because a browser handed only the page may not read the slides/ folder beside it."""
    if not path.exists():
        return None
    with tempfile.TemporaryDirectory() as tmp:
        jpeg = Path(tmp) / "slide.jpg"
        subprocess.run(
            ["sips", "-s", "format", "jpeg", "-s", "formatOptions", str(EMBED_JPEG_QUALITY), str(path), "--out", str(jpeg)],
            capture_output=True,
        )
        data, mime = (jpeg.read_bytes(), "image/jpeg") if jpeg.exists() else (path.read_bytes(), "image/png")
    return f"data:{mime};base64,{base64.b64encode(data).decode()}"


def render_html(client, key, course, notes_path, lecture_name):
    doc = notes_path.read_text()
    budget = page_budget(doc)
    slides = len(SLIDE_EMBED_RE.findall(doc))
    system = page_system(course, budget, max(2, min(8, round(slides * 0.3))))
    # the typeset page is kept against the notes and the prompt that made it: a change to either
    # typesets again, while a change to the design alone re-renders for free
    cache = notes_path.parent / ".live_notes" / f"{notes_path.stem}.page.json"
    digest = hashlib.sha256((system + doc).encode()).hexdigest()
    saved = json.loads(cache.read_text()) if cache.exists() else {}
    if saved.get("source") == digest:
        say("page", "page", "notes unchanged since they were typeset, so only the page design is reapplied (free)")
        fills, words = saved["fills"], saved["words"]
    else:
        say("page", "page", f"distilling {notes_path.name} with {NOTES_MODEL}, a few minutes")
        fills, words = typeset_page(client, key, notes_path, doc, system, budget)
        cache.parent.mkdir(exist_ok=True)
        write_atomic(cache, json.dumps({"source": digest, "fills": fills, "words": words, "budget": budget}))
    week, _, title = lecture_name.partition(" — ")
    if not title:
        week, title = "", lecture_name
    stamp = re.search(r"\d{8}", notes_path.stem)
    # the cache keeps slide paths; the page gets the slides themselves, read fresh each time
    paths = json.loads(fills["slides"])
    uris = {p: slide_uri(notes_path.parent / p) or p for p in set(paths.values())}
    fills = fills | {
        "slides": json.dumps({n: uris[p] for n, p in paths.items()}),
        "content": re.sub(r'src="([^"]+)"', lambda m: f'src="{uris.get(unescape(m.group(1)), m.group(1))}"', fills["content"]),
        "week": escape(week),
        "title": escape(title),
        "course": escape(course),
        "date": datetime.strptime(stamp.group(0), "%Y%m%d").strftime("%-d %B %Y") if stamp else "",
    }
    # one pass, so text inside the generated content is never read as a placeholder
    page = re.sub(r"\{\{(\w+)\}\}", lambda m: fills.get(m.group(1), m.group(0)), HTML_TEMPLATE.read_text())
    html_path = page_path(notes_path, lecture_name)
    write_atomic(html_path, page)
    return html_path, words, budget


def write_html(client, key, course, notes_path, lecture_name):
    before = spend.by_kind["page"]
    try:
        page, words, budget = render_html(client, key, course, notes_path, lecture_name)
    except Exception as e:
        progress("page")
        say("warn", "page failed", f"{e}. The notes are unchanged; `lecture page` tries again.")
        return
    length = paint(f"{words:,} words of {budget:,} allowed", "red" if words > budget else "dim")
    say("done", "page", f"{page.name}  {length}  {paint(money(spend.by_kind['page'] - before), 'dim')}")


HELP = """\
examples:
  lecture              record in this folder; resumes if today's notes are already here
  lecture page         typeset today's study page from the notes, without recording
  lecture spend        what the tool has cost, by month, course and lecture

while recording:
  Enter                snapshot: fold what was said since the last one into the notes
  a hint, then Enter   the same, with a focus hint for the note-taker
  polish, then Enter   rewrite the notes into a clean study document and its page
  Ctrl+C               stop, after a last snapshot

defaults:
  course   the folder above Weeks/ in this path, else LECTURE_COURSE in .env
  device   LECTURE_DEVICE in .env (e.g. BlackHole), else the system input"""


def plural(n, word):
    return f"{n} {word}{'' if n == 1 else 's'}"


def main():
    ap = argparse.ArgumentParser(
        prog="lecture",
        description="Live lecture notes. Run it inside the lecture's folder.",
        epilog=HELP,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    ap.add_argument("command", nargs="?", choices=["page", "spend"], help="page or spend; leave out to record")
    ap.add_argument("--course", help="course name (default: the folder above Weeks/, else LECTURE_COURSE)")
    ap.add_argument("--device", help="audio input, or part of its name (default: LECTURE_DEVICE, else the system input)")
    ap.add_argument("--keyterm", action="append", default=[], help="a term to bias recognition toward (repeatable, up to 100)")
    ap.add_argument("--notes", help="notes file (default: lecture_notes_<today>.md)")
    ap.add_argument("--transcript", help="transcript file (default: lecture_transcript_<today>.txt)")
    ap.add_argument("--slides-dir", default="slides", help="where captured slides go (default: slides)")
    args = ap.parse_args()
    if args.command == "spend":
        return show_spend()

    env = load_env()
    key = env["GROK_API_KEY"]
    lecture_dir = Path.cwd()
    course = args.course or course_from_path(lecture_dir) or env.get("LECTURE_COURSE") or "Lecture"
    device = args.device or env.get("LECTURE_DEVICE") or None
    if args.command != "page":
        # checked before any file is created, so a missing device leaves the folder untouched
        try:
            input_name = sd.query_devices(device, "input")["name"]
        except ValueError:
            inputs = [d["name"] for d in sd.query_devices() if d["max_input_channels"] > 0]
            sys.exit(f"No audio input matches {device!r}. Inputs now: {', '.join(inputs)}.")
    today = datetime.now()
    stamp = today.strftime("%Y%m%d")
    transcript_path = Path(args.transcript or f"lecture_transcript_{stamp}.txt").resolve()
    notes_path = Path(args.notes or f"lecture_notes_{stamp}.md").resolve()
    slides_dir = Path(args.slides_dir).resolve()
    bookkeeping = lecture_dir / ".live_notes"
    state_path = bookkeeping / f"{notes_path.stem}.json"
    title = f"# {course} — {lecture_dir.name} — {today:%Y-%m-%d}"
    client = httpx.Client()
    spend.open(course, lecture_dir.name)

    if args.command == "page":
        if not notes_path.exists():
            sys.exit(f"No notes to typeset: {notes_path.name} is not in this folder.")
        write_html(client, key, course, notes_path, lecture_dir.name)
        return

    slides_dir.mkdir(parents=True, exist_ok=True)
    bookkeeping.mkdir(exist_ok=True)
    notes_existed = notes_path.exists()
    if not notes_existed:
        notes_path.write_text(title + "\n")
    transcript_path.touch()
    slides_now = existing_slides(slides_dir)
    state = json.loads(state_path.read_text()) if state_path.exists() else {}
    if "transcript_offset" not in state:
        state = initial_state(state, notes_existed, transcript_path, slides_now)
    recover_commit(state, notes_path)
    write_atomic(state_path, json.dumps(state))

    pending = []
    with open(transcript_path, "rb") as tf:
        tf.seek(state["transcript_offset"])
        for line in tf.read().decode(errors="replace").splitlines():
            m = LINE_RE.match(line)
            if m:
                pending.append((m.group(1), m.group(2)))
    pending_slides = [s for s in slides_now if s[1] > state["slide_index"]]
    with open(transcript_path, "a") as tf:
        tf.write(f"--- {'resumed' if transcript_path.stat().st_size else 'started'} {hms(time.time())} ---\n")

    print()
    print(f"  {paint(course, 'bold')}  {paint('›', 'dim')}  {lecture_dir.name}")
    waiting = f", resumed with {plural(len(pending), 'line')} and {plural(len(pending_slides), 'slide')} for the next snapshot"
    print(paint(f"  listening on {input_name}{waiting if pending or pending_slides else ''}", "dim"))
    print()

    lock = threading.Lock()
    snapshot_lock = threading.RLock()
    audio_q = queue.Queue()
    chunk_q = queue.Queue()
    cut_request = threading.Event()
    cut_done = threading.Event()
    stopping = threading.Event()
    counters = {"slide": max([s[1] for s in existing_slides(slides_dir)], default=0) + 1}
    started = time.time()
    trace = collections.deque(maxlen=TRACE_BLOCKS)
    heard = {"at": started}

    def save_state():
        write_atomic(state_path, json.dumps(state))

    def status(width):
        t = int(time.time() - started)
        if stopping.is_set():
            parts = [(" ◌ ", "teal"), ("finishing ", "bold")]
        else:
            parts = [(" ● ", "red"), ("REC ", "red", "bold")]
        parts.append((f"{t // 3600:02d}:{t % 3600 // 60:02d}:{t % 60:02d}  ",))
        # exact digital silence is what a routing fault sounds like; a quiet room never reads zero
        if time.time() - heard["at"] > NO_SIGNAL_SECONDS:
            parts.append(("no signal".rjust(TRACE_BLOCKS, "─") + "  ", "red", "bold"))
        else:
            parts.append(("".join(trace).rjust(TRACE_BLOCKS) + "  ", "teal"))
        frame = SPINNER[int(time.time() * 8) % len(SPINNER)]
        for label in list(screen.busy.values()):
            parts += [(f"{frame} ", "teal"), (f"{label}  ",)]
        with lock:
            lines, slides = len(pending), len(pending_slides)
        queued = f"{plural(lines, 'line')}, {plural(slides, 'slide')} queued" if lines or slides else "nothing queued"
        parts += [(f"{queued}  ", "dim"), (f"{money(spend.lecture)}  ",), (input_name, "dim")]
        return fit(parts, width)

    def callback(indata, frames, time_info, status):
        if status:
            say("warn", "audio", str(status))
        audio_q.put(indata[:, 0].copy())

    def transcribe_worker():
        while True:
            item = chunk_q.get()
            if item is None:
                chunk_q.task_done()
                return
            start, chunk = item
            text = transcribe(client, key, to_wav(chunk), len(chunk) / SAMPLE_RATE, args.keyterm)
            if text:
                line = f"[{hms(start)}] {text}"
                screen.write(f"  {paint(hms(start), 'dim')}  {text}")
                # file line and pending entry change together so a snapshot's offset matches its batch
                with lock:
                    with open(transcript_path, "a") as tf:
                        tf.write(line + "\n")
                    pending.append((hms(start), text))
            chunk_q.task_done()

    def snapshot(hint="", flush=True):
        with snapshot_lock:
            progress("snapshot", "snapshot")
            try:
                return take_snapshot(hint, flush)
            finally:
                progress("snapshot")

    def take_snapshot(hint, flush):
        t_enter = time.time()
        if flush:
            cut_done.clear()
            cut_request.set()
            cut_done.wait(5)
            if chunk_q.unfinished_tasks:
                progress("snapshot", "snapshot, waiting for transcription")
            deadline = time.time() + 90
            while chunk_q.unfinished_tasks and time.time() < deadline:
                time.sleep(0.2)
        with lock:
            segments = sorted(pending)
            slides = sorted(pending_slides)
            pending.clear()
            pending_slides.clear()
            offset = transcript_path.stat().st_size
        if not segments and not slides:
            say("notes", "snapshot", "nothing new since the last one")
            return True
        slide_index = max([s[1] for s in slides], default=state["slide_index"])
        words = sum(len(s[1].split()) for s in segments)
        progress("snapshot", f"snapshot, {words} words to {NOTES_MODEL}")
        doc = notes_path.read_text()
        before = spend.by_kind["notes"]
        try:
            notes = make_notes(client, key, course, doc, segments, slides, hint, notes_path.parent)
        except Exception as e:
            say("warn", "snapshot failed", f"{e}; everything is kept for the next one")
            with lock:
                pending[:0] = segments
                pending_slides[:0] = slides
            return False
        for s in slides:
            if embed_md(s, notes_path.parent) not in notes:
                notes += f"\n\n{embed_md(s, notes_path.parent)}\n"
        block = f"\n<!-- {hms(t_enter)} -->\n{notes}\n"
        state["commit"] = {
            "before": notes_path.stat().st_size, "block": block,
            "transcript_offset": offset, "slide_index": slide_index,
        }
        save_state()
        with open(notes_path, "a") as nf:
            nf.write(block)
            nf.flush()
            os.fsync(nf.fileno())
        del state["commit"]
        state["transcript_offset"], state["slide_index"] = offset, slide_index
        save_state()
        screen.write("\n".join(f"  {paint('│', 'teal')} {paint(line, 'dim')}" for line in notes.strip().splitlines()))
        cost = paint(money(spend.by_kind["notes"] - before), "dim")
        say("notes", "notes", f"{plural(words, 'word')} and {plural(len(slides), 'slide')} folded in  {cost}")
        return True

    def polish():
        with snapshot_lock:
            if not snapshot():
                say("warn", "polish stopped", "the snapshot before it failed; the notes are unchanged")
                return
            doc = notes_path.read_text()
            transcript = transcript_path.read_text()
            embeds = EMBED_RE.findall(doc)
            progress("polish", f"polishing {plural(len(doc.split()), 'word')}")
            before = spend.by_kind["polish"]
            try:
                new = polish_notes(client, key, course, doc, transcript, title)
            except Exception as e:
                say("warn", "polish failed", f"{e}; the notes are unchanged")
                return
            finally:
                progress("polish")
            for e in embeds:
                if e not in new:
                    new += f"\n\n{e}\n"
            backup = bookkeeping / f"{notes_path.stem}_{datetime.now():%H%M%S}.md"
            shutil.copy2(notes_path, backup)
            with open(backup, "rb") as bf:
                os.fsync(bf.fileno())
            write_atomic(notes_path, new.rstrip() + "\n")
            cost = paint(money(spend.by_kind["polish"] - before), "dim")
            say("done", "polished", f"{notes_path.name}, previous version in .live_notes/{backup.name}  {cost}")
        # typesetting takes minutes and only reads the polished file, so snapshots are not held up by it
        write_html(client, key, course, notes_path, lecture_dir.name)

    def on_line(text):
        cmd = text.strip()
        if screen.active:
            screen.write(f"  {paint('›', 'teal')} {cmd or paint('snapshot', 'dim')}")
        if cmd.lower() == "polish":
            threading.Thread(target=polish, daemon=True).start()
        else:
            threading.Thread(target=snapshot, args=(cmd,), daemon=True).start()

    def register_slide(p):
        ts = p.stat().st_mtime
        idx = counters["slide"]
        counters["slide"] += 1
        dest = slides_dir / f"slide_{idx:02d}_{datetime.fromtimestamp(ts):%H%M%S}{p.suffix.lower()}"
        if p.resolve() != dest:
            shutil.move(p, dest)
        shrink_if_large(dest)
        with lock:
            pending_slides.append((hms(ts), idx, dest))
        say("slide", f"slide {idx}", f"{dest.name}, into the next snapshot")
        return dest

    def slide_watcher():
        shots_dir = screenshot_dir()
        started_at = time.time()
        known = {p.resolve() for p in slides_dir.iterdir()}
        sizes = {}
        while True:
            candidates = list(slides_dir.iterdir()) + [
                p for p in shots_dir.glob("Screen*") if p.stat().st_mtime >= started_at
            ]
            for p in candidates:
                if p.resolve() in known or p.suffix.lower() not in IMAGE_SUFFIXES:
                    continue
                try:
                    size = p.stat().st_size
                except OSError:
                    continue
                # wait until the file stops growing so a half-written capture is never read
                if sizes.get(p) != size:
                    sizes[p] = size
                    continue
                known.add(register_slide(p))
            time.sleep(1)

    worker = threading.Thread(target=transcribe_worker, daemon=True)
    worker.start()
    threading.Thread(target=slide_watcher, daemon=True).start()

    blocks, loudness = [], []
    chunk_start = None

    def cut():
        nonlocal blocks, loudness
        if blocks:
            chunk_q.put((chunk_start, np.concatenate(blocks)))
        blocks, loudness = [], []

    screen.start(status, on_line)
    try:
        with sd.InputStream(
            device=device,
            samplerate=SAMPLE_RATE,
            channels=1,
            dtype="float32",
            blocksize=int(SAMPLE_RATE * BLOCK_SECONDS),
            callback=callback,
        ):
            try:
                while True:
                    block = audio_q.get()
                    if not blocks:
                        chunk_start = time.time() - BLOCK_SECONDS
                    blocks.append(block)
                    loudness.append(float(np.sqrt(np.mean(block ** 2))))
                    if block.any():
                        heard["at"] = time.time()
                    # -60 to -10 dBFS across the eight trace heights
                    db = 20 * np.log10(max(loudness[-1], 1e-9))
                    trace.append(TRACE[min(7, max(0, int((db + 60) / 6.25)))])
                    seconds = len(blocks) * BLOCK_SECONDS
                    quiet = loudness[-1] < QUIET_RATIO * (sum(loudness) / len(loudness))
                    if cut_request.is_set():
                        cut_request.clear()
                        cut()
                        cut_done.set()
                    elif seconds >= MAX_CHUNK_SECONDS or (seconds >= MIN_CHUNK_SECONDS and quiet):
                        cut()
            except KeyboardInterrupt:
                pass

        stopping.set()
        cut()
        chunk_q.put(None)
        if "page" in screen.busy:
            say("warn", "stopping", "the page was still being typeset; `lecture page` finishes it later")
        say("notes", "stopping", "transcribing what is left, then a last snapshot (Ctrl+C again skips the waiting audio)")
        try:
            # join with a timeout so a second Ctrl+C gets through while held audio waits on the connection
            while worker.is_alive():
                worker.join(0.5)
        except KeyboardInterrupt:
            say("warn", "stopping", "skipped the audio that was still waiting to be transcribed")
        snapshot(flush=False)
    finally:
        screen.stop()
    print(f"\n  {paint('✓', 'teal')} {paint('saved', 'bold')}  {notes_path.name}  {transcript_path.name}")
    print(paint(f"    {money(spend.lecture)} spent on this lecture today; `lecture spend` has the rest", "dim"))
    print()


if __name__ == "__main__":
    main()
