//! Windows Terminal's key records (mode 9001, "win32 input mode"): with the mode on, each key press and release arrives as
//! `ESC [ Vk ; Sc ; Uc ; Kd ; Cs ; Rc _`, which says the key that was pressed (a virtual key code and a scan code) and, apart from it,
//! the character that came of it with the layout that is on.
//!
//! This is what a window of Windows is told, and what makes Ctrl+X work with any layout: with the Russian layout the key that has `Q` on
//! it gives the virtual key `Q` and the character `й`. [`key_bytes`] turns a record into the bytes a terminal would send for the same
//! key in the usual way (a letter, a control character, `ESC [ A`), taking the letter from the virtual key and not from the layout.
//! What was measured in Windows Terminal, and the sequences it sent, are in `docs/viewer-keys.md`.

/// A record: `Vk` the virtual key, `Sc` the scan code, `Uc` the character, `Kd` whether the key went down (else up), `Cs` the state of
/// the control keys (the flags of the console API), `Rc` the repeat count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    pub vk: u16,
    pub scan: u16,
    pub unicode: u32,
    pub down: bool,
    pub state: u32,
    pub repeat: u16,
}

const RIGHT_ALT: u32 = 0x01;
const LEFT_ALT: u32 = 0x02;
const RIGHT_CTRL: u32 = 0x04;
const LEFT_CTRL: u32 = 0x08;
const SHIFT: u32 = 0x10;
const CAPS_LOCK: u32 = 0x80;

/// The record in `seq` (`ESC [ … _`, the whole sequence), if it is one.
pub fn parse(seq: &[u8]) -> Option<Record> {
    let body = seq.strip_prefix(b"\x1b[")?.strip_suffix(b"_")?;
    let text = std::str::from_utf8(body).ok()?;
    let mut fields = text
        .split(';')
        .map(|f| if f.is_empty() { Some(0) } else { f.parse::<u32>().ok() });
    // an empty field, and a field left out at the end, have their defaults: 0, and 1 for the repeat count
    let mut next = |default: u32| fields.next().unwrap_or(Some(default));
    let record = Record {
        vk: u16::try_from(next(0)?).ok()?,
        scan: u16::try_from(next(0)?).ok()?,
        unicode: next(0)?,
        down: next(0)? != 0,
        state: next(0)?,
        repeat: u16::try_from(next(1)?).ok()?.max(1),
    };
    Some(record)
}

/// The bytes of the key a record stands for, when it is a press (not a release) of a key that makes something: a letter (the key's own,
/// whatever the layout), a control character, a character typed, or the usual sequence of an arrow, Home, Page Down and the like.
/// None for a release and for the keys that only change the state of the others (Shift, Ctrl, Alt, Caps Lock, the Windows keys).
pub fn key_bytes(r: &Record) -> Option<Vec<u8>> {
    if !r.down {
        return None;
    }
    let shift = r.state & SHIFT != 0;
    let ctrl = r.state & (LEFT_CTRL | RIGHT_CTRL) != 0;
    let alt = r.state & (LEFT_ALT | RIGHT_ALT) != 0;
    // Ctrl and Alt together is AltGr: it types characters (an @ on a German layout), it is not a shortcut
    let alt_gr = ctrl && alt;
    match r.vk {
        // Shift, Ctrl, Alt, Caps Lock, the Windows keys, Num Lock, Scroll Lock
        16 | 17 | 18 | 20 | 91 | 92 | 144 | 145 => return None,
        // a letter key: the letter it has in the Latin layout, whatever the layout that is on
        65..=90 if !alt_gr => {
            let letter = u8::try_from(r.vk).ok()?.to_ascii_lowercase();
            if ctrl {
                // Ctrl+letter is the control character (Ctrl+C is 3), as in any terminal
                let control = letter & 0x1f;
                return Some(if alt { vec![0x1b, control] } else { vec![control] });
            }
            let capital = shift != (r.state & CAPS_LOCK != 0);
            let letter = if capital { letter.to_ascii_uppercase() } else { letter };
            return Some(if alt { vec![0x1b, letter] } else { vec![letter] });
        }
        _ => {}
    }
    // a digit of the top row and a key of punctuation: the character that key has in the Latin (US) layout, whatever the layout that is
    // on (a layout may have symbols there and the digits on Shift: the 0 key gave `%` and the key of `-` gave `!` in the author's)
    if !alt_gr
        && !ctrl
        && let Some(c) = latin_character(r.vk, shift)
    {
        return Some(if alt { vec![0x1b, c] } else { vec![c] });
    }
    // 1 + Shift + 2 Alt + 4 Ctrl: the number the usual sequences carry for the keys held
    let modifier = 1 + u32::from(shift) + 2 * u32::from(alt) + 4 * u32::from(ctrl);
    let with = |letter: char| {
        if modifier > 1 {
            format!("\x1b[1;{modifier}{letter}").into_bytes()
        } else {
            format!("\x1b[{letter}").into_bytes()
        }
    };
    let tilde = |number: u32| {
        if modifier > 1 {
            format!("\x1b[{number};{modifier}~").into_bytes()
        } else {
            format!("\x1b[{number}~").into_bytes()
        }
    };
    let function = |letter: char| {
        if modifier > 1 {
            format!("\x1b[1;{modifier}{letter}").into_bytes()
        } else {
            format!("\x1bO{letter}").into_bytes()
        }
    };
    let special = match r.vk {
        33 => Some(tilde(5)),
        34 => Some(tilde(6)),
        35 => Some(with('F')),
        36 => Some(with('H')),
        37 => Some(with('D')),
        38 => Some(with('A')),
        39 => Some(with('C')),
        40 => Some(with('B')),
        45 => Some(tilde(2)),
        46 => Some(tilde(3)),
        112 => Some(function('P')),
        113 => Some(function('Q')),
        114 => Some(function('R')),
        115 => Some(function('S')),
        _ => None,
    };
    if r.unicode == 0 {
        return special;
    }
    // a character typed (a digit, a point, a space, Enter, Escape, Backspace, and the letters of the layout for the keys that are no
    // letter keys): as itself; a unit of a pair of surrogates is half a character and is left
    let c = char::from_u32(r.unicode)?;
    let mut out = Vec::new();
    if alt && !alt_gr {
        out.push(0x1b);
    }
    let mut buffer = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
    Some(out)
}

/// The character the key with the virtual key `vk` has in the Latin (US) layout: a top-row digit (only without Shift: with Shift the
/// character of the layout is kept, as the question mark of Shift+7 in the Russian one is wanted as it is) and the keys of punctuation.
fn latin_character(vk: u16, shift: bool) -> Option<u8> {
    let (plain, shifted) = match vk {
        48..=57 if !shift => return u8::try_from(vk).ok(),
        186 => (b';', b':'),
        187 => (b'=', b'+'),
        188 => (b',', b'<'),
        189 => (b'-', b'_'),
        190 => (b'.', b'>'),
        191 => (b'/', b'?'),
        192 => (b'`', b'~'),
        219 => (b'[', b'{'),
        220 => (b'\\', b'|'),
        221 => (b']', b'}'),
        222 => (b'\'', b'"'),
        _ => return None,
    };
    Some(if shift { shifted } else { plain })
}

/// What a text field is given for a record: the character the layout made (so that the Russian letters are typed as Russian letters), or
/// the key's bytes for a key that is no character (Enter, Escape, Backspace, an arrow).
pub fn typed_bytes(r: &Record) -> Option<Vec<u8>> {
    if !r.down {
        return None;
    }
    let ctrl = r.state & (LEFT_CTRL | RIGHT_CTRL) != 0;
    let alt = r.state & (LEFT_ALT | RIGHT_ALT) != 0;
    if r.unicode >= 32 && !(ctrl && !alt) {
        let c = char::from_u32(r.unicode)?;
        let mut buffer = [0u8; 4];
        return Some(c.encode_utf8(&mut buffer).as_bytes().to_vec());
    }
    key_bytes(r)
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test with a record that does not parse should stop at once
mod tests {
    use super::*;

    fn record(text: &str) -> Record {
        parse(format!("\x1b[{text}_").as_bytes()).expect("a record")
    }

    // The records below were sent by Windows Terminal with the Russian layout on (see docs/viewer-keys.md).

    #[test]
    fn the_d_key_with_the_russian_layout_is_a_d_and_types_a_russian_letter() {
        let r = record("68;32;1074;1;32;1");
        assert_eq!((r.vk, r.scan, r.unicode, r.down), (68, 32, 1074, true));
        assert_eq!(key_bytes(&r), Some(b"d".to_vec()));
        assert_eq!(typed_bytes(&r), Some("в".as_bytes().to_vec()));
    }

    #[test]
    fn shift_ctrl_and_alt_with_the_q_key() {
        assert_eq!(
            key_bytes(&record("81;16;1049;1;48;1")),
            Some(b"Q".to_vec()),
            "Shift: й is Й, the key is Q"
        );
        assert_eq!(
            key_bytes(&record("81;16;17;1;40;1")),
            Some(vec![0x11]),
            "Ctrl+Q is the control character"
        );
        assert_eq!(
            key_bytes(&record("81;16;1081;1;34;1")),
            Some(b"\x1bq".to_vec()),
            "Alt+Q is Escape and q"
        );
    }

    #[test]
    fn a_release_and_the_keys_that_only_change_the_state_make_nothing() {
        assert_eq!(key_bytes(&record("68;32;1074;0;32;1")), None);
        for modifier in ["17;29;0;1;40;1", "18;56;0;1;34;1", "16;42;0;1;48;1", "20;58;0;1;32;1"] {
            assert_eq!(key_bytes(&record(modifier)), None, "{modifier}");
        }
    }

    #[test]
    fn arrows_carry_the_keys_held_as_the_usual_sequences_do() {
        assert_eq!(key_bytes(&record("38;72;0;1;288;1")), Some(b"\x1b[A".to_vec()));
        assert_eq!(
            key_bytes(&record("38;72;0;1;304;1")),
            Some(b"\x1b[1;2A".to_vec()),
            "Shift+Up"
        );
        assert_eq!(
            key_bytes(&record("34;81;0;1;288;1")),
            Some(b"\x1b[6~".to_vec()),
            "Page Down"
        );
        assert_eq!(key_bytes(&record("112;59;0;1;32;1")), Some(b"\x1bOP".to_vec()), "F1");
    }

    // The records below were all measured on the author's Windows Terminal (docs/viewer-keys.md).

    #[test]
    fn enter_escape_backspace_space_and_the_keys_with_no_character_are_as_measured() {
        assert_eq!(key_bytes(&record("13;28;13;1;32;1")), Some(b"\r".to_vec()), "Enter");
        assert_eq!(key_bytes(&record("27;1;27;1;32;1")), Some(vec![0x1b]), "Escape");
        assert_eq!(key_bytes(&record("8;14;8;1;32;1")), Some(vec![8]), "Backspace");
        assert_eq!(key_bytes(&record("32;57;32;1;32;1")), Some(b" ".to_vec()));
        assert_eq!(
            key_bytes(&record("34;81;0;1;288;1")),
            Some(b"\x1b[6~".to_vec()),
            "Page Down"
        );
        assert_eq!(
            key_bytes(&record("33;73;0;1;288;1")),
            Some(b"\x1b[5~".to_vec()),
            "Page Up"
        );
        assert_eq!(key_bytes(&record("36;71;0;1;288;1")), Some(b"\x1b[H".to_vec()), "Home");
        assert_eq!(key_bytes(&record("35;79;0;1;288;1")), Some(b"\x1b[F".to_vec()), "End");
        assert_eq!(key_bytes(&record("112;59;0;1;32;1")), Some(b"\x1bOP".to_vec()), "F1");
        assert_eq!(typed_bytes(&record("13;28;13;1;32;1")), Some(b"\r".to_vec()));
    }

    #[test]
    fn digits_and_punctuation_are_the_keys_whatever_the_layout_made_of_them() {
        // the author's Russian layout has symbols where the digits are: the 0 key gave %, the key of - gave !, the key of / gave ё
        assert_eq!(key_bytes(&record("48;11;37;1;32;1")), Some(b"0".to_vec()), "the 0 key");
        assert_eq!(
            key_bytes(&record("189;12;33;1;32;1")),
            Some(b"-".to_vec()),
            "the key of -"
        );
        assert_eq!(
            key_bytes(&record("191;53;1105;1;32;1")),
            Some(b"/".to_vec()),
            "the key of /"
        );
        assert_eq!(
            key_bytes(&record("187;13;92;1;48;1")),
            Some(b"+".to_vec()),
            "Shift + the key of ="
        );
        // text typed is what the layout made: the ё
        assert_eq!(
            typed_bytes(&record("191;53;1105;1;32;1")),
            Some("ё".as_bytes().to_vec())
        );
        // a digit with Shift keeps the character of the layout (the 7 key gave 7 on Shift there; a layout with ? on it gives ?)
        assert_eq!(key_bytes(&record("55;8;55;1;48;1")), Some(b"7".to_vec()));
        assert_eq!(key_bytes(&record("55;8;63;1;48;1")), Some(b"?".to_vec()));
    }

    #[test]
    fn caps_lock_turns_the_letters_and_alt_gr_types() {
        assert_eq!(
            key_bytes(&record("81;16;1049;1;160;1")),
            Some(b"Q".to_vec()),
            "Caps Lock"
        );
        assert_eq!(
            key_bytes(&record("81;16;1049;1;176;1")),
            Some(b"q".to_vec()),
            "Caps Lock and Shift"
        );
        // AltGr+Q on a German layout types @ (Ctrl and Alt are held together)
        assert_eq!(key_bytes(&record("81;16;64;1;43;1")), Some(b"@".to_vec()));
    }

    #[test]
    fn fields_left_out_at_the_end_or_empty_take_their_defaults() {
        // the examples of the specification (Windows Terminal, doc/specs/#4999): `65;30;97;1`, `65;30;97` and `17;29;;`
        let r = record("65;30;97;1");
        assert_eq!(
            (r.vk, r.scan, r.unicode, r.down, r.state, r.repeat),
            (65, 30, 97, true, 0, 1)
        );
        let r = record("65;30;97");
        assert_eq!((r.down, r.state, r.repeat), (false, 0, 1));
        let r = record("17;29;;");
        assert_eq!((r.vk, r.unicode, r.down, r.repeat), (17, 0, false, 1));
        assert!(parse(b"\x1b[_").is_some_and(|r| r.vk == 0 && r.repeat == 1));
    }

    #[test]
    fn what_is_no_record_is_none() {
        for bad in [&b"\x1b[A"[..], b"\x1b[x;1;1;1;1;1_", b"abc", b"\x1b[70000;1;1;1;1;1_"] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }
}
