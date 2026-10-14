//! The prompts. The notes and polish prompts are carried over unchanged from `live_notes.py` (spec §6.1) and pinned to
//! its source by the tests; the study page's own prompts (spec §6.4) are this project's design and are not.
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
        r##"You build the study page for a lecture in the course "{course}". You receive the lecture's complete notes and, when the lecture had slides, their screenshots. The reader is a student who learns by seeing and doing: they open this page after class, drag things, watch what changes, and read the words only to confirm what they just saw. A page that is mostly paragraphs and bullets has failed, however accurate. The notes stay the complete record; this page is where the ideas become obvious.

How each topic is taught, in this order:
1. A visual that carries the idea. Almost always an interactive widget: the student changes a parameter, a threshold, a sample or a choice and watches the result move. Where a widget cannot show it, a drawn figure.
2. The technical explanation, short: what it is, the formula or rule, when it applies and when it does not, in under about 90 words besides its formulas. Do not describe in words what the visual already shows.
3. "Explained like I'm five", straight after each technical explanation: a child's picture of the same idea. One concrete comparison from a child's world (marbles, a seesaw, a lunch queue), one or two short sentences, no symbols, no jargon.
4. "In everyday terms", straight after that: the same idea for an intelligent adult outside the field. A real situation they know (a referee, a panel of doctors, guessing a language from an accent), two or three sentences, still no formulas. The two blocks are for different readers, so they are never the same text at two lengths. Both are about this idea, not a generic one, and may point at the visual ("drag the slider and watch the red area grow").

Every topic section has at least one widget or figure. The page has at least four widgets whenever the lecture has ideas that change with a parameter, a threshold, a sample size or a choice, which is nearly always. A topic that cannot be shown is merged into one that can, or cut.

What earns a section, in order: what the lecturer flagged or stressed, and what a quiz is likely to test; the methods, formulas and decision rules needed to solve problems, and when each applies rather than its alternatives; the lecture's own worked example, as a widget when its numbers can move; terms introduced in this lecture. Cut reviews of earlier weeks, logistics, repetition, asides and software walkthroughs.

Budget: at most {budget} words of visible text in total, counting the lede, every section, the explained-like-I'm-five and everyday-terms blocks, the glossary, the takeaways and the questions with their answers; math counts as words, widget code and drawings do not. Spend about {topics} on the topic sections, {glossary} on the glossary and takeaways, and {questions} on the questions. This is a ceiling, not a target: a visual replaces words.

Course materials: when the request carries a notebook and datasets from the lecture folder, they are what the class actually ran. Read the notebook for what was computed and which numbers it printed; draw the datasets in widgets with w.data, so a figure shows what the student has already seen in their own notebook. Respect the exam rules the notebook states (a test set is opened once, never tuned on).

Slides: redraw at most {max_slides}, choosing those whose figure, table, diagram or formula block teaches faster than words. A redraw whose numbers can move is better as a widget. Text-only and title slides are never redrawn. Copy numbers and symbols exactly.

Output exactly this, in order, and nothing else: no code fences, no <style>, no style or event attributes, and no <script> except the one inside a widget.
<div class="keystone">…</div>: the one thing to remember if nothing else: a single formula (\[ … \]), or one <p> of at most 20 words.
<p class="lede">…</p>: two sentences on what the lecture covered and what it lets you do.
5 to 8 <section class="part"> blocks, one per topic, each opening with an <h2> that states the claim ("Lower cut-offs catch more disease and raise more false alarms"), not the subject.
<section class="part"><h2>Glossary</h2><dl class="glossary">…</dl></section>: at most 12 terms, one line each.
<section class="part"><h2>Key takeaways</h2><ul class="takeaways">…</ul></section>: at most 5.
<ol class="quiz">…</ol>: 4 or 5 questions, each <p class="q">…</p><div class="answer">…</div>, specific and answerable, each answer leading with the result.

Widgets. One per interactive figure, inside a section:
<figure class="widget" data-title="what the student can move, in a few words" data-aspect="16/9"><figcaption>Try: one sentence saying what to do and what to notice.</figcaption><script type="text/widget">…JavaScript…</script></figure>
data-aspect is the canvas's width/height (default 16/9; 2/1 or 3/1 for wide, thin plots).
The script is the body of a function that receives `w` and nothing else: no network, no page access, no libraries. It runs once, declares its controls, and sets one draw function.
w.slider(id, {{label, min, max, value, step, log, format}}) adds a slider; w.v[id] is its number. log:true spaces it logarithmically (min above 0). format(x) returns the text shown beside it.
w.toggle(id, {{label, value}}) adds a checkbox; w.v[id] is a boolean.
w.choice(id, {{label, options: ["a", "b"] or [{{value, label}}], value}}) adds a row of buttons; w.v[id] is the chosen value.
w.button(label, fn) adds a button that runs fn(v) and then redraws; keep any state in your own variables.
w.readout(label, fn) shows a live number or phrase under the figure: fn(v) returns it, recomputed on every redraw. Put the quantity the lecture cares about here.
w.draw(fn) is required. fn(ctx, v, W, H) paints the whole figure on every control change, resize and theme change. ctx is a 2D canvas context; W and H are in CSS pixels. Nothing persists between calls, so recompute everything from v and your own variables.
w.click(fn) is optional: fn(x, y, v) runs when the student presses or drags on the canvas, in CSS pixels. Store what you need in your own variable and read it in draw.
w.frame({{x: [lo, hi], y: [lo, hi], xlabel, ylabel, xticks, yticks}}) paints axes and a light grid when called inside draw (xticks: false or yticks: false leaves that axis without numbers, for bars or categories you label yourself), and returns {{X, Y, ix, iy, left, right, top, bottom}}: X(x) and Y(y) map data to pixels, ix(px) and iy(py) map back. What you draw after it is clipped to the plot area. Called outside draw (in a click handler, to turn a pixel into data) it paints nothing and only maps.
w.data(name) returns the rows of a course dataset (listed under "Course materials" below) as an array of objects keyed by column, numeric columns as numbers. Use it instead of inventing data wherever a dataset is given.
w.shared is one object every widget on the page can read, and w.share(key, value) sets a key in it and redraws every widget. When the lecture follows one running case through several ideas (one patient, one tumour, one pair of measurements), keep that case in w.shared.probe so a click in any widget moves it in all of them; each widget reads it with a default.
w.rng(seed) returns a seeded random function (0 to 1). Use it for any random data, with a fixed seed, so the figure is the same on every visit.
w.c holds colours that follow light and dark: ink, muted, grid, bg, a (red), b (teal), aSoft, bSoft. Use a and b for two groups or two models, the same way throughout the page. Never write a colour yourself.

A widget that works:
- It means something before anyone touches it: the starting values are the lecture's own example, and the first frame already shows the idea.
- One idea, labels written next to the shapes they name (no legend to decode), at most three controls, and a readout with the numbers that change.
- Use the lecture's own numbers and examples, and the course dataset where one is given. Where there is neither, invent small illustrative data and say "Illustrative data" in the caption; never present invented data as a result.
- The mathematics matches the notes: when a widget shows a formula's result, compute it with that formula.
- Honesty. A number the class's notebook printed is shown as the notebook's, in a table or a sentence naming it, and is never recomputed in a widget and presented as that result. A model a widget fits in the browser is a re-implementation: say so in the caption, and never present its output as the notebook's result. Words beside a widget are true of that widget, so claim "can" or "usually" unless the widget demonstrates it. When two numbers are compared, give the size of the difference and word the message from it. When a difference is about one case, say what one case is worth.
- Plain JavaScript and Math. Each draw takes under 20 ms (at most a few thousand points or grid cells). No eval, no timers, no DOM access, no external code. Text on the canvas is 12 to 14 px and stays inside it at 320 px wide.

A complete widget:
<figure class="widget" data-title="Move the cut-off and trade sensitivity for specificity" data-aspect="2/1"><figcaption>Try: slide the cut-off right. Fewer healthy people are flagged, and more sick people are missed. Illustrative data.</figcaption><script type="text/widget">
const gauss = (x, m) => Math.exp(-0.5 * (x - m) ** 2) / 2.5066;
const cdf = (z) => {{ const t = 1 / (1 + 0.2316419 * Math.abs(z)), d = 0.3989423 * Math.exp(-z * z / 2), p = d * t * (0.3193815 + t * (-0.3565638 + t * (1.781478 + t * (-1.821256 + t * 1.330274)))); return z > 0 ? 1 - p : p; }};
w.slider("cut", {{ label: "Cut-off", min: -1, max: 6, value: 2.5, step: 0.05 }});
w.readout("Sensitivity", (v) => (1 - cdf(v.cut - 4)).toFixed(2));
w.readout("Specificity", (v) => cdf(v.cut - 1).toFixed(2));
w.draw((ctx, v, W, H) => {{
  const f = w.frame({{ x: [-2, 7], y: [0, 0.5], xlabel: "test value" }});
  ctx.lineWidth = 2;
  [[1, w.c.b, "healthy"], [4, w.c.a, "disease"]].forEach(([m, col, name]) => {{
    ctx.strokeStyle = ctx.fillStyle = col;
    ctx.beginPath();
    for (let x = -2; x <= 7.001; x += 0.05) {{ const px = f.X(x), py = f.Y(gauss(x, m)); x === -2 ? ctx.moveTo(px, py) : ctx.lineTo(px, py); }}
    ctx.stroke();
    ctx.fillText(name, f.X(m) - 20, f.Y(0.44));
  }});
  ctx.strokeStyle = w.c.ink;
  ctx.setLineDash([5, 4]);
  ctx.beginPath(); ctx.moveTo(f.X(v.cut), f.top); ctx.lineTo(f.X(v.cut), f.bottom); ctx.stroke();
}});
</script></figure>

Static markup inside sections: <h3>, <p>, <ul>, <ol>, <li>, <strong>, <em>, and <table> with <thead> and <tbody>. Prose is for the technical explanation and stays short; bullets only for parallel items, a table only when items are compared along the same dimensions. Math is TeX, \( … \) inline and \[ … \] displayed, never Unicode symbols or entities and never inside <code>. R code goes in <pre><code>, function names in <code>.
- Explained like I'm five: <aside class="eli5"><p>…</p></aside>, then In everyday terms: <aside class="everyday"><p>…</p></aside>
- A formula to memorise: <div class="formula" data-name="short plain-text name">\[ … \]</div>
- A point flagged as examinable, at most 4 on the page: <aside class="flag">…</aside>
- A worked example that does not need a widget: <div class="example"><h4>its title</h4><p>the setup</p><ol class="steps"><li>one step each, the last stating the result</li></ol></div>
- Situations mapped to methods: <dl class="map"><dt>situation</dt><dd>method</dd></dl>
- A slide redraw: <figure class="redraw" data-slide="N">…<figcaption>one sentence</figcaption></figure>; a diagram of your own is the same without data-slide. Either holds display math, a <table> (cells of a highlighted group class="a", a second group class="b"), or one inline <svg viewBox="0 0 640 H"> without width or height attributes.
SVG: no fill, stroke, color, style or font-family attributes; draw only with these classes. Lines and text are ink by default; "muted" for axes, grid lines and secondary labels; "a" and "b" for two series or groups; "area-a" and "area-b" for shaded regions such as rejection regions; "dot" for filled points; "thick" and "dash" for emphasis and reference lines. Text is <text> with a font-size from 13 to 18, kept inside the viewBox.

Write the words as part of the design. Sentence case; plain verbs and the active voice; each element does one job; no section opens by restating its heading; ground every sentence in this lecture's own terms, numbers and examples, not generic study-guide filler."##
    )
}

pub fn revise_system(budget: u32) -> String {
    format!(
        r##"You shorten a study page that is over its length budget. Rewrite it to at most {budget} words of visible text, math counting as words and widget code and drawings not counting, by cutting the lowest-yield prose first: second examples, secondary detail, lesser glossary terms, extra questions, restatements. Keep its order, its markup and components exactly as they are used, every formula a problem needs, a plain-words block for every technical explanation that stays, and every widget and figure unchanged, widget scripts included, character for character. Output the complete page in the same format and nothing else."##
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

    /// What the goldens read: live_notes.py's prompts and user-message lines, verbatim, frozen before its removal (M6).
    fn excerpt() -> String {
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/prompts/live_notes_excerpt.txt")).expect("the frozen excerpt of live_notes.py")
    }

    /// The excerpt is live_notes.py verbatim, block by block, while live_notes.py exists (removed with it in M6 Task 17).
    #[test]
    fn the_frozen_excerpt_is_live_notes_py_verbatim() {
        let py = live_notes_source();
        for block in excerpt().split("\n\n").skip(1) {
            assert!(py.contains(block.trim_end_matches('\n')), "not in live_notes.py:\n{block}");
        }
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
    fn the_notes_and_polish_prompts_are_live_notes_py_literals() {
        let src = excerpt();
        for course in ["Machine Learning", "Statistik für KI — Grundlagen"] {
            assert_eq!(notes_system(course), python_fstring(&src, "notes_system", &[("course", course)]));
            assert_eq!(polish_system(course), python_fstring(&src, "polish_system", &[("course", course)]));
        }
    }

    /// The study page's prompt is its own design (visual first, interactive, plain words after each technical
    /// explanation), no longer the Python CLI's condensing brief: its parts are all there, every placeholder is filled,
    /// and its worked widget is one the sanitiser keeps whole.
    #[test]
    fn the_page_prompt_asks_for_widgets_and_plain_words_and_its_example_survives_the_sanitiser() {
        let p = page_system("Machine Learning", 1_000, 5);
        assert!(p.contains("\"Machine Learning\"") && p.contains("at most 1000 words") && p.contains("redraw at most 5"), "course, budget and slides are filled in");
        assert!(p.contains("about 650 on the topic sections, 150 on the glossary and takeaways, and 200 on the questions"), "the budget's split is filled in");
        assert!(!p.contains("{budget}") && !p.contains("{topics}") && !p.contains("{{") && !p.contains("}}"), "every placeholder and escape was resolved");
        for part in ["class=\"eli5\"", "class=\"everyday\"", "w.data(", "w.shared", "w.share(", "Honesty.", "Course materials:", "class=\"widget\"", "text/widget", "w.slider(", "w.toggle(", "w.choice(", "w.button(", "w.readout(", "w.draw(", "w.click(", "w.frame(", "w.rng(", "w.c ", "class=\"keystone\"", "at least four widgets"] {
            assert!(p.contains(part), "the prompt lost {part:?}");
        }
        let start = p.find("<figure class=\"widget\" data-title=\"Move the cut-off").expect("the worked example");
        let end = start + p[start..].find("</figure>").unwrap() + "</figure>".len();
        let example = &p[start..end];
        assert_eq!(crate::notes::page::strip_unsafe(example), example, "scripts, and the code in them, are kept");
        let revise = revise_system(900);
        assert!(revise.contains("at most 900 words") && revise.contains("widget scripts included"), "{revise}");
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
        let src = excerpt();
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
