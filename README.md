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
cp .env.example .env     # then fill in GROK_API_KEY and LECTURE_COURSE
uv sync
```

## Use

Run it from the folder that should hold that lecture's files:

```bash
cd ~/path/to/course/Week-5
uv run --project ~/path/to/LectureLive live-notes --keyterm pretrained --keyterm "fine-tuning"
```

| Key                | Action                                                                 |
| ------------------ | ---------------------------------------------------------------------- |
| `Enter`            | Snapshot: fold everything since the last snapshot into today's notes   |
| `<hint>` + `Enter` | Same, with a focus hint for the note-taker                             |
| `polish` + `Enter` | Rewrite today's notes in place into one clean study document           |
| `Ctrl+C`           | Stop (takes a final snapshot first)                                    |

Take a screenshot of a slide with the normal macOS shortcut while it runs; the file is
moved into `slides/` and placed in the timeline at the moment it was captured, so the
next snapshot knows which words were said while that slide was up. Images dropped
into `slides/` are picked up the same way.

`--keyterm` (repeatable, up to 100) biases recognition toward vocabulary the lecturer
uses; it noticeably helps with accented technical terms.

## Files written

```
<lecture folder>/
  lecture_notes_YYYYMMDD.md        one document per day, append-only during class
  lecture_transcript_YYYYMMDD.txt  [HH:MM:SS] text, one line per utterance
  slides/slide_NN_HHMMSS.png       captured slides, ≤1600 px
  .live_notes/                     resume state and polish backups
```

Restarting the tool on the same day resumes: transcript lines and slides newer than
the last snapshot are picked up, nothing is re-processed, and no new document is made.

## Cost

Grok Voice Transcribe batch transcription is billed per hour of audio; each snapshot
is one `grok-4.7` call carrying the notes so far, the new transcript, and the new
slide images. A lecture typically costs cents.
