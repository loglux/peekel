//! The text of the "Peekel: formats" box (F11): what the plugin opens by itself, and what it finds in this Windows. With WIC the
//! set of formats depends on the machine, so the user can see here what it is on theirs (docs/internal/formats.md).

use crate::draw::core_reads;
use crate::wic::Codec;

/// The longest line of the box, in characters.
const WIDTH: usize = 66;

/// At most this many extensions of one decoder are listed (a RAW decoder has thirty-six).
const MOST_EXTENSIONS: usize = 12;

/// `words` laid out in lines of at most `WIDTH` characters, each line after `first` starting with `indent`.
fn wrap(first: &str, indent: &str, words: &[String]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = first.to_string();
    let mut on_line = 0;
    for word in words {
        if on_line > 0 && line.len() + 1 + word.len() > WIDTH {
            lines.push(std::mem::replace(&mut line, indent.to_string()));
            on_line = 0;
        }
        if !line.is_empty() && !line.ends_with(' ') {
            line.push(' ');
        }
        line.push_str(word);
        on_line += 1;
    }
    lines.push(line);
    lines
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

/// The Store extensions that give Windows a codec for a format `core` does not read, by file extension (no dot, lower case):
/// the names as Microsoft's pages have them (docs/internal/wic.md: the JPEG XL Image Extension and the HEIF Image Extension are
/// Store products; the HEIF page says the HEVC and AV1 codecs "might not be available on all PCs" and come from the Store).
/// Only these are named; no other file gets a hint.
fn store_extensions(extension: &str) -> Option<&'static str> {
    match extension {
        "jxl" => Some("the JPEG XL Image Extension"),
        "heic" | "heif" | "hif" | "avci" | "heics" | "heifs" => Some("the HEIF Image Extension and an HEVC codec"),
        "avif" | "avifs" => Some("the HEIF Image Extension and an AV1 codec"),
        _ => None,
    }
}

/// What the card of a file that no codec opened may say: what was seen (Windows has no codec for it, or lists one that did not
/// start) and which Store extension may give it. A possibility, not a cause: the extension of the file is one of a Store codec,
/// that is all that is known. `why` is the reason the file was not read as a picture. None for any other file.
pub fn codec_hint(path: &std::path::Path, why: &str) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    hint_for(&extension, crate::wic::supports(&extension), why)
}

/// `codec_hint` for an extension (lower case) and what is known of this Windows: whether a decoder of it is in the list.
fn hint_for(extension: &str, listed: bool, why: &str) -> Option<String> {
    let names = store_extensions(extension)?;
    if !listed {
        return Some(format!(
            "Windows has no codec for it; {names} (Microsoft Store) may add one. Ctrl+Alt+S opens the Store"
        ));
    }
    if crate::wic::is_component_failure(why) {
        return Some(format!(
            "Windows lists a codec but it did not start (0x88982F8B); {names} (Microsoft Store) may fix it. Ctrl+Alt+S opens the Store"
        ));
    }
    None // the codec is there: the file is damaged, or the card was asked for
}

/// The Store product that gives Windows the codec for files of this extension, by its Store product ID. Each ID was checked on its
/// own page (apps.microsoft.com/detail/<id>: "JPEG XL Image Extension", "HEIF Image Extension"), and the Photos app of Windows
/// carries the same two in its own table of Store links (docs/internal/wic.md). The first extension named in `store_extensions`
/// is the one the key opens (a HEIC also wants an HEVC codec, an AVIF an AV1 one: those have pages of their own).
fn store_product(extension: &str) -> Option<&'static str> {
    match extension {
        "jxl" => Some("9MZPRTH5C0TB"),
        "heic" | "heif" | "hif" | "avci" | "heics" | "heifs" | "avif" | "avifs" => Some("9PMMSR1CGPWG"),
        _ => None,
    }
}

/// The address that opens the page of that Store product in the Microsoft Store (`ms-windows-store://pdp/?ProductId=`, documented at
/// learn.microsoft.com/windows/apps/develop/launch/launch-store-app), for a file whose extension is one of a Store codec that
/// Windows lacks or could not start (the cases of `codec_hint`); None for any other file.
pub fn store_url(path: &std::path::Path) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    url_for(
        &extension,
        crate::wic::supports(&extension) && !crate::wic::failed(&extension),
    )
}

/// `store_url` for an extension (lower case) and whether Windows has a codec of it that works.
fn url_for(extension: &str, works: bool) -> Option<String> {
    if works {
        return None;
    }
    Some(format!(
        "ms-windows-store://pdp/?ProductId={}",
        store_product(extension)?
    ))
}

/// Opens `url` as the shell does for the user (a `ms-windows-store:` address starts the Store). Nothing is waited for or installed
/// here: the Store shows the page, and the user decides.
pub fn open_url(url: &str) {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, PCWSTR, w};
    let address = HSTRING::from(url);
    // SAFETY: both strings are zero-ended UTF-16 that live through the call; no window is the owner.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &address,
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecute answers a value above 32 when it worked
    if (result.0 as isize) <= 32 {
        crate::log::line(&format!(
            "the Store did not open: ShellExecute answered {}",
            result.0 as isize
        ));
    }
}

/// The box: a title line, then lines (Far's message box takes them as one string, split at the new lines).
pub fn text(codecs: Option<&[Codec]>, version: &str) -> String {
    let mut out = vec![format!("Peekel {version}: formats"), String::new()];
    out.push("Always, by itself (the same on every machine):".to_string());
    out.extend(wrap(
        "  ",
        "  ",
        &words("PNG JPEG GIF BMP WebP TIFF SVG  PDF  Word, PowerPoint, Excel (docx pptx xlsx)"),
    ));
    out.extend(wrap(
        "  RAW photographs, by the camera's own JPEG preview:",
        "    ",
        &words("NEF CR2 ARW DNG ORF PEF SR2 3FR ERF MEF KDC DCR RW2 NRW"),
    ));
    out.push(String::new());
    out.push("Found in this Windows (WIC), for what is not above:".to_string());
    match codecs {
        None => out.push("  being read: open this again in a moment".to_string()),
        Some(all) => {
            let mut shown = 0;
            for codec in all {
                // the extensions that `core` does not read by itself; a decoder with none left (PNG, JPEG, ...) is left out
                let left: Vec<&String> = codec.extensions.iter().filter(|e| !core_reads(e)).collect();
                if left.is_empty() {
                    continue;
                }
                shown += 1;
                let mut list: Vec<String> = left.iter().take(MOST_EXTENSIONS).map(|e| (*e).clone()).collect();
                if left.len() > MOST_EXTENSIONS {
                    list.push(format!("and {} more", left.len() - MOST_EXTENSIONS));
                }
                out.extend(wrap(&format!("  {}:", codec.name), "    ", &list));
                if left.iter().any(|e| crate::wic::failed(e)) {
                    out.push("    listed, but Windows could not start it on a file (0x88982F8B)".to_string());
                }
            }
            if shown == 0 {
                out.push("  nothing more (Windows lists no decoders of HEIC, AVIF, JPEG XL, ...)".to_string());
            }
        }
    }
    out.push(String::new());
    out.push("Any other file is shown as a card (its kind, size and dates).".to_string());
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codec(name: &str, extensions: &[&str]) -> Codec {
        Codec {
            name: name.to_string(),
            extensions: extensions.iter().map(|e| (*e).to_string()).collect(),
        }
    }

    #[test]
    fn a_file_of_a_store_codec_that_windows_lacks_names_the_extension_and_no_other_file_gets_a_hint() {
        // not in the list: the extension is named
        let jxl = hint_for("jxl", false, "no codec");
        assert!(
            jxl.as_deref().is_some_and(|h| h.contains("JPEG XL Image Extension")),
            "{jxl:?}"
        );
        let heic = hint_for("heic", false, "no codec");
        assert!(
            heic.as_deref().is_some_and(|h| h.contains("HEIF Image Extension")),
            "{heic:?}"
        );
        // in the list and the file is just damaged: no hint; in the list but not started: the hint
        assert_eq!(hint_for("heic", true, "the image is unrecognized (0x88982F60)"), None);
        let failed = hint_for("jxl", true, "The component initialization has failed. (0x88982F8B)");
        assert!(
            failed
                .as_deref()
                .is_some_and(|h| h.contains("did not start") && h.contains("may fix")),
            "{failed:?}"
        );
        // a cause is never stated as a fact
        assert!(hint_for("jxl", false, "no codec").is_some_and(|h| h.contains("may add") && !h.contains("needs")));
        // the hint says which key opens the Store, only where that key will work
        assert!(hint_for("jxl", false, "no codec").is_some_and(|h| h.contains("Ctrl+Alt+S")));
        // no other file gets one
        assert_eq!(hint_for("png", false, "no codec"), None);
        assert_eq!(hint_for("docx", false, "no codec"), None);
        assert_eq!(codec_hint(std::path::Path::new("noextension"), "no codec"), None);
    }

    #[test]
    fn the_store_address_is_the_page_of_the_product_and_only_for_a_store_codec_that_is_missing() {
        assert_eq!(
            url_for("jxl", false).as_deref(),
            Some("ms-windows-store://pdp/?ProductId=9MZPRTH5C0TB"),
            "the JPEG XL Image Extension"
        );
        assert_eq!(
            url_for("heic", false).as_deref(),
            Some("ms-windows-store://pdp/?ProductId=9PMMSR1CGPWG"),
            "the HEIF Image Extension"
        );
        assert_eq!(url_for("avif", false), url_for("heic", false));
        assert_eq!(url_for("jxl", true), None, "a codec that works needs no Store");
        assert_eq!(url_for("png", false), None, "not a Store codec");
        assert_eq!(url_for("docx", false), None);
        assert_eq!(
            store_url(std::path::Path::new("noextension")),
            None,
            "nothing to look up without an extension"
        );
    }

    #[test]
    fn a_decoder_that_did_not_start_is_told_from_a_damaged_file() {
        assert!(crate::wic::is_component_failure(
            "The component initialization has failed. (0x88982F8B)"
        ));
        assert!(!crate::wic::is_component_failure(
            "The image is unrecognized. (0x88982F60)"
        ));
    }

    #[test]
    fn the_box_says_what_is_built_in_and_what_windows_adds_and_leaves_out_what_core_reads() {
        let all = [
            codec("PNG Decoder", &[".png"]),
            codec("Microsoft HEIF Decoder", &[".heic", ".heif", ".avif"]),
            codec("Microsoft JPEG XL Decoder", &[".jxl"]),
        ];
        let shown = text(Some(&all), "9.9.9");
        assert!(shown.starts_with("Peekel 9.9.9: formats"));
        assert!(shown.contains("PNG JPEG GIF"));
        assert!(shown.contains("Microsoft HEIF Decoder: .heic .heif .avif"));
        assert!(shown.contains("Microsoft JPEG XL Decoder: .jxl"));
        assert!(
            !shown.contains("PNG Decoder"),
            "core reads PNG: Windows' decoder is not listed"
        );
    }

    #[test]
    fn no_line_is_longer_than_the_box_and_a_long_list_is_cut() {
        let many: Vec<String> = (0..40).map(|i| format!(".x{i:02}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let shown = text(
            Some(&[codec("A Decoder With A Rather Long Name Indeed", &refs)]),
            "0.2.1",
        );
        for line in shown.lines() {
            assert!(line.len() <= WIDTH + 12, "{line:?} is {} long", line.len());
        }
        assert!(shown.contains("and 28 more"), "{shown}");
    }

    #[test]
    fn before_the_list_is_read_and_with_none_the_box_says_so() {
        assert!(text(None, "0.2.1").contains("being read"));
        assert!(text(Some(&[]), "0.2.1").contains("no decoders of HEIC"));
    }

    #[test]
    fn the_raw_extensions_listed_are_those_core_reads() {
        for e in [
            "nef", "cr2", "arw", "dng", "orf", "pef", "sr2", "3fr", "erf", "mef", "kdc", "dcr", "rw2", "nrw",
        ] {
            assert!(core_reads(e), "{e}");
        }
        assert!(!core_reads("heic") && !core_reads("jxl"));
    }
}
