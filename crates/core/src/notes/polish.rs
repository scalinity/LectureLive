//! Polish (spec §6.3): the notes and the whole transcript rewritten into one study document.
use std::time::Duration;

use crate::notes::chat::{ChatRequest, Content};
use crate::notes::embeds::{clean_output, embeds_in, repair, Repaired};
use crate::notes::prompts::{polish_system, polish_user};
use crate::session::spend::SpendKind;

pub fn request(course: &str, title: &str, doc: &str, transcript: &str) -> ChatRequest {
    ChatRequest { what: SpendKind::Polish, system: polish_system(course), content: Content::Text(polish_user(title, doc, transcript)), effort: None, timeout: Duration::from_secs(600) }
}

/// The document as it will be written: output cleaned with its title kept, every embed of the notes exactly once.
pub fn validate(output: &str, doc: &str) -> Repaired {
    repair(&clean_output(output, false), &embeds_in(doc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::embeds::NOT_PLACED;

    #[test]
    fn the_polished_notes_keep_the_title_and_every_embed_once() {
        let (s1, s2) = ("![Slide 1](slides/slide_01_100000.png)", "![Slide 2](slides/slide_02_100500.png)");
        let doc = format!("# T\n\n<!-- 10:00:10 -->\n## A\n{s1}\n- a\n\n<!-- 10:05:10 -->\n{s2}\n- b\n");
        let out = format!("```markdown\n# T\n\nSummary.\n\n## A\n{s1}\n- a\n{s1}\n```");
        let r = validate(&out, &doc);
        assert_eq!(r.text, format!("# T\n\nSummary.\n\n## A\n{s1}\n- a\n\n{NOT_PLACED}\n\n{s2}"));
    }

    #[test]
    fn a_polish_request_is_the_clis() {
        let r = request("Machine Learning", "# ML — W — 2026-09-25", "doc", "tr");
        assert_eq!(r.what, SpendKind::Polish);
        assert_eq!(r.system, crate::notes::prompts::polish_system("Machine Learning"));
        assert!(matches!(&r.content, Content::Text(t) if *t == crate::notes::prompts::polish_user("# ML — W — 2026-09-25", "doc", "tr")));
        assert_eq!((r.effort, r.timeout), (None, Duration::from_secs(600)));
    }
}
