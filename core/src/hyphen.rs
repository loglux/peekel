//! Where a Russian word may be broken at the end of a line. The rules are those of P. Khristov as Dymchenko and Varsanofiev put them
//! (a break between two letters by the classes of the letters around it: vowels, consonants, and й ь ъ), with the usual two limits:
//! at least two letters on each side, and a vowel on each side. Rules and not a dictionary: a break may now and then fall where a
//! dictionary would not put it (between a prefix and a root it does not know), never in the middle of a syllable.
//!
//! A word that has no Cyrillic letter is not broken here (an English book's lines stay as they were). A hyphen already in a word
//! (`кто-то`) is a place to break too, the hyphen staying at the end of the line.

/// The class of a letter for the rules.
#[derive(Clone, Copy, PartialEq)]
enum Class {
    Vowel,
    Consonant,
    /// й, ь, ъ: kept with the letter before them.
    Sign,
    /// Not a Cyrillic letter (a quote, a comma, a digit).
    Other,
}

fn class(c: char) -> Class {
    match c.to_lowercase().next().unwrap_or(c) {
        'а' | 'е' | 'ё' | 'и' | 'о' | 'у' | 'ы' | 'э' | 'ю' | 'я' => Class::Vowel,
        'й' | 'ь' | 'ъ' => Class::Sign,
        'а'..='я' => Class::Consonant,
        _ => Class::Other,
    }
}

/// The places in `word` (indices of its chars: a break before that char) where it may be broken, in order. A place after a hyphen
/// already in the word needs no hyphen added; the others do.
pub fn points(word: &str) -> Vec<usize> {
    let chars: Vec<char> = word.chars().collect();
    if !chars.iter().any(|&c| class(c) != Class::Other) {
        return Vec::new();
    }
    let classes: Vec<Class> = chars.iter().map(|&c| class(c)).collect();
    let at = |i: isize| -> Class {
        usize::try_from(i)
            .ok()
            .and_then(|i| classes.get(i).copied())
            .unwrap_or(Class::Other)
    };
    use Class::{Consonant as S, Sign as X, Vowel as G};
    let mut out = Vec::new();
    for i in 1..chars.len() {
        let p = i as isize;
        let rule = if chars[i - 1] == '-' {
            // a hyphen in the word: letters on both sides of it
            at(p - 2) != Class::Other && at(p) != Class::Other
        } else {
            let (b2, b1, a0, a1, a2) = (at(p - 3), at(p - 2), at(p - 1), at(p), at(p + 1));
            // x | any letter
            (a0 == X && a1 != Class::Other)
            // g s | s g
            || (b1 == G && a0 == S && a1 == S && a2 == G)
            // s g | s g
            || (b1 == S && a0 == G && a1 == S && a2 == G)
            // s g | g
            || (b1 == S && a0 == G && a1 == G)
            // g s | s s g
            || (b1 == G && a0 == S && a1 == S && a2 == S && at(p + 2) == G)
            // g s s | s s g
            || (b2 == G && b1 == S && a0 == S && a1 == S && a2 == S && at(p + 2) == G)
        };
        if rule && enough(&classes[..i]) && enough(&classes[i..]) {
            out.push(i);
        }
    }
    out
}

/// A part of a word that may stand alone on a line: two letters at least, one of them a vowel.
fn enough(part: &[Class]) -> bool {
    part.iter().filter(|&&c| c != Class::Other).count() >= 2 && part.contains(&Class::Vowel)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The word with its breaks shown by `-`.
    fn shown(word: &str) -> String {
        let breaks = points(word);
        word.chars()
            .enumerate()
            .flat_map(|(i, c)| {
                let mark = breaks.contains(&i) && !word.chars().nth(i.wrapping_sub(1)).is_some_and(|p| p == '-');
                mark.then_some('-').into_iter().chain(std::iter::once(c))
            })
            .collect()
    }

    #[test]
    fn russian_words_are_broken_between_syllables() {
        assert_eq!(shown("капитанская"), "ка-пи-тан-ская");
        assert_eq!(shown("дочка"), "доч-ка");
        assert_eq!(shown("чтение"), "чте-ние");
        assert_eq!(shown("война"), "вой-на");
        assert_eq!(shown("подъезд"), "подъ-езд");
        assert_eq!(shown("Пугачёв"), "Пу-га-чёв");
    }

    #[test]
    fn short_words_and_one_letter_left_alone_are_not_broken() {
        assert!(points("он").is_empty());
        assert!(points("мама").len() == 1);
        // "о-зеро" would leave one letter
        assert_eq!(shown("озеро"), "озе-ро");
        // no vowel on one side
        assert!(points("встр").is_empty());
    }

    #[test]
    fn punctuation_does_not_count_and_english_is_left_as_it_is() {
        assert_eq!(shown("«дочка»,"), "«доч-ка»,");
        assert!(points("hyphenation").is_empty());
        assert!(points("").is_empty());
        assert!(points("1999").is_empty());
    }

    #[test]
    fn a_hyphen_in_the_word_is_a_place_to_break() {
        // a break right after the hyphen, and none added there
        let word = "кто-нибудь";
        assert!(points(word).contains(&4));
        assert_eq!(shown(word), "кто-ни-будь");
    }
}
