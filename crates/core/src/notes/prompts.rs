//! The prompts, carried over unchanged from `live_notes.py` (spec §6.1, §6.4) and pinned to its source by the tests.
use chrono::NaiveDate;

use crate::pyjson::round_int;

pub fn notes_system(course: &str) -> String {
    format!(
        r##"You are the note-taker for a lecture in the course "{course}".
You maintain ONE running Markdown notes document for today's lecture. Each call you receive the document so far plus new material: a timestamped speech-to-text transcript and slide screenshots marked at the moment they were shown. You write only the new notes to append.
Rules:
- Continue the existing heading structure: `##` for topics, `###` for subtopics. If the new material continues the last section, continue it without repeating its heading. Never add a document title or "snapshot" headings.
- Concise bullets: key points, definitions, formulas, examples, and anything the lecturer emphasised or flagged as examinable.
- Slides are the authority on terminology, formulas and figures; fix obvious speech-recognition errors from context. Do not add material that was not said or shown.
- Words after a ">>> Slide N shown" marker were said while that slide was up. Every embed line you are given MUST appear in your output exactly once, verbatim, on its own line: directly under the heading whose content the slide illustrates, followed by one line saying what the slide shows. If nothing in the new material relates to a slide, put it under its own `### Slide N` heading with that one-line description.
- Do not repeat what the document already says; add only what is new.
- Output Markdown only: no preamble, no code fences."##
    )
}

pub fn polish_system(course: &str) -> String {
    format!(
        r##"You turn raw, incrementally written lecture notes from the course "{course}" into one clean study document.
You receive the notes as written during the lecture and the full transcript. Produce the complete replacement document in Markdown:
- Title line, then a 3-5 sentence summary of the lecture.
- Sections by topic in lecture order (`##` / `###`), merging duplicates and removing snapshot or session headings.
- Keep every substantive point, definition, formula and example; use the transcript to fill gaps and fix speech-recognition errors. Do not invent content.
- Keep every `![Slide N](...)` embed line exactly once, verbatim, next to the content it illustrates.
- End with "Key takeaways", a short "Glossary" of terms introduced, and "Questions / follow-ups" if anything is unclear.
Output Markdown only: no preamble, no code fences."##
    )
}

pub fn page_system(course: &str, budget: u32, max_slides: u32) -> String {
    let b = budget as f64;
    let (topics, glossary, questions) = (round_int(b * 0.65), round_int(b * 0.15), round_int(b * 0.2));
    format!(
        r##"You make the high-yield study page for a lecture in the course "{course}". You receive the lecture's complete notes and its slide screenshots. The notes are long; the page must not be. Its reader is a student revising for a quiz or exam who wants everything that will matter and nothing that will not. Condensing is the whole job: a short page that keeps what matters beats a complete one.

Budget: at most {budget} words of visible text in total, counting the summary, every section, the glossary, the takeaways and the questions with their answers; math counts as words. Spend about {topics} on the topic sections, {glossary} on the glossary and takeaways, and {questions} on the questions. This is a ceiling, not a target.

Choose before you write. What earns space, in order:
1. What the lecturer flagged as examinable or stressed, and what a quiz is likely to test.
2. The formulas and decision rules needed to solve problems: what each method is for, when to use it rather than its alternatives, and its assumptions.
3. At most one worked example per method, cut to the steps that carry the method, with the lecture's own numbers.
4. Terms introduced in this lecture.
Cut: reviews of earlier weeks beyond a line, digressions, logistics, repetition, the lecturer's asides and hedges, step-by-step software walkthroughs (keep only which function does what), and anything you would not put on a one-page exam sheet. Merge overlapping topics. Prefer a table, a situation-to-method map or a short list over prose; one idea per bullet; no sentence that restates another.

Slides: redraw at most {max_slides}, choosing those whose figure, table, diagram or formula block teaches faster than words, and redraw only the part that matters, not the slide's text. Text-only and title slides are never redrawn; their substance, if it earns space, goes in the text. Copy numbers and symbols exactly.

Output exactly this, in order, and nothing else: no code fences, no <script> or <style>, no style or event attributes.
<div class="keystone">…</div>: the single formula (\[ … \]) or idea (<p>) to remember if nothing else.
<p class="lede">…</p>: two or three sentences on what the lecture covered and what it lets you do.
4 to 7 <section class="part"> blocks, one per topic, each opening with an <h2>.
<section class="part"><h2>Glossary</h2><dl class="glossary">…</dl></section>: at most 12 terms, one line each.
<section class="part"><h2>Key takeaways</h2><ul class="takeaways">…</ul></section>: at most 5.
<ol class="quiz">…</ol>: 4 or 5 questions, each <p class="q">…</p><div class="answer">…</div>, answers brief with the key working.

Markup inside sections: <h3>, <p>, <ul>, <ol>, <li>, <strong>, <em>, and <table> with <thead> and <tbody>. Math is TeX, \( … \) inline and \[ … \] displayed, never Unicode symbols or entities and never inside <code>. R code goes in <pre><code>, function names in <code>.
- A formula to memorise: <div class="formula" data-name="short plain-text name">\[ … \]</div>
- A point flagged as examinable, at most 4 on the page: <aside class="flag">…</aside>
- A worked example: <div class="example"><h4>its title</h4><p>the setup</p><ol class="steps"><li>one step each, the last stating the result</li></ol></div>
- Situations mapped to methods: <dl class="map"><dt>situation</dt><dd>method</dd></dl>
- A slide redraw: <figure class="redraw" data-slide="N">…<figcaption>one sentence</figcaption></figure> holding display math, a <table> (cells of a highlighted group class="a", a second group class="b"), or one inline <svg viewBox="0 0 640 H"> without width or height attributes.
SVG: no fill, stroke, color, style or font-family attributes; draw only with these classes. Lines and text are ink by default; "muted" for axes, grid lines and secondary labels; "a" and "b" for two series or groups; "area-a" and "area-b" for shaded regions such as rejection regions; "dot" for filled points; "thick" and "dash" for emphasis and reference lines. Text is <text> with a font-size from 13 to 18, kept inside the viewBox."##
    )
}

pub fn revise_system(budget: u32) -> String {
    format!(
        r##"You shorten a study page that is over its length budget. Rewrite it to at most {budget} words of visible text, math counting as words, by cutting the lowest-yield material first: second examples, secondary detail, lesser glossary terms, extra questions, restatements. Keep its order, its markup and components exactly as they are used, every formula a problem needs, and every figure you keep unchanged. Output the complete page in the same format and nothing else."##
    )
}

/// `make_notes`'s user text: the document context (§6.1), the batch's timeline, its embed lines and the hint.
pub fn notes_user(doc_context: &str, timeline: &str, embeds: &[String], hint: &str) -> String {
    let mut user = format!("Notes document so far:\n<<<\n{doc_context}\n>>>\n\n");
    user += "New material since the last snapshot, in chronological order:\n<<<\n";
    user += &format!("{timeline}\n>>>\n\n");
    if !embeds.is_empty() {
        user += "Slide images are attached in the same order as their markers. These embed lines are mandatory, each exactly once, verbatim:\n";
        user += &format!("{}\n\n", embeds.join("\n"));
    }
    if !hint.is_empty() {
        user += &format!("Focus hint from the student: {hint}\n\n");
    }
    user += "Write only the new notes to append.";
    user
}

pub fn polish_user(title: &str, doc: &str, transcript: &str) -> String {
    format!("Use this exact title line: {title}\n\nNotes as written during the lecture:\n<<<\n{doc}\n>>>\n\nFull transcript:\n<<<\n{transcript}\n>>>\n\nWrite the complete replacement document.")
}

/// `typeset_page`'s user text; `words` is the notes' word count, `slide_numbers` the embeds' numbers in order.
pub fn page_user(doc: &str, words: usize, slide_numbers: &[String], budget: u32) -> String {
    let mut user = format!("The complete notes ({words} words):\n<<<\n{doc}\n>>>\n\n");
    if !slide_numbers.is_empty() {
        let list: Vec<String> = slide_numbers.iter().map(|n| format!("Slide {n}")).collect();
        user += &format!("Slide images are attached in this order: {}.\n\n", list.join(", "));
    }
    user += &format!("Write the study page in at most {budget} words.");
    user
}

pub fn revise_user(page: &str, words: usize, budget: u32) -> String {
    format!("The page, {words} words:\n<<<\n{page}\n>>>\n\nWrite it in at most {budget} words.")
}

/// The notes' first line and polish's title (`live_notes.py` `title`).
pub fn title(course: &str, folder: &str, date: NaiveDate) -> String {
    format!("# {course} — {folder} — {}", date.format("%Y-%m-%d"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyjson::round_int;

    fn live_notes_source() -> String {
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../live_notes.py")).expect("live_notes.py stays until M6; M6 freezes these goldens before removing it")
    }

    /// The body of `def name(…)`'s `return f"""…"""`, evaluated as Python evaluates it for these arguments.
    fn python_fstring(src: &str, name: &str, args: &[(&str, &str)]) -> String {
        let def = src.find(&format!("def {name}(")).unwrap_or_else(|| panic!("def {name} in live_notes.py"));
        let open = def + src[def..].find("return f\"\"\"").expect("an f-string body") + "return f\"\"\"".len();
        let close = open + src[open..].find("\"\"\"").expect("its end");
        let mut out = String::new();
        let mut chars = src[open..close].chars();
        while let Some(c) = chars.next() {
            match c {
                '{' => {
                    let expr: String = chars.by_ref().take_while(|&c| c != '}').collect();
                    out += &eval(&expr, args);
                }
                '\\' => match chars.next() {
                    Some('\\') => out.push('\\'),
                    other => panic!("an escape this evaluator does not know in {name}: \\{other:?}"),
                },
                c => out.push(c),
            }
        }
        out
    }

    fn eval(expr: &str, args: &[(&str, &str)]) -> String {
        if let Some((_, v)) = args.iter().find(|(k, _)| *k == expr) {
            return v.to_string();
        }
        let budget: f64 = args.iter().find(|(k, _)| *k == "budget").map(|(_, v)| v.parse().unwrap()).unwrap_or_else(|| panic!("{{{expr}}} needs the budget"));
        let factor = match expr {
            "round(budget * 0.65)" => 0.65,
            "round(budget * 0.15)" => 0.15,
            "round(budget * 0.2)" => 0.2,
            other => panic!("an expression live_notes.py gained: {{{other}}}"),
        };
        round_int(budget * factor).to_string()
    }

    #[test]
    fn system_prompts_are_live_notes_py_literals() {
        let src = live_notes_source();
        for course in ["Machine Learning", "Statistik für KI — Grundlagen"] {
            assert_eq!(notes_system(course), python_fstring(&src, "notes_system", &[("course", course)]));
            assert_eq!(polish_system(course), python_fstring(&src, "polish_system", &[("course", course)]));
            for budget in [600u32, 650, 1_150, 2_500] {
                for max_slides in [2u32, 5, 8] {
                    let (b, m) = (budget.to_string(), max_slides.to_string());
                    let expected = python_fstring(&src, "page_system", &[("course", course), ("budget", &b), ("max_slides", &m)]);
                    assert_eq!(page_system(course, budget, max_slides), expected, "{budget} {max_slides}");
                }
            }
        }
        for budget in [600u32, 2_500] {
            assert_eq!(revise_system(budget), python_fstring(&src, "revise_system", &[("budget", &budget.to_string())]));
        }
    }

    /// Hand goldens from live_notes.py's make_notes, polish_notes, typeset_page and the title line.
    #[test]
    fn user_messages_are_the_python_clis() {
        assert_eq!(
            notes_user("# T\n", "[10:00:01] hello", &["![Slide 1](slides/slide_01_100000.png)".into()], "momentum"),
            "Notes document so far:\n<<<\n# T\n\n>>>\n\nNew material since the last snapshot, in chronological order:\n<<<\n[10:00:01] hello\n>>>\n\nSlide images are attached in the same order as their markers. These embed lines are mandatory, each exactly once, verbatim:\n![Slide 1](slides/slide_01_100000.png)\n\nFocus hint from the student: momentum\n\nWrite only the new notes to append."
        );
        assert_eq!(
            notes_user("d", "t", &[], ""),
            "Notes document so far:\n<<<\nd\n>>>\n\nNew material since the last snapshot, in chronological order:\n<<<\nt\n>>>\n\nWrite only the new notes to append."
        );
        assert_eq!(
            polish_user("# C — W — 2026-09-25", "doc", "tr"),
            "Use this exact title line: # C — W — 2026-09-25\n\nNotes as written during the lecture:\n<<<\ndoc\n>>>\n\nFull transcript:\n<<<\ntr\n>>>\n\nWrite the complete replacement document."
        );
        assert_eq!(
            page_user("doc", 1, &["3".into(), "7".into()], 600),
            "The complete notes (1 words):\n<<<\ndoc\n>>>\n\nSlide images are attached in this order: Slide 3, Slide 7.\n\nWrite the study page in at most 600 words."
        );
        assert_eq!(page_user("doc", 1, &[], 600), "The complete notes (1 words):\n<<<\ndoc\n>>>\n\nWrite the study page in at most 600 words.");
        assert_eq!(revise_user("<p>x</p>", 812, 600), "The page, 812 words:\n<<<\n<p>x</p>\n>>>\n\nWrite it in at most 600 words.");
        let date = chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap();
        assert_eq!(title("Machine Learning", "Week 01 — Optimisation", date), "# Machine Learning — Week 01 — Optimisation — 2026-09-25");
    }

    /// Every fixed fragment of the user messages is still written that way in live_notes.py.
    #[test]
    fn user_message_fragments_appear_in_live_notes_py() {
        let src = live_notes_source();
        for fragment in [
            "Notes document so far:\\n<<<\\n",
            "New material since the last snapshot, in chronological order:\\n<<<\\n",
            "Slide images are attached in the same order as their markers. These embed lines are mandatory, each exactly once, verbatim:\\n",
            "Focus hint from the student: {hint}\\n\\n",
            "Write only the new notes to append.",
            "Use this exact title line: {title}\\n\\n",
            "Notes as written during the lecture:\\n<<<\\n{doc}\\n>>>\\n\\n",
            "Full transcript:\\n<<<\\n{transcript}\\n>>>\\n\\n",
            "Write the complete replacement document.",
            "The complete notes ({notes_words(doc)} words):\\n<<<\\n{doc}\\n>>>\\n\\n",
            "Slide images are attached in this order: ",
            "Write the study page in at most {budget} words.",
            "The page, {words} words:\\n<<<\\n{out}\\n>>>\\n\\nWrite it in at most {budget} words.",
            "title = f\"# {course} — {lecture_dir.name} — {today:%Y-%m-%d}\"",
        ] {
            assert!(src.contains(fragment), "live_notes.py no longer contains {fragment:?}");
        }
    }
}
