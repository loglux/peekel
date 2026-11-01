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
/// the names as the Microsoft Store has them (docs/internal/wic.md). Only these are named; no other file gets a hint.
fn store_extensions(extension: &str) -> Option<&'static str> {
    match extension {
        "jxl" => Some("the JPEG XL Image Extension"),
        "heic" | "heif" | "hif" | "avci" | "heics" | "heifs" => {
            Some("the HEIF Image Extension and HEVC Video Extensions")
        }
        "avif" | "avifs" => Some("the HEIF Image Extension and the AV1 Video Extension"),
        _ => None,
    }
}

/// What the card of a file that no codec opened may say: which Store extension gives Windows the codec, when the extension of
/// the file is one of a Store codec and Windows has none of it (not in the list, or listed and not started). `why` is the
/// reason the file was not read as a picture. None for any other file.
pub fn codec_hint(path: &std::path::Path, why: &str) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    hint_for(&extension, crate::wic::supports(&extension), why)
}

/// `codec_hint` for an extension (lower case) and what is known of this Windows: whether a decoder of it is in the list.
fn hint_for(extension: &str, listed: bool, why: &str) -> Option<String> {
    let names = store_extensions(extension)?;
    if listed && !crate::wic::is_component_failure(why) {
        return None; // the codec is there: the file is damaged, or the card was asked for
    }
    Some(format!("needs {names} (Microsoft Store)"))
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
                    out.push(
                        "    listed, but it did not start on a file: the extension is not really installed".to_string(),
                    );
                }
            }
            if shown == 0 {
                out.push("  nothing more (no decoders of HEIC, AVIF, JPEG XL, ... are installed)".to_string());
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
        assert!(hint_for("jxl", true, "The component initialization has failed. (0x88982F8B)").is_some());
        // no other file gets one
        assert_eq!(hint_for("png", false, "no codec"), None);
        assert_eq!(hint_for("docx", false, "no codec"), None);
        assert_eq!(codec_hint(std::path::Path::new("noextension"), "no codec"), None);
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
