//! How much of the notes a snapshot request carries (spec §6.1).

/// `grok-4.7`'s long-context threshold: above it the whole request is billed at twice the rate (plan research).
pub const BUDGET_TOKENS: usize = 200_000;
/// Reasoning plus the block the model writes.
pub const OUTPUT_TOKENS: usize = 16_000;
/// A slide of up to 1600 px.
pub const IMAGE_TOKENS: usize = 1_800;
/// Characters per token for text: conservative for English with Markdown and TeX.
const CHARS_PER_TOKEN: usize = 3;

pub const OMITTED: &str = "[... earlier part of the document omitted; its headings follow ...]";
pub const RECENT: &str = "[... the most recent part of the document, verbatim ...]";

pub fn text_tokens(s: &str) -> usize {
    s.chars().count().div_ceil(CHARS_PER_TOKEN)
}

/// The whole document while the request fits the budget; otherwise the omitted prefix's headings,
/// then the most recent part verbatim from a line start. Sliced by lines, so never inside a character.
pub fn doc_context(doc: &str, rest_tokens: usize, budget: usize) -> String {
    let room = budget.saturating_sub(rest_tokens + OUTPUT_TOKENS);
    if text_tokens(doc) <= room {
        return doc.to_string();
    }
    let with_cut = |cut: usize| {
        let (head, tail) = doc.split_at(cut);
        let outline: Vec<&str> = head.lines().filter(|l| l.starts_with('#')).collect();
        format!("{OMITTED}\n{}\n{RECENT}\n{tail}", outline.join("\n"))
    };
    let starts: Vec<usize> = std::iter::once(0).chain(doc.match_indices('\n').map(|(i, _)| i + 1)).filter(|&i| i < doc.len()).collect();
    // Later cuts omit more and carry less: the first that fits keeps the most.
    let first_fit = starts.partition_point(|&cut| text_tokens(&with_cut(cut)) > room);
    match starts.get(first_fit) {
        Some(&cut) => with_cut(cut),
        None => {
            let outline: Vec<&str> = doc.lines().filter(|l| l.starts_with('#')).collect();
            format!("{OMITTED}\n{}", outline.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_that_fits_is_sent_whole() {
        let doc = "# T\n\n## A\n- x\n";
        assert_eq!(doc_context(doc, 1_000, BUDGET_TOKENS), doc);
    }

    #[test]
    fn over_budget_the_prefix_becomes_its_outline_and_the_rest_stays_verbatim() {
        let mut doc = String::from("# Title\n");
        for i in 0..400 {
            doc += &format!("## Topic {i}\n- a point about é—𝛼 number {i}\n");
        }
        let room = 5_000;
        let out = doc_context(&doc, 1_000, OUTPUT_TOKENS + 1_000 + room);
        assert!(text_tokens(&out) <= room, "{} tokens", text_tokens(&out));
        let (head, tail) = out.split_once(&format!("{RECENT}\n")).expect("the recent part is marked");
        assert!(head.starts_with(OMITTED));
        assert!(doc.ends_with(tail), "the recent part is the document's own tail");
        let omitted = &doc[..doc.len() - tail.len()];
        assert!(omitted.ends_with('\n'), "the cut falls at a line start");
        let expected: Vec<&str> = omitted.lines().filter(|l| l.starts_with('#')).collect();
        assert_eq!(head.lines().skip(1).collect::<Vec<_>>(), expected, "the outline is the omitted part's headings");
        assert!(tail.len() > 6_000, "the budget keeps a real recent part");
    }

    #[test]
    fn tokens_count_characters_not_bytes() {
        assert_eq!(text_tokens("ééé"), 1);
        assert_eq!(text_tokens("𝛼𝛼𝛼𝛼"), 2);
        assert_eq!(text_tokens(""), 0);
    }
}
