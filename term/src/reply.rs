//! Reading what the terminal sends back: its answers to our questions and the keys pressed.
//!
//! The questions are `CSI c` (device attributes; a `4` in the answer means Sixel) and `CSI 16 t` (the cell's size in
//! pixels, answered `CSI 6 ; height ; width t`) and `OSC 11 ; ?` (the background colour, answered
//! `OSC 11 ; rgb:RRRR/GGGG/BBBB` ended by BEL or `ESC \`). Windows Terminal answers `CSI ? 61 ; 4 ; 6 ; 7 ; ... c` and
//! `CSI 6 ; 20 ; 10 t` (measured in BlueEdit: 10 × 20 pixels a cell, whatever the font). Everything else that arrives
//! is a key; this viewer only needs to know that one was pressed.

/// One thing the terminal sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The answer to `CSI c`: the numbers after `CSI ?`.
    DeviceAttributes(Vec<u32>),
    /// The answer to `CSI 16 t`: a cell's size in pixels.
    CellSize { width: u32, height: u32 },
    /// The answer to `CSI 5 n` (`CSI 0 n`: "in working order"). A terminal answers it only after it has dealt with
    /// everything sent before the question, which is what makes it a way to time its work.
    StatusReport,
    /// The answer to `OSC 11 ; ?`: the terminal's background colour (red, green, blue).
    Background([u8; 3]),
    /// A mouse report (`CSI < button ; column ; row M` for a press or a move, `m` for a release; columns and rows count
    /// from 1). `button`: 0 left, 1 middle, 2 right, plus 32 for a move with a button held, 64 and 65 for the wheel (up,
    /// down), plus 4, 8 and 16 for Shift, Alt and Ctrl.
    Mouse {
        button: u32,
        column: u32,
        row: u32,
        press: bool,
    },
    /// The answer to `CSI ? mode $ p` (is this mode known?): `CSI ? mode ; state $ y`. State 1 is set, 2 reset, 0 not known, 3 and 4
    /// fixed on and off.
    ModeReport { mode: u32, state: u32 },
    /// A key: the bytes it sent (a character, or an escape sequence such as an arrow's).
    Key(Vec<u8>),
}

/// The longest escape sequence kept; a longer one is junk and is handed over as a key.
const LONGEST: usize = 64;

const ESC: u8 = 0x1B;

/// Turns the bytes read from the terminal into [`Event`]s, one byte at a time. An answer may arrive in pieces, so a
/// half-read escape sequence waits in the parser; [`Parser::flush`] hands it over as a key when nothing follows (an
/// Escape key pressed alone is a lone ESC byte).
#[derive(Debug, Default)]
pub struct Parser {
    pending: Vec<u8>,
}

impl Parser {
    /// Whether an escape sequence is half read.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The next byte; an event when it completes one.
    pub fn push(&mut self, byte: u8) -> Option<Event> {
        // an ESC begins a new sequence (outside a string, where `ESC \` ends it): what was half read is a key of its own. Escape
        // pressed while the program was busy, then the terminal's answer, would else be read as `ESC ESC` and keys `[`, `0`, `n`
        if byte == ESC && !self.pending.is_empty() && !self.pending.starts_with(&[ESC, b']']) {
            let before = std::mem::replace(&mut self.pending, vec![ESC]);
            return Some(Event::Key(before));
        }
        match self.pending.as_slice() {
            [] if byte != ESC => Some(Event::Key(vec![byte])),
            [] => {
                self.pending.push(byte);
                None
            }
            [ESC] => match byte {
                b'[' | b'O' | b']' => {
                    self.pending.push(byte);
                    None
                }
                _ => Some(self.take_as_key(byte)), // Alt + a key
            },
            [ESC, b'O'] => Some(self.take_as_key(byte)), // SS3: F1..F4 and the like, one final byte
            [ESC, b']', ..] => self.push_osc(byte),
            _ => self.push_csi(byte),
        }
    }

    /// Inside `ESC ]`: text until BEL or `ESC \`.
    fn push_osc(&mut self, byte: u8) -> Option<Event> {
        let after_escape = self.pending.last() == Some(&ESC);
        match byte {
            0x07 if !after_escape => Some(self.finish_osc()),
            b'\\' if after_escape => {
                self.pending.pop();
                Some(self.finish_osc())
            }
            ESC if !after_escape && self.pending.len() < LONGEST => {
                self.pending.push(byte);
                None
            }
            0x20..=0x7E if !after_escape && self.pending.len() < LONGEST => {
                self.pending.push(byte);
                None
            }
            _ => Some(self.take_as_key(byte)), // not a string after all (or too long)
        }
    }

    fn finish_osc(&mut self) -> Event {
        let sequence = std::mem::take(&mut self.pending);
        match sequence.get(2..).and_then(background_of) {
            Some(rgb) => Event::Background(rgb),
            None => Event::Key(sequence),
        }
    }

    /// A half-read sequence handed over as a key: call it when a short while has passed without another byte.
    pub fn flush(&mut self) -> Option<Event> {
        (!self.pending.is_empty()).then(|| Event::Key(std::mem::take(&mut self.pending)))
    }

    fn take_as_key(&mut self, last: u8) -> Event {
        let mut bytes = std::mem::take(&mut self.pending);
        bytes.push(last);
        Event::Key(bytes)
    }

    /// Inside `ESC [`: parameters and intermediate bytes (0x20..=0x3F) until a final byte (0x40..=0x7E).
    fn push_csi(&mut self, byte: u8) -> Option<Event> {
        match byte {
            0x20..=0x3F if self.pending.len() < LONGEST => {
                self.pending.push(byte);
                None
            }
            0x40..=0x7E => {
                let mut sequence = std::mem::take(&mut self.pending);
                sequence.push(byte);
                Some(classify(sequence))
            }
            _ => Some(self.take_as_key(byte)), // not a sequence after all (or too long)
        }
    }
}

/// `ESC [ params final` as the answer it is, or as a key.
fn classify(sequence: Vec<u8>) -> Event {
    let (Some(&final_byte), Some(params)) = (sequence.last(), sequence.get(2..sequence.len() - 1)) else {
        return Event::Key(sequence);
    };
    match (final_byte, params) {
        (b'c', [b'?', numbers @ ..]) => Event::DeviceAttributes(numbers_of(numbers)),
        (b'n', [b'0']) => Event::StatusReport,
        (b'M' | b'm', [b'<', numbers @ ..]) => match numbers_of(numbers).as_slice() {
            [button, column, row] => Event::Mouse {
                button: *button,
                column: *column,
                row: *row,
                press: final_byte == b'M',
            },
            _ => Event::Key(sequence),
        },
        (b'y', [b'?', numbers @ .., b'$']) => match numbers_of(numbers).as_slice() {
            [mode, state] => Event::ModeReport {
                mode: *mode,
                state: *state,
            },
            _ => Event::Key(sequence),
        },
        (b't', numbers) => match numbers_of(numbers).as_slice() {
            [6, height, width] if *height > 0 && *width > 0 => Event::CellSize {
                width: *width,
                height: *height,
            },
            _ => Event::Key(sequence),
        },
        _ => Event::Key(sequence),
    }
}

/// `11;rgb:RRRR/GGGG/BBBB` (each part one to four hex digits) as three bytes; None for anything else.
fn background_of(text: &[u8]) -> Option<[u8; 3]> {
    let text = std::str::from_utf8(text).ok()?;
    let colour = text.strip_prefix("11;rgb:")?;
    let mut parts = colour.split('/');
    let mut rgb = [0u8; 3];
    for slot in &mut rgb {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 4 {
            return None;
        }
        let value = u32::from_str_radix(part, 16).ok()?;
        let most = (1u32 << (4 * part.len())) - 1;
        *slot = u8::try_from((value * 255 + most / 2) / most).ok()?;
    }
    parts.next().is_none().then_some(rgb)
}

/// `61;4;6` as 61, 4, 6; a part that is not a number is 0.
fn numbers_of(params: &[u8]) -> Vec<u32> {
    params
        .split(|&b| b == b';')
        .map(|part| std::str::from_utf8(part).ok().and_then(|s| s.parse().ok()).unwrap_or(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(parser: &mut Parser, bytes: &[u8]) -> Vec<Event> {
        bytes.iter().filter_map(|&b| parser.push(b)).collect()
    }

    fn parse(bytes: &[u8]) -> Vec<Event> {
        feed(&mut Parser::default(), bytes)
    }

    #[test]
    fn windows_terminals_device_attributes_say_sixel() {
        let got = parse(b"\x1b[?61;4;6;7;14;21;22;23;24;28;32;42;52c");
        assert_eq!(
            got,
            [Event::DeviceAttributes(vec![
                61, 4, 6, 7, 14, 21, 22, 23, 24, 28, 32, 42, 52
            ])]
        );
    }

    #[test]
    fn the_cell_size_answer_is_height_then_width() {
        assert_eq!(parse(b"\x1b[6;20;10t"), [Event::CellSize { width: 10, height: 20 }]);
    }

    #[test]
    fn the_answer_about_a_mode_is_its_own_event() {
        // what Windows Terminal answers to `CSI ? 9001 $ p`
        assert_eq!(parse(b"\x1b[?9001;1$y"), [Event::ModeReport { mode: 9001, state: 1 }]);
        assert_eq!(parse(b"\x1b[?9001;0$y"), [Event::ModeReport { mode: 9001, state: 0 }]);
        // an answer with no state, or another kind of sequence ending in y, is a key
        assert_eq!(parse(b"\x1b[?9001$y"), [Event::Key(b"\x1b[?9001$y".to_vec())]);
    }

    #[test]
    fn the_status_report_is_its_own_event_and_other_n_sequences_are_keys() {
        assert_eq!(parse(b"\x1b[0n"), [Event::StatusReport]);
        assert_eq!(
            parse(b"\x1b[3n"),
            [Event::Key(b"\x1b[3n".to_vec())],
            "3 is a malfunction report, not ours"
        );
    }

    #[test]
    fn a_cell_size_of_zero_is_no_answer() {
        assert_eq!(parse(b"\x1b[6;0;0t"), [Event::Key(b"\x1b[6;0;0t".to_vec())]);
    }

    #[test]
    fn both_answers_in_a_row_and_a_key_after_them() {
        let got = parse(b"\x1b[?62;4c\x1b[6;18;9tq");
        assert_eq!(
            got,
            [
                Event::DeviceAttributes(vec![62, 4]),
                Event::CellSize { width: 9, height: 18 },
                Event::Key(b"q".to_vec())
            ]
        );
    }

    #[test]
    fn an_answer_may_arrive_in_pieces() {
        let mut parser = Parser::default();
        assert_eq!(feed(&mut parser, b"\x1b[6;2"), []);
        assert!(parser.has_pending());
        assert_eq!(feed(&mut parser, b"0;10t"), [Event::CellSize { width: 10, height: 20 }]);
        assert!(!parser.has_pending());
    }

    #[test]
    fn the_background_answer_ends_with_bel_or_st_and_has_four_digits_a_part() {
        assert_eq!(
            parse(b"\x1b]11;rgb:ffff/8080/0000\x07"),
            [Event::Background([255, 128, 0])]
        );
        assert_eq!(
            parse(b"\x1b]11;rgb:0c0c/0c0c/0c0c\x1b\\"),
            [Event::Background([12, 12, 12])]
        );
    }

    #[test]
    fn the_background_answer_may_have_fewer_digits_and_arrive_in_pieces() {
        let mut parser = Parser::default();
        assert_eq!(feed(&mut parser, b"\x1b]11;rgb:f/0/"), []);
        assert!(parser.has_pending());
        assert_eq!(feed(&mut parser, b"8\x07"), [Event::Background([255, 0, 136])]);
    }

    #[test]
    fn a_string_that_is_no_background_is_a_key_and_keys_after_it_still_come() {
        assert_eq!(parse(b"\x1b]0;title\x07"), [Event::Key(b"\x1b]0;title".to_vec())]);
        assert_eq!(
            parse(b"\x1b]11;rgb:zz/00/00\x07"),
            [Event::Key(b"\x1b]11;rgb:zz/00/00".to_vec())]
        );
        assert_eq!(
            parse(b"\x1b]11;rgb:00/00/00\x07q"),
            [Event::Background([0, 0, 0]), Event::Key(b"q".to_vec())]
        );
    }

    #[test]
    fn a_background_answer_between_the_other_answers() {
        assert_eq!(
            parse(b"\x1b[?62;4c\x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\\x1b[6;18;9t"),
            [
                Event::DeviceAttributes(vec![62, 4]),
                Event::Background([30, 30, 30]),
                Event::CellSize { width: 9, height: 18 }
            ]
        );
    }

    #[test]
    fn mouse_reports_in_the_sgr_form() {
        assert_eq!(
            parse(b"\x1b[<0;12;5M"),
            [Event::Mouse {
                button: 0,
                column: 12,
                row: 5,
                press: true
            }]
        );
        assert_eq!(
            parse(b"\x1b[<0;12;5m"),
            [Event::Mouse {
                button: 0,
                column: 12,
                row: 5,
                press: false
            }]
        );
        assert_eq!(
            parse(b"\x1b[<64;3;4M\x1b[<65;3;4Mq"),
            [
                Event::Mouse {
                    button: 64,
                    column: 3,
                    row: 4,
                    press: true
                },
                Event::Mouse {
                    button: 65,
                    column: 3,
                    row: 4,
                    press: true
                },
                Event::Key(b"q".to_vec())
            ]
        );
    }

    #[test]
    fn a_mouse_report_with_the_wrong_number_of_parts_is_a_key() {
        assert_eq!(parse(b"\x1b[<0;12M"), [Event::Key(b"\x1b[<0;12M".to_vec())]);
    }

    #[test]
    fn a_plain_key_is_a_key_at_once() {
        assert_eq!(parse(b"x"), [Event::Key(b"x".to_vec())]);
    }

    #[test]
    fn an_arrow_key_is_a_key_not_an_answer() {
        assert_eq!(parse(b"\x1b[A"), [Event::Key(b"\x1b[A".to_vec())]);
        assert_eq!(parse(b"\x1b[1;5C"), [Event::Key(b"\x1b[1;5C".to_vec())]);
        assert_eq!(parse(b"\x1bOP"), [Event::Key(b"\x1bOP".to_vec())]);
    }

    #[test]
    fn alt_and_a_key_is_one_key() {
        assert_eq!(parse(b"\x1bx"), [Event::Key(b"\x1bx".to_vec())]);
    }

    #[test]
    fn an_escape_pressed_alone_waits_and_is_handed_over_by_flush() {
        let mut parser = Parser::default();
        assert_eq!(parser.push(ESC), None);
        assert!(parser.has_pending());
        assert_eq!(parser.flush(), Some(Event::Key(vec![ESC])));
        assert!(!parser.has_pending());
        assert_eq!(parser.flush(), None, "nothing left to hand over");
    }

    #[test]
    fn an_escape_then_the_terminals_answer_at_once_are_the_key_and_the_answer() {
        // Escape pressed while the viewer was drawing, and the answer to "are you done?" right behind it
        assert_eq!(
            parse(b"\x1b\x1b[0nq"),
            [Event::Key(vec![ESC]), Event::StatusReport, Event::Key(b"q".to_vec())]
        );
        // an arrow cut short by another sequence: the piece is a key, the next sequence is whole
        assert_eq!(
            parse(b"\x1b[1\x1b[A"),
            [Event::Key(b"\x1b[1".to_vec()), Event::Key(b"\x1b[A".to_vec())]
        );
        // inside a string, ESC \ still ends it
        assert_eq!(parse(b"\x1b]11;rgb:00/00/00\x1b\\"), [Event::Background([0, 0, 0])]);
    }

    #[test]
    fn a_sequence_that_never_ends_is_given_up_as_a_key() {
        let mut parser = Parser::default();
        let mut bytes = b"\x1b[".to_vec();
        bytes.extend(std::iter::repeat_n(b'1', 200));
        let got = feed(&mut parser, &bytes);
        // the first LONGEST bytes are kept, the next one gives the junk up as one key, the rest are keys of their own
        assert!(
            matches!(&got[0], Event::Key(k) if k.len() == LONGEST + 1),
            "{:?}",
            got[0]
        );
        assert!(got[1..].iter().all(|e| matches!(e, Event::Key(k) if k.len() == 1)));
        assert_eq!(got.len(), 1 + (202 - (LONGEST + 1)));
        assert!(!parser.has_pending());
    }

    #[test]
    fn a_control_byte_inside_a_sequence_breaks_it_into_a_key() {
        // ESC [ 1 then a newline: not a sequence; the bytes so far and the newline come out as one key
        assert_eq!(parse(b"\x1b[1\n"), [Event::Key(b"\x1b[1\n".to_vec())]);
    }

    #[test]
    fn numbers_that_are_not_numbers_are_zero() {
        assert_eq!(numbers_of(b"61;;x;4"), [61, 0, 0, 4]);
    }
}
