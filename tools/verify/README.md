# The verification harness

`tools/verify/run.sh` runs what `docs/VERIFICATION.html` asks of a person, wherever a machine can, while no one is at
the Mac. It speaks synthetic sentences into BlackHole, has the real transcription service transcribe them, takes the
network from the lecture alone, fills a scratch disk, removes a virtual input, kills and quits, drives the app's window,
and judges the folders with `lecturelive lecture audit`. It writes one verdict per check, with its evidence, to
`~/Library/Application Support/LectureLive/verify/<run>/` (`SUMMARY.md` first; `latest` points at the newest).

```
tools/verify/run.sh [--deadline HH:MM] [--only a,b] [--skip a,b] [--soak-min N] [--no-soak]
```

It needs the dev build's page server (`npm run dev` in `apps/desktop`; the gui stages start one if none runs), the `.env`
key, BlackHole, `cliclick`, `jq` and Xcode's command line tools (`swiftc`, for the virtual input). A run costs about
$0.30 in transcription and snapshots; `COST_CAP` (default $1.50) stops it earlier.

| Stage | Checks | How |
|---|---|---|
| `pause` | V17 | the CLI in plain mode; words said while paused are not transcribed, two recordings, the audit explains the hole |
| `net` | V10 | a forwarder of the harness's own holds, then refuses, the link; every word said is in the transcript at the end |
| `quit` | V18 (CLI) | SIGTERM, SIGHUP, and SIGTERM while paused: prompt, exit 143/129, finalized, no last snapshot, nothing to repair |
| `crash` | V14 | SIGKILL mid-lecture, then a new session: repaired, the cut-off utterance recovered |
| `disk` | V13 | a 40 MB disk image of its own fills under a recording; the session stops by itself, and the next start repairs it |
| `devgone` | V15, V11 | a virtual input (`virtual_input.swift`) stands in for the receiver: removed and restored, alone and mixed with Zoom's side |
| soak | V01 | a recording of silent BlackHole for `--soak-min` minutes, in the background beside the stages, audited last |
| `gui_capture` | V19, V20 | the in-app capture check: pause, a restarted window followed, a control bar, a quit with the lecture running |
| `gui_quit` | V18 (dev app) | the dev app quit by Cmd-Q, window close, SIGTERM, SIGHUP, and SIGTERM while paused |
| `gui_bundle` | V18 (packaged app) | the packaged app, built here, quit by Apple Event by app name (as a proctoring tool does), Cmd-Q, window close, SIGTERM, and by Apple Event while paused |
| `gui_looks` | V06 | the app photographed running and paused, in light and dark, in normal and large type (`looks/`) |
| `gui_drag` | V05 | the window dragged by its top strip and zoomed by a double click, read back through System Events |
| `gui_faults` | V10, V14 (app) | the app's own lecture through the forwarder, killed with SIGKILL, started again |
| `gui_perms_mic`, `gui_perms_screen` | V16, V12 (experimental) | the packaged app's own microphone and Screen Recording permission reset and its prompt answered through System Events; opt-in with `--only`, since the scripted refusal does not yet register as a refusal (the app keeps reading `undetermined`) |
| `unit` | all suites | `cargo test --workspace` and the frontend tests, with the known failures named |

## Not automated

V02 and V04 need a real Zoom meeting a lecturer shares slides in. V07 typesets a page with a paid request and reads the
Keychain. V08 needs the login password. V12 and V16 would take Screen Recording and the microphone from Terminal, which
also serves the Python tool and the harness. V03 and V09 were run by hand and in M6.

## What it will not do

- It signals only processes it started, by pid, after checking their command line; never by name or pattern.
- It never speaks into BlackHole while the Python lecture (`live_notes`) runs, and says so (`BLOCKED`).
- It stops at its deadline: a stage that cannot finish before it is skipped.
- Every stage ends the app by quitting it, never by Stop, so no slide is ever sent for notes.
- It keeps the display awake while the gui stages run, switches the system appearance for `gui_looks` only to restore it
  (also when interrupted), and never types a password.

## Class mode

`tools/verify/class.sh` is for a real Zoom class, run beside the Python tool, with nobody's hands needed. It speaks nothing, chooses
nothing on screen and touches neither the Python tool nor Zoom's view.

```
tools/verify/class.sh arm                   waits for a Zoom meeting window and starts by itself (log: class.arm.log)
tools/verify/class.sh start [--minutes N]   once the meeting window is open (default limit 130 minutes)
tools/verify/class.sh status
tools/verify/class.sh finish                also done by itself when the meeting window has been gone for 3 minutes
tools/verify/class.sh v04                   optional: Zoom full screen with another app in front for 5 minutes
tools/verify/class.sh selftest              the harness's own checks against canned data; touches no Zoom, no class
```

`LECTURELIVE_BIN=<path>` runs class mode with another build of the CLI than `target/debug/lecturelive`, for a build made
beside a class that is still running (`CARGO_TARGET_DIR=<elsewhere> cargo build -p lecturelive-cli`). A script that a class
is running from is never edited in place (bash reads it as it goes): write a copy and `mv` it over.

`start` runs one headless CLI lecture on BlackHole in a scratch folder under its own course name, watching the Zoom meeting window through
the region you chose for your course (or `REGION=x,y,w,h` as fractions of the window when the window is a new size), with the control bar
at the bottom left out. It ends by quitting, so no snapshot is taken and no slide is sent anywhere. `finish` writes `class-report.md`:

- **V01 (real class):** the audio audit over the lecture's sound (needs 30 minutes), and that the class's words reached the transcript.
- **V02:** the slides the detector took by itself and when, the capture states it went through, and its recording of what it saw
  (`detector/`), to read against what the lecturer showed: each slide or build once, none that is only the camera or the control bar.
- **V04:** `v04` puts Zoom full screen on its own desktop with Terminal in front for five minutes, confirms by reading Zoom's state
  back that it is full screen, then takes it out again whatever Zoom's window is called by then (also on Ctrl-C; if it cannot, it says
  to press Ctrl-Cmd-F in Zoom). The verdict reads each slide that arrived meanwhile: PASS when at least one arrived and every one
  is a whole slide, FAIL when any is cropped (under 80% of the widest slide before it), INCONCLUSIVE with the reason otherwise;
  a re-run replaces the earlier line. It changes what you see for five minutes, so it is run when you choose, not by `start`.
