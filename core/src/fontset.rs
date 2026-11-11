//! Which fonts the text of a drawing, a card or a page of Word is set in, and where they are looked for.
//!
//! The layout of Word, Excel and PowerPoint is tuned to Calibri, Arial and the like, which a Linux system has not, and which cannot be
//! shipped or fetched by the program. Three things stand in for them:
//!
//! - the free fonts that have the same letter widths as the common ones (SIL OFL; measured, `docs/backlog.md` item 5): Carlito for
//!   Calibri, Liberation Sans, Serif and Mono for Arial, Times New Roman and Courier New. Selawik stands for Segoe UI (its Latin letters
//!   are as wide, it has no Cyrillic: the next family of the list gives those) and Caladea for Cambria (a look-alike only: its widths
//!   differ). They are named after the family wanted, in the list that `font-family` takes, so that a machine that has the original
//!   uses it and one that has not uses the free one;
//! - a folder of the user's own fonts (the originals copied from their Windows or Office), `PEEKEL_FONTS` or the `fonts` line of the
//!   settings, looked in first;
//! - the folder of the program's own for fonts it was given (`peekel/fonts` in the user's data folder), the folder `fonts` beside the
//!   program (the Linux archive brings the free ones there), then the fonts of the system.

use std::path::PathBuf;
use std::sync::Mutex;

/// The family wanted and the free families that stand for it, in the order they are tried.
const ANALOGUES: &[(&str, &[&str])] = &[
    ("Calibri", &["Carlito"]),
    ("Cambria", &["Caladea"]),
    ("Arial", &["Liberation Sans"]),
    ("Times New Roman", &["Liberation Serif"]),
    ("Courier New", &["Liberation Mono"]),
    ("Segoe UI", &["Selawik", "Liberation Sans"]),
];

/// The families that stand for `family` (capitals do not matter), none when it is not one that has an analogue.
pub fn analogues(family: &str) -> &'static [&'static str] {
    ANALOGUES
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(family.trim()))
        .map_or(&[], |(_, free)| *free)
}

/// `family` followed by its free analogues, as the list a `font-family` takes: `Calibri, Carlito`. The name is not changed.
pub fn with_analogues(family: &str) -> String {
    let mut list = family.to_string();
    for free in analogues(family) {
        list.push_str(", ");
        list.push_str(free);
    }
    list
}

/// The families of a list (`Segoe UI, Arial, sans-serif`) each followed by its analogues, those already in the list left where they are.
pub fn expand(list: &str) -> String {
    let names: Vec<&str> = list.split(',').map(str::trim).filter(|n| !n.is_empty()).collect();
    let mut out: Vec<String> = Vec::new();
    for name in &names {
        for candidate in std::iter::once(*name).chain(analogues(name).iter().copied()) {
            let named_later = !out.iter().any(|o| o.eq_ignore_ascii_case(candidate));
            if named_later {
                out.push(candidate.to_string());
            }
        }
    }
    out.join(", ")
}

/// The folders the viewer was told of by its settings, looked in after `PEEKEL_FONTS` and before the program's own.
static GIVEN: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Tells which folders the user's own fonts are in (the viewer reads them from its settings); to be called before the first text is drawn.
pub fn set_user_dirs(dirs: Vec<PathBuf>) {
    if let Ok(mut given) = GIVEN.lock() {
        *given = dirs;
    }
}

/// The program's own folder of fonts: `peekel/fonts` in the user's data folder (`%LOCALAPPDATA%` on Windows, `XDG_DATA_HOME` or
/// `~/.local/share` elsewhere).
pub fn own_dir() -> Option<PathBuf> {
    let given = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(windows) {
        given("LOCALAPPDATA")
    } else {
        given("XDG_DATA_HOME").or_else(|| given("HOME").map(|h| h.join(".local").join("share")))
    };
    base.map(|d| d.join("peekel").join("fonts"))
}

/// The folder `fonts` beside the program: the free fonts the Linux archive brings with it.
pub fn beside_program() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("fonts")))
}

/// The folders to read fonts from, the first the most wanted: those of `PEEKEL_FONTS` (a list the way `PATH` is), those the settings gave,
/// the program's own, the one beside the program. Only those that are folders.
pub fn dirs() -> Vec<PathBuf> {
    let mut all: Vec<PathBuf> = Vec::new();
    if let Some(list) = std::env::var_os("PEEKEL_FONTS") {
        all.extend(std::env::split_paths(&list));
    }
    if let Ok(given) = GIVEN.lock() {
        all.extend(given.iter().cloned());
    }
    all.extend(own_dir());
    all.extend(beside_program());
    all.retain(|d| d.is_dir());
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_common_office_font_is_followed_by_the_free_font_of_its_width() {
        assert_eq!(with_analogues("Calibri"), "Calibri, Carlito");
        assert_eq!(with_analogues("arial"), "arial, Liberation Sans");
        assert_eq!(with_analogues("Segoe UI"), "Segoe UI, Selawik, Liberation Sans");
    }

    #[test]
    fn a_font_with_no_analogue_is_left_as_it_is() {
        assert_eq!(with_analogues("Comic Sans MS"), "Comic Sans MS");
        assert_eq!(with_analogues(""), "");
    }

    #[test]
    fn a_list_gets_the_analogues_after_each_family_and_none_twice() {
        assert_eq!(
            expand("Segoe UI, Arial, sans-serif"),
            "Segoe UI, Selawik, Liberation Sans, Arial, sans-serif"
        );
        assert_eq!(
            expand("Calibri, Carlito, Arial"),
            "Calibri, Carlito, Arial, Liberation Sans"
        );
        assert_eq!(expand(""), "");
    }

    #[test]
    fn only_folders_that_are_there_are_listed() {
        let there = std::env::temp_dir();
        let missing = there.join("peekel-no-such-fonts-folder");
        set_user_dirs(vec![there.clone(), missing.clone()]);
        let all = dirs();
        set_user_dirs(Vec::new());
        assert!(all.contains(&there));
        assert!(!all.contains(&missing));
    }

    #[test]
    #[allow(clippy::expect_used)] // a test that cannot make its folder should stop at once
    fn the_folder_fonts_beside_the_program_is_read_when_it_is_there() {
        let beside = beside_program().expect("the test program has a folder");
        let made = !beside.exists();
        if made {
            std::fs::create_dir(&beside).expect("a folder beside the test program");
        }
        let all = dirs();
        if made {
            let _ = std::fs::remove_dir(&beside);
        }
        assert!(all.contains(&beside), "{beside:?} not in {all:?}");
        assert_eq!(beside.file_name().and_then(|n| n.to_str()), Some("fonts"));
    }
}
