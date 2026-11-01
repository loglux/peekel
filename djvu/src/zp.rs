//! The Z'-Coder, the binary arithmetic decoder that DjVu uses for everything it compresses (JB2 pictures, BZZ data): Appendix 3 of the
//! official DjVu v3 specification, Figure 1 (a decision in a context) and Figure 2 (a decision without one, "pass-through").
//!
//! Registers: `A` and `C` of at least 16 bits. `A` starts at 0 and `C` is the first two octets of the data (the first in bits 15
//! to 8). A decision is made with the context's probability state `K(i)` (one byte, 0 at the start, changed only by the decoder),
//! the table of [`crate::zp_table`], and the arithmetic of the figures. Bits are taken from each octet most significant bit
//! first, and when the data runs out every further bit is a 1 (spec 12.1); what is left over at the end is ignored.
//!
//! The registers are kept to 16 bits, as the spec allows ("storing A and C in registers of exactly 16 bits", 12.1): a value that
//! would go past them is cut, which for a valid stream never happens and for any other input is what keeps the decoder from
//! panicking or growing without end. `A` is below 0x8000 when a decision begins (it starts at 0) and below 0x10000 always, so the
//! renormalising loop ends within sixteen turns.

use crate::zp_table::STATES;

/// One row of the table of states (Table 9): see [`crate::zp_table`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct State {
    pub delta: u16,
    pub theta: u16,
    pub mps_next: u8,
    pub lps_next: u8,
}

/// The decoder, over the bytes of one chunk (or one part of one).
#[derive(Debug, Clone)]
pub struct ZpDecoder<'a> {
    data: &'a [u8],
    /// How many octets of `data` have been taken into `bits` (past the end they are 1 bits, and the count goes on).
    taken: usize,
    /// The bits that have been taken in and not used yet: the lowest `held` bits, the next one to use highest.
    bits: u32,
    held: u32,
    a: u32,
    c: u32,
    /// `C`, or 0x7FFF if that is less. A decision whose `A + delta` is not above it is the more probable symbol and needs nothing
    /// renormalised: that is the most common case, and it is told from the rest by one comparison.
    fence: u32,
}

impl<'a> ZpDecoder<'a> {
    /// Starts decoding `data` (spec 12.2): `A` is 0, `C` the first two octets; octets that are not there are 1 bits.
    pub fn new(data: &'a [u8]) -> Self {
        let first = u32::from(data.first().copied().unwrap_or(0xFF));
        let second = u32::from(data.get(1).copied().unwrap_or(0xFF));
        let c = first << 8 | second;
        let mut decoder = Self {
            data,
            taken: 2,
            bits: 0,
            held: 0,
            a: 0,
            c,
            fence: c.min(0x7FFF),
        };
        decoder.refill();
        decoder
    }

    /// Takes octets into `bits` until it holds more than 24 bits (an octet past the end of the data is 0xFF).
    fn refill(&mut self) {
        while self.held <= 24 {
            let octet = self.data.get(self.taken).copied().unwrap_or(0xFF);
            self.bits = self.bits << 8 | u32::from(octet);
            self.held += 8;
            self.taken = self.taken.saturating_add(1);
        }
    }

    /// Lines 15 to 18 of Figure 1 (and 11 to 14 of Figure 2): while `A` is 0x8000 or more, double `A` and `C` and bring in a bit; done
    /// at once, the number of doublings being the number of leading 1 bits of `A` (which is below 0x10000), and the bits that come
    /// in being taken from `bits` together. Leaves `fence` right for the new `C`.
    fn renormalize(&mut self) {
        let shift = (self.a as u16).leading_ones();
        if shift > 0 {
            self.held -= shift;
            self.a = (self.a << shift) & 0xFFFF;
            let incoming = (self.bits >> self.held) & ((1 << shift) - 1);
            self.c = (self.c << shift | incoming) & 0xFFFF;
            if self.held < 16 {
                self.refill();
            }
        }
        self.fence = self.c.min(0x7FFF);
    }

    /// One binary decision in the context `ctx` (Figure 1). `ctx` is the probability state `K(i)` of that context: 0 at the start
    /// of a stream, and then only ever changed by this function. A value that is not a state is read as state 0.
    #[inline]
    pub fn decode_bit(&mut self, ctx: &mut u8) -> bool {
        let index = if usize::from(*ctx) < STATES.len() { *ctx } else { 0 };
        let k = STATES[usize::from(index)];
        let mps = index & 1; // the MPS is 1 for an odd state, 0 for an even one (spec 12.1)
        let z = self.a + u32::from(k.delta);
        if z <= self.fence {
            // the more probable symbol with nothing to renormalise (z is below 0x8000, so the state stays, and below the limit
            // that lines 2 and 3 of Figure 1 set, which only a larger z can pass)
            self.a = z;
            return mps == 1;
        }
        self.decode_bit_slow(ctx, k, mps, z)
    }

    /// The rest of the decision of Figure 1, for a `z` (`A` plus the delta of the state) above the fence.
    #[cold]
    #[inline(never)]
    fn decode_bit_slow(&mut self, ctx: &mut u8, k: State, mps: u8, mut z: u32) -> bool {
        let d = 0x6000 + ((z + self.a) >> 2);
        if z > d {
            z = d;
        }
        let bit;
        if self.c >= z {
            // the more probable symbol: lines 4 to 8
            bit = mps;
            // the state moves only when the interval has to be renormalised (z is 0x8000 or more): Figure 1 checks A against theta
            // every time, and a reader that does so does not decode real streams (found with the streams of another encoder)
            if z >= 0x8000 && self.a >= u32::from(k.theta) {
                *ctx = k.mps_next;
            }
            self.a = z;
        } else {
            // the less probable one: lines 9 to 14
            bit = 1 - mps;
            self.a = self.a + 0x1_0000 - z;
            self.c = (self.c + 0x1_0000 - z) & 0xFFFF;
            *ctx = k.lps_next;
        }
        self.renormalize();
        bit == 1
    }

    /// One row of a shape by direct coding (spec 11.5.9) for the JB2 decoder: the same decisions as [`Self::decode_bit`] in the
    /// contexts `contexts` (one for each value of the 10 neighbours, the first neighbour the highest bit), with the registers kept in
    /// variables of the function for the length of the row, which is what makes it quick: the rare decisions that are not the
    /// common one go through [`Self::decode_bit_slow`], and the registers are put back and read again round it. `two_up` and
    /// `one_up` are the two rows above, `row` this one, all in a frame with two empty columns before the first pixel and at least
    /// three after the last (the pixel `x` is at `x + 2`); `width` pixels are decoded.
    pub(crate) fn decode_direct_row(
        &mut self,
        contexts: &mut [u8],
        row: &mut [u8],
        two_up: &[u8],
        one_up: &[u8],
        width: usize,
    ) {
        let Ok(contexts) = <&mut [u8; 1024]>::try_from(contexts) else {
            return;
        };
        let (mut a, mut fence) = (self.a, self.fence);
        let mut far = usize::from(two_up[1]) << 2 | usize::from(two_up[2]) << 1 | usize::from(two_up[3]);
        let mut near = usize::from(one_up[0]) << 4
            | usize::from(one_up[1]) << 3
            | usize::from(one_up[2]) << 2
            | usize::from(one_up[3]) << 1
            | usize::from(one_up[4]);
        let mut left = 0_usize;
        for x in 0..width {
            let slot = &mut contexts[(far << 7 | near << 2 | left) & 1023];
            let index = if usize::from(*slot) < STATES.len() { *slot } else { 0 };
            let k = STATES[usize::from(index)];
            let z = a + u32::from(k.delta);
            let bit = if z <= fence {
                // the more probable symbol, nothing to renormalise
                a = z;
                index & 1 == 1
            } else {
                self.a = a;
                let bit = self.decode_bit_slow(slot, k, index & 1, z);
                a = self.a;
                fence = self.fence;
                bit
            };
            row[x + 2] = u8::from(bit);
            far = (far << 1 & 0b111) | usize::from(two_up[x + 4]);
            near = (near << 1 & 0b11111) | usize::from(one_up[x + 5]);
            left = (left << 1 & 0b11) | usize::from(bit);
        }
        self.a = a;
    }

    /// One decision without a context, in the form of Figure 2 of the spec: the threshold is 0x8000 + 3A/8. This is the form that the
    /// IW44 data uses (the spec does not say so; the form BZZ uses is [`Self::decode_pass_half`]).
    pub fn decode_pass(&mut self) -> bool {
        let z = 0x8000 + ((self.a + self.a + self.a) >> 3);
        self.pass_with(z)
    }

    /// One decision without a context, with the threshold 0x8000 + A/2: the form that BZZ uses for the sizes and the speed in a
    /// block (spec 13.2.1.1 sends `decode_passthru` to Figure 2, whose threshold is the other one; with that one the sizes of real
    /// BZZ streams do not come out, with this one they do, found by decoding what another encoder made).
    pub fn decode_pass_half(&mut self) -> bool {
        let z = 0x8000 + (self.a >> 1);
        self.pass_with(z)
    }

    /// The decision of Figure 2 for a threshold `z`.
    fn pass_with(&mut self, z: u32) -> bool {
        let bit;
        if self.c >= z {
            bit = false;
            self.a = z;
        } else {
            bit = true;
            self.a = self.a + 0x1_0000 - z;
            self.c = (self.c + 0x1_0000 - z) & 0xFFFF;
        }
        self.renormalize();
        bit
    }

    /// Whether the decoder has read past the end of its data by more than the two octets it starts with and a little slack; a
    /// stream that goes on after this is made of the 1 bits that stand for missing data, and a reader that loops on records can
    /// stop on it (as the JB2 decoder does).
    pub fn is_exhausted(&self) -> bool {
        // the bits used so far: those of the octets taken less those not used yet
        (self.taken * 8).saturating_sub(self.held as usize) > self.data.len().saturating_mul(8).saturating_add(8 * 4)
    }

    #[cfg(test)]
    pub(crate) fn registers(&self) -> (u32, u32) {
        (self.a, self.c)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// A cheap deterministic stream of bytes that is not constant.
    fn noise(len: usize, mut seed: u64) -> Vec<u8> {
        (0..len)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn the_table_has_the_rows_of_the_spec_and_every_next_state_is_a_state() {
        assert_eq!(STATES.len(), 251);
        assert_eq!(
            STATES[0],
            State {
                delta: 0x8000,
                theta: 0,
                mps_next: 84,
                lps_next: 145
            }
        );
        assert_eq!(
            STATES[3],
            State {
                delta: 0x6BBD,
                theta: 0x10A5,
                mps_next: 5,
                lps_next: 1
            }
        );
        assert_eq!(STATES[49].theta, 0x7D0F, "the row the printed spec spells 0x7DOF");
        assert_eq!(STATES[95].delta, 0x10CA, "0xl0CA in the printed spec");
        assert_eq!(
            STATES[250],
            State {
                delta: 0x481A,
                theta: 0,
                mps_next: 230,
                lps_next: 246
            }
        );
        for (k, s) in STATES.iter().enumerate() {
            assert!(
                usize::from(s.mps_next) < STATES.len() && usize::from(s.lps_next) < STATES.len(),
                "row {k}"
            );
            assert!(
                s.delta > 0,
                "row {k}: a state that cannot shrink the interval would not end the renormalising"
            );
        }
    }

    #[test]
    fn the_registers_keep_to_their_bounds_on_any_data() {
        for seed in [1_u64, 7, 0xDEAD_BEEF, 0xFFFF_FFFF_FFFF] {
            for data in [
                noise(4096, seed),
                vec![],
                vec![0; 100],
                vec![0xFF; 100],
                vec![0x00, 0xFF, 0x00, 0xFF],
            ] {
                let mut decoder = ZpDecoder::new(&data);
                let mut contexts = [0_u8; 64];
                for i in 0..60_000 {
                    if i % 7 == 0 {
                        decoder.decode_pass();
                    } else {
                        decoder.decode_bit(&mut contexts[i % 64]);
                    }
                    let (a, c) = decoder.registers();
                    assert!(a < 0x8000, "A is below 0x8000 after a decision: {a:#x}");
                    assert!(c <= 0xFFFF, "C stays in 16 bits: {c:#x}");
                }
            }
        }
    }

    #[test]
    fn a_context_that_is_not_a_state_is_read_as_the_first_state_and_does_not_panic() {
        let data = noise(64, 3);
        let mut decoder = ZpDecoder::new(&data);
        let mut ctx = 255_u8;
        for _ in 0..200 {
            decoder.decode_bit(&mut ctx);
        }
        assert!(usize::from(ctx) < STATES.len(), "after one decision it is a state");
    }

    #[test]
    fn data_that_runs_out_is_ones_and_the_decoder_says_so() {
        let mut decoder = ZpDecoder::new(&[0x12, 0x34]);
        let mut ctx = 0_u8;
        assert!(!decoder.is_exhausted());
        // decisions in a state that has learned the data need few bits, so how many it takes is not fixed: run until it says so
        let mut turns = 0;
        while !decoder.is_exhausted() && turns < 1_000_000 {
            decoder.decode_pass(); // a pass-through decision takes a bit at about every other turn
            turns += 1;
        }
        let _ = &mut ctx;
        assert!(decoder.is_exhausted());
    }
}
