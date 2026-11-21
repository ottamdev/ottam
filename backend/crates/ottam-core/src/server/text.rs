//! Text cut to fit: a sidebar title, a note to a parent, a banner's detail.

use unicode_segmentation::UnicodeSegmentation;

/// `text` trimmed and cut to `limit` grapheme clusters, ending in "…" when
/// cut, so a cut never splits an emoji or an accented letter.
pub(crate) fn excerpt(text: &str, limit: usize) -> String {
    let text = text.trim();
    let mut graphemes = text.graphemes(true);
    let cut: String = graphemes.by_ref().take(limit).collect();
    if graphemes.next().is_some() {
        format!("{}…", cut.trim_end())
    } else {
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::excerpt;

    #[test]
    fn an_excerpt_cuts_whole_graphemes() {
        assert_eq!(excerpt("  short  ", 10), "short");
        assert_eq!(excerpt("one two three", 4), "one…");
        let family = "👨‍👩‍👧";
        assert_eq!(excerpt(&family.repeat(3), 2), format!("{family}{family}…"));
        let accented = "e\u{301}";
        assert_eq!(excerpt(&accented.repeat(3), 1), format!("{accented}…"));
    }
}
