//! A search in the text of a book: which pages have a word, and which hit to go to next. Pure arithmetic over the text, without a
//! screen: the viewer and Peekel TV (an Android app on this core) share it.

/// The text of a book made ready for a search: for each page, in lower case, with every run of white space and of the separators that
/// DjVu puts between columns and paragraphs (US, GS, VT, FF) made one space.
#[derive(Debug, Default)]
pub struct Index {
    pages: Vec<String>,
}

/// `text` as the search sees it: lower case, one space for each run of white space or separators. The same as the words of a page are
/// made for the marking, so that what is found is what is marked.
pub use crate::djvu::normalise;

impl Index {
    pub fn new(texts: &[String]) -> Self {
        Self {
            pages: texts.iter().map(|t| normalise(t)).collect(),
        }
    }

    /// The pages (from 0) whose text has `needle` in it, in order. The case of the letters and the kind of the spaces do not matter.
    pub fn pages_with(&self, needle: &str) -> Vec<usize> {
        let needle = normalise(needle);
        if needle.is_empty() {
            return Vec::new();
        }
        self.pages
            .iter()
            .enumerate()
            .filter(|(_, page)| page.contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }
}

/// The hit to go to from `current` in a list of hits: the next one (or the previous, with `forward` false) whose spread is not the one
/// that is on the screen (`spread_of` gives the first page of the spread of a page), going round at the ends. `current` is the index of
/// the hit last shown, if any. None when every hit is in the spread on the screen.
pub fn next_hit(
    hits: &[usize],
    current: Option<usize>,
    on_screen: usize,
    forward: bool,
    spread_of: impl Fn(usize) -> usize,
) -> Option<usize> {
    let count = hits.len();
    if count == 0 {
        return None;
    }
    // where to begin looking: just after (or before) the hit last shown, else the first hit after (before) the spread on screen
    let first = match (current, forward) {
        (Some(c), true) => c + 1,
        (Some(c), false) => c + count - 1,
        (None, true) => hits.partition_point(|&p| spread_of(p) <= on_screen),
        (None, false) => hits.partition_point(|&p| spread_of(p) < on_screen) + count - 1,
    };
    (0..count)
        .map(|step| (first + step) % count)
        .find(|&i| hits.get(i).is_some_and(|&p| spread_of(p) != on_screen))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn the_case_the_kind_of_space_and_the_separators_do_not_matter() {
        let index = Index::new(&texts(&[
            "Начало\nглавы",
            "второе слово: Интеграл\u{1f}\u{b}Римана",
            "",
            "ИНТЕГРАЛ  римана",
        ]));
        assert_eq!(index.pages_with("интеграл римана"), [1, 3]);
        assert_eq!(index.pages_with("Начало  главы"), [0]);
        assert_eq!(index.pages_with("нету"), Vec::<usize>::new());
        assert_eq!(index.pages_with("  "), Vec::<usize>::new(), "nothing to look for");
    }

    #[test]
    fn the_next_hit_skips_the_ones_in_the_spread_on_the_screen_and_goes_round() {
        let hits = [2, 3, 9, 20];
        // two pages side by side, the cover alone: 0 | 1 2 | 3 4 | ...
        let spread = |p: usize| if p == 0 { 0 } else { 1 + (p - 1) / 2 * 2 };
        // on the spread 1-2: the hit at page 2 is on it, the next is page 3 (spread 3-4)
        assert_eq!(next_hit(&hits, None, 1, true, spread), Some(1));
        // having shown the hit at 3, the next is 9, then 20, then round to 2
        assert_eq!(next_hit(&hits, Some(1), 3, true, spread), Some(2));
        assert_eq!(next_hit(&hits, Some(3), 19, true, spread), Some(0));
        // backwards from 3: the hit at 2 (spread 1-2), then round to 20
        assert_eq!(next_hit(&hits, Some(1), 3, false, spread), Some(0));
        assert_eq!(next_hit(&hits, Some(0), 1, false, spread), Some(3));
        // every hit on the spread that is shown: nowhere to go
        assert_eq!(next_hit(&[1, 2], None, 1, true, spread), None);
        assert_eq!(next_hit(&[], None, 0, true, spread), None);
    }
}
