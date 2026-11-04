//! Putting text on the terminal's clipboard: `OSC 52 ; c ; base64 ST`. The terminal may refuse (some ask the user, some
//! have it off by default) and says nothing either way, so the caller cannot know whether it worked.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The standard base64 of `bytes`, padded with `=`.
fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, &b)| acc | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// What to write to the terminal to put `text` on its clipboard.
pub fn copy_sequence(text: &str) -> String {
    format!("\x1b]52;c;{}\x1b\\", base64(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_of_the_rfc_4648_examples() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(plain.as_bytes()), encoded, "{plain:?}");
        }
    }

    #[test]
    fn bytes_above_ascii_are_encoded_as_they_are() {
        assert_eq!(base64("й".as_bytes()), "0Lk=");
    }

    #[test]
    fn the_sequence_is_osc_52_for_the_clipboard_ended_by_st() {
        assert_eq!(copy_sequence("foo"), "\x1b]52;c;Zm9v\x1b\\");
    }
}
