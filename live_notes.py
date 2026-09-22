"""Live lecture transcription (Grok Voice Transcribe) with snapshot notes (grok-4.7).

Run from the lecture's folder:  uv run --project <repo> live-notes [--keyterm TERM ...]
  Enter           snapshot: fold everything since the last snapshot into today's notes
  <hint> + Enter  same, with a focus hint for the note-taker
  polish + Enter  rewrite today's notes in place into one clean study document
  Ctrl+C          stop (takes a final snapshot first)

Writes into the current directory: one notes document per day that grows across
snapshots and restarts, a timestamped transcript, and slides/. Screenshots taken
while running (or images dropped into slides/) are placed in the timeline at the
moment they were captured. Bookkeeping and polish backups live in .live_notes/.

Configuration comes from .env next to this file (see .env.example).
"""

import argparse
import base64
import io
import json
import os
import queue
import re
import shutil
import subprocess
import sys
import threading
import time
import wave
from datetime import datetime
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

HERE = Path(__file__).resolve().parent
LINE_RE = re.compile(r"^\[(\d\d:\d\d:\d\d)\] (.+)$")
SLIDE_RE = re.compile(r"^slide_(\d+)_(\d{6})\.(png|jpe?g)$", re.I)
EMBED_RE = re.compile(r"!\[Slide \d+\]\([^)]+\)")


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


def hms(ts):
    return datetime.fromtimestamp(ts).strftime("%H:%M:%S")


def load_env():
    values = {}
    env_file = HERE / ".env"
    if env_file.exists():
        for line in env_file.read_text().splitlines():
            if "=" in line and not line.lstrip().startswith("#"):
                k, v = line.split("=", 1)
                values[k.strip()] = v.strip().strip('"').strip("'")
    values.update({k: os.environ[k] for k in ("GROK_API_KEY", "LECTURE_COURSE") if k in os.environ})
    if not values.get("GROK_API_KEY"):
        sys.exit("GROK_API_KEY not set (put it in .env next to live_notes.py, see .env.example)")
    return values


def to_wav(audio):
    pcm = (np.clip(audio, -1, 1) * 32767).astype(np.int16)
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(SAMPLE_RATE)
        w.writeframes(pcm.tobytes())
    return buf.getvalue()


def transcribe(client, key, wav, keyterms):
    data = {"language": "en", "format": "true"}
    if keyterms:
        data["keyterm"] = keyterms
    for attempt in range(3):
        try:
            r = client.post(
                f"{API}/stt",
                headers={"Authorization": f"Bearer {key}"},
                files={"file": ("chunk.wav", wav, "audio/wav")},
                data=data,
                timeout=60,
            )
            if r.status_code == 200:
                return r.json().get("text", "").strip()
            print(f"[stt {r.status_code}] {r.text[:200]}", file=sys.stderr)
        except httpx.HTTPError as e:
            print(f"[stt error] {e}", file=sys.stderr)
        time.sleep(2 * (attempt + 1))
    print("[stt] giving up on this chunk", file=sys.stderr)
    return ""


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


def chat(client, key, system, content):
    r = client.post(
        f"{API}/chat/completions",
        headers={"Authorization": f"Bearer {key}"},
        json={"model": NOTES_MODEL, "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": content},
        ]},
        timeout=600,
    )
    r.raise_for_status()
    return r.json()["choices"][0]["message"]["content"]


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
    return clean_output(chat(client, key, notes_system(course), content), strip_title=True)


def polish_notes(client, key, course, doc, transcript, title):
    user = (
        f"Use this exact title line: {title}\n\n"
        f"Notes as written during the lecture:\n<<<\n{doc}\n>>>\n\n"
        f"Full transcript:\n<<<\n{transcript}\n>>>\n\n"
        "Write the complete replacement document."
    )
    return clean_output(chat(client, key, polish_system(course), user), strip_title=False)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--transcript", default=None)
    ap.add_argument("--notes", default=None)
    ap.add_argument(
        "--keyterm", action="append", default=[],
        help="domain term to bias recognition toward (repeatable, max 100)",
    )
    ap.add_argument("--slides-dir", default="slides")
    ap.add_argument("--course", default=None, help="course name for the note-taker (default: LECTURE_COURSE from .env)")
    args = ap.parse_args()

    env = load_env()
    key = env["GROK_API_KEY"]
    course = args.course or env.get("LECTURE_COURSE") or "Lecture"
    lecture_dir = Path.cwd()
    today = datetime.now()
    stamp = today.strftime("%Y%m%d")
    transcript_path = Path(args.transcript or f"lecture_transcript_{stamp}.txt").resolve()
    notes_path = Path(args.notes or f"lecture_notes_{stamp}.md").resolve()
    slides_dir = Path(args.slides_dir).resolve()
    slides_dir.mkdir(parents=True, exist_ok=True)
    bookkeeping = lecture_dir / ".live_notes"
    bookkeeping.mkdir(exist_ok=True)
    state_path = bookkeeping / f"{notes_path.stem}.json"
    title = f"# {course} — {lecture_dir.name} — {today:%Y-%m-%d}"
    client = httpx.Client()

    if not notes_path.exists():
        notes_path.write_text(title + "\n")
    if state_path.exists():
        state = json.loads(state_path.read_text())
    else:
        # first run against an existing notes file: treat everything before now as already noted
        state = {"noted_through": hms(time.time()) if transcript_path.exists() else "00:00:00"}
    with open(transcript_path, "a") as tf:
        tf.write(f"--- {'resumed' if transcript_path.stat().st_size else 'started'} {hms(time.time())} ---\n")

    pending = []
    pending_slides = []
    for line in transcript_path.read_text().splitlines():
        m = LINE_RE.match(line)
        if m and m.group(1) >= state["noted_through"]:
            pending.append((m.group(1), m.group(2)))
    pending_slides += [s for s in existing_slides(slides_dir) if s[0] >= state["noted_through"]]
    if pending or pending_slides:
        print(f"Resumed: {len(pending)} transcript line(s), {len(pending_slides)} slide(s) not yet in notes.", file=sys.stderr)

    lock = threading.Lock()
    snapshot_lock = threading.RLock()
    audio_q = queue.Queue()
    chunk_q = queue.Queue()
    cut_request = threading.Event()
    cut_done = threading.Event()
    counters = {"slide": max([s[1] for s in existing_slides(slides_dir)], default=0) + 1}

    def save_state():
        state_path.write_text(json.dumps(state))

    def callback(indata, frames, time_info, status):
        if status:
            print(status, file=sys.stderr)
        audio_q.put(indata[:, 0].copy())

    def transcribe_worker():
        while True:
            item = chunk_q.get()
            if item is None:
                chunk_q.task_done()
                return
            start, chunk = item
            text = transcribe(client, key, to_wav(chunk), args.keyterm)
            if text:
                line = f"[{hms(start)}] {text}"
                print(line)
                with open(transcript_path, "a") as tf:
                    tf.write(line + "\n")
                with lock:
                    pending.append((hms(start), text))
            chunk_q.task_done()

    def snapshot(hint="", flush=True):
        with snapshot_lock:
            t_enter = time.time()
            if flush:
                cut_done.clear()
                cut_request.set()
                cut_done.wait(5)
                if chunk_q.unfinished_tasks:
                    print("[snapshot] waiting for transcription to catch up ...", file=sys.stderr)
                deadline = time.time() + 90
                while chunk_q.unfinished_tasks and time.time() < deadline:
                    time.sleep(0.2)
            with lock:
                segments = sorted(pending)
                slides = sorted(pending_slides)
                pending.clear()
                pending_slides.clear()
            if not segments and not slides:
                print("[snapshot] nothing new since the last snapshot", file=sys.stderr)
                return
            words = sum(len(s[1].split()) for s in segments)
            print(f"[snapshot] {words} words, {len(slides)} slide(s) -> {NOTES_MODEL} ...", file=sys.stderr)
            doc = notes_path.read_text()
            try:
                notes = make_notes(client, key, course, doc, segments, slides, hint, notes_path.parent)
            except Exception as e:
                print(f"[snapshot] failed, kept for next snapshot: {e}", file=sys.stderr)
                with lock:
                    pending[:0] = segments
                    pending_slides[:0] = slides
                return
            for s in slides:
                if embed_md(s, notes_path.parent) not in notes:
                    notes += f"\n\n{embed_md(s, notes_path.parent)}\n"
            block = f"\n<!-- {hms(t_enter)} -->\n{notes}\n"
            with open(notes_path, "a") as nf:
                nf.write(block)
            state["noted_through"] = hms(t_enter)
            save_state()
            print("\n" + "=" * 60 + block + "=" * 60 + "\n")

    def polish():
        with snapshot_lock:
            snapshot()
            doc = notes_path.read_text()
            transcript = transcript_path.read_text()
            embeds = EMBED_RE.findall(doc)
            print(f"[polish] rewriting {len(doc.split())} words of notes with {NOTES_MODEL} ...", file=sys.stderr)
            try:
                new = polish_notes(client, key, course, doc, transcript, title)
            except Exception as e:
                print(f"[polish] failed, notes unchanged: {e}", file=sys.stderr)
                return
            for e in embeds:
                if e not in new:
                    new += f"\n\n{e}\n"
            backup = bookkeeping / f"{notes_path.stem}_{datetime.now():%H%M%S}.md"
            shutil.copy2(notes_path, backup)
            notes_path.write_text(new.rstrip() + "\n")
            print(f"[polish] done. Previous version: {backup}", file=sys.stderr)

    def stdin_worker():
        for line in sys.stdin:
            cmd = line.strip()
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
        print(f"[slide {idx}] {dest.name} -> next snapshot", file=sys.stderr)
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
    threading.Thread(target=stdin_worker, daemon=True).start()
    threading.Thread(target=slide_watcher, daemon=True).start()

    blocks, loudness = [], []
    chunk_start = None

    def cut():
        nonlocal blocks, loudness
        if blocks:
            chunk_q.put((chunk_start, np.concatenate(blocks)))
        blocks, loudness = [], []

    print("Listening. Enter = snapshot, 'polish' + Enter = clean rewrite, Ctrl+C = stop.", file=sys.stderr)
    with sd.InputStream(
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

    cut()
    chunk_q.put(None)
    print("\nStopping: transcribing remaining audio ...", file=sys.stderr)
    worker.join()
    snapshot(flush=False)
    print(f"Transcript: {transcript_path}\nNotes: {notes_path}", file=sys.stderr)


if __name__ == "__main__":
    main()
