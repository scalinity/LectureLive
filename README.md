# LectureLive

Live lecture transcription with slide capture and LLM-written notes, for a single
person on a Mac. Audio goes to Grok Voice Transcribe, the running transcript and
slide screenshots go to `grok-4.7` on demand, and the result is one Markdown notes
document per lecture that grows during class and can be rewritten into a clean study
document afterwards.

Current state: a Python command-line tool (`live_notes.py`) that works today.
A native companion app (Tauri + Svelte, Rust pipeline) is proposed in
`docs/spec.md`, with the build order in `docs/milestones.md`.

## Requirements

- macOS (uses `sips` and the system screenshot folder)
- [uv](https://docs.astral.sh/uv/) (manages Python 3.12+ and dependencies)
- An xAI API key

## Setup

```bash
cp .env.example .env                  # then fill in GROK_API_KEY (and LECTURE_DEVICE, see below)
uv tool install --editable .          # puts `lecture` on your PATH; code edits apply without reinstalling
```

## Use

Run it inside the folder that holds that lecture's files:

```bash
cd ~/path/to/<course>/Weeks/<week>
lecture                 # record; resumes if today's notes are already here
lecture page            # typeset today's study page from the notes, without recording
lecture spend           # what the tool has cost, by month, course and lecture
```

The course comes from the folder above `Weeks/` (else `LECTURE_COURSE`, else `--course`),
and the audio input from `LECTURE_DEVICE` in `.env` (else the system input, else
`--device`). `lecture --help` lists everything.

| Key                | Action                                                                 |
| ------------------ | ---------------------------------------------------------------------- |
| `Enter`            | Snapshot: fold everything since the last snapshot into today's notes   |
| `<hint>` + `Enter` | Same, with a focus hint for the note-taker                             |
| `polish` + `Enter` | Rewrite today's notes into one clean study document, then its HTML page |
| `Ctrl+C`           | Stop (takes a final snapshot first)                                    |

While it records, the transcript scrolls above a footer that stays at the bottom: the
recording light and elapsed time, a trace of the input's loudness, what is queued for the
next snapshot, and what this lecture has cost so far. When the input has been exactly
silent for 15 seconds the trace reads **no signal**, which with BlackHole means Zoom's
audio is no longer reaching it (usually Zoom's speaker setting).

Take a screenshot of a slide with the normal macOS shortcut while it runs; the file is
moved into `slides/` and placed in the timeline at the moment it was captured, so the
next snapshot knows which words were said while that slide was up. Images dropped
into `slides/` are picked up the same way.

If the connection drops, recording carries on and the held audio is transcribed, in
order and with its original times, once the connection is back. Stopping during an
outage waits for it; a second `Ctrl+C` stops without the audio still waiting.

`--keyterm` (repeatable, up to 100) biases recognition toward vocabulary the lecturer
uses; it noticeably helps with accented technical terms.

`--device NAME` picks the audio input by name or part of it for one run; a name that
matches nothing lists the inputs there are. The input in use is printed at start.

### Zoom lectures on headphones

With headphones on, the Mac's microphone can't hear the lecture, so Zoom's audio is
captured directly through the BlackHole virtual device. One-time setup:

1. `brew install blackhole-2ch`
2. In Audio MIDI Setup, **+** → **Create Multi-Output Device**. Tick **BlackHole 2ch** and
   the headphones only, keep **BlackHole 2ch** as the **Primary Device** (it never
   disconnects, so the clock never disappears), and keep drift correction on for the
   headphones. Rename it, e.g. *AirPods + notes*. A second one with the MacBook speakers in
   place of the headphones covers days without them.
3. Leave the Mac's own output (System Settings → Sound) on the headphones or speakers.
   Only Zoom uses the Multi-Output Device; anything else played into it would be
   transcribed into the notes.
4. In Zoom → Settings → Audio, pick by name rather than "Same as System": **Speaker** is
   the Multi-Output Device and **Microphone** is the MacBook's built-in mic. Using the
   Bluetooth headphones' own mic switches them into call mode, which lowers playback
   quality and changes their sample rate underneath the Multi-Output Device.
5. For AirPods, turn off **Automatic Head Detection** and set **Connect to This Mac** to
   *When Last Connected to This Mac*, so putting them back on does not make macOS grab
   them as the default speaker and mic again.

Then set `LECTURE_DEVICE=BlackHole` in `.env`. Set the listening volume on the headphones
themselves; Zoom's speaker slider also lowers the level that gets transcribed.

### The study page

`polish` also distils the polished notes into a high-yield study page named after the
lecture (the folder `Week 06 — Statistical Analysis Methods` gives `Statistical Analysis
Methods.html`). The notes stay the complete record; the page keeps what you need to revise,
within a budget of 30% of the notes' words: flagged and examinable points, the formulas and
when to use each, one compressed example per method, and new terms. The few slides whose
figures teach best are redrawn as formulas, tables and diagrams, each with a switch back to
the original, and all of them stay one click away in the collapsed All slides gallery.
Around that: a contents rail, a self-test mode that hides definitions, steps and formulas
until clicked, glossary definitions on hover, a formula sheet, and check-yourself questions.

Distilling takes a few minutes; wait for `✓ page`, which also reports the page's words
against its budget. The slides are embedded, so the page is one self-contained file that
opens and moves anywhere; formulas and fonts load from the web when it opens.

`lecture page` distils today's notes (or `--notes FILE`) without recording, e.g. to retry a
failed page. The result is kept in `.live_notes/`, so while the notes are unchanged it only
reapplies the page design (`notes_template.html`) and costs nothing.

## Files written

```
<lecture folder>/
  lecture_notes_YYYYMMDD.md        one document per day, append-only during class
  <lecture title>.html             the study page, written by polish (dated only if the
                                   folder holds more than one day's notes)
  lecture_transcript_YYYYMMDD.txt  [HH:MM:SS] text, one line per utterance
  slides/slide_NN_HHMMSS.png       captured slides, ≤1600 px
  .live_notes/                     resume state, polish backups, typeset page parts
```

Restarting the tool on the same day resumes: transcript lines and slides newer than
the last snapshot are picked up, nothing is re-processed, and no new document is made.

## Cost

Every paid call is appended to `spend.jsonl` next to `live_notes.py`: when, which course
and lecture, what it was for (`transcribe`, `notes`, `polish`, `page`) and what it cost.
`grok-4.7` calls record the cost xAI reports with each response (`billed`); transcription
responses carry no cost, so those are worked out from the audio length at the published
$0.10 per hour (`estimated`). The recording footer shows today's total for the lecture,
and `lecture spend` summarises by month, course and lecture.

Transcription is the smallest share, about $0.20 for a two-hour lecture. Each snapshot
is one `grok-4.7` call carrying the notes so far, the new transcript and the new slide
images; polish is one larger call; the study page is one call per section plus one for
the keystone and questions, each carrying that section's slides.
