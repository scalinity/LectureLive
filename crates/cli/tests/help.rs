//! Task 1's help goldens (M7 plan §J): `lecture --help` and `record --help`, byte for byte, run
//! exactly as the person runs them. Piped stdout paints no colour, as on a non-TTY.

fn help(args: &[&str]) -> String {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_lecturelive")).args(args).output().unwrap();
    assert!(out.status.success(), "exit {:?}", out.status.code());
    assert!(out.stderr.is_empty(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn lecture_help_is_pinned() {
    assert_eq!(help(&["lecture", "--help"]), concat!(
        "A lecture with live notes, as live_notes.py does it: Enter takes a snapshot, a hint then Enter adds a focus hint, polish then Enter polishes and typesets the page, Ctrl-C stops (twice: stop waiting for recovery)\n",
        "\n",
        "Usage: lecturelive lecture [OPTIONS] [COMMAND]\n",
        "\n",
        "Arguments:\n",
        "  [COMMAND]  page: typeset the study page from the notes; spend: what the tool has cost; audit: whether the folder's audio is whole (exit 1 when anything is unexplained, 2 while something waits). Leave out to record [possible values: page, spend, audit]\n",
        "\n",
        "Options:\n",
        "      --dir <DIR>                The lecture folder (default: the current directory)\n",
        "      --course <COURSE>          Course name (default: the folder above Weeks/, else LECTURE_COURSE)\n",
        "      --loopback                 Record BlackHole: Zoom through \"LectureLive Loopback\"\n",
        "      --mixed <MIXED>            Record Zoom (through LectureLive Loopback) and this input together: its UID or part of its name\n",
        "      --device <DEVICE>          Audio input: its UID or part of its name (default: LECTURE_DEVICE)\n",
        "      --keyterm <KEYTERMS>       A term to bias recognition toward (repeatable; up to 100, each at most 50 characters)\n",
        "      --notes <NOTES>            Notes file (default: lecture_notes_<today>.md)\n",
        "      --transcript <TRANSCRIPT>  Transcript file (default: lecture_transcript_<today>.txt)\n",
        "      --slides-dir <SLIDES_DIR>  Where captured slides go (default: slides)\n",
        "      --secs <SECS>              Stop after this many seconds\n",
        "      --keep-days <KEEP_DAYS>    Delete closed recordings older than this many days that have no unresolved gap\n",
        "      --rebuild                  Rebuild a corrupt sidecar from the notes, transcript and slides\n",
        "  -h, --help                     Print help\n",
    ));
}

#[test]
fn record_help_is_pinned() {
    assert_eq!(help(&["record", "--help"]), concat!(
        "Record an input (or Zoom through BlackHole) into a lecture folder until Ctrl-C\n",
        "\n",
        "Usage: lecturelive record [OPTIONS]\n",
        "\n",
        "Options:\n",
        "      --loopback               \n",
        "      --mixed <MIXED>          Record Zoom (through LectureLive Loopback) and this input together: its UID or part of its name\n",
        "      --device <DEVICE>        Input device UID (see `inputs`)\n",
        "      --dir <DIR>              \n",
        "      --secs <SECS>            Stop after this many seconds\n",
        "      --keep-days <KEEP_DAYS>  Delete closed recordings older than this many days that have no unresolved gap\n",
        "      --stt                    Stream to Grok speech-to-text and write the transcript (GROK_API_KEY from the repository's .env or the environment)\n",
        "      --keyterm <KEYTERMS>     A term to bias recognition toward (repeatable; up to 100, each at most 50 characters)\n",
        "  -h, --help                   Print help\n",
    ));
}
