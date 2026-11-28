# Crates of other projects, kept here with a change for Peekel

Not members of the workspace (`exclude` in `Cargo.toml`), so our lints do not judge their code; `hayro` and `hayro-syntax` take
the place of the crates.io ones through `[patch.crates-io]`. Each keeps its own licence files. Every change of ours is marked `PEEKEL`.

| Folder | What | From | Licence |
|---|---|---|---|
| `hayro` | hayro's renderer, 0.9.0 | crates.io, `hayro` 0.9.0 (github.com/LaurenzV/hayro); its example left out | MIT OR Apache-2.0 |
| `hayro-syntax` | hayro's PDF reader, 0.9.0 | crates.io, `hayro-syntax` 0.9.0 (github.com/LaurenzV/hayro) | MIT OR Apache-2.0 |
| `zune-jpeg` | the JPEG decoder, 0.5.16-rc4 | github.com/etemesi254/zune-image, branch `dev`, commit `b077e9badc9257e7e9b206f0a3f3f2fc7ad64227` (27 November 2026), `crates/zune-jpeg` | MIT OR Apache-2.0 OR Zlib |
| `zune-core` | what zune-jpeg is built on, 0.5.4 | the same commit, `crates/zune-core` | MIT OR Apache-2.0 OR Zlib |

**Why (27 November 2026).** A page of a PDF that is one large JPEG (a magazine, a scan in colour) was slow on a 32-bit ARM
tablet: about 0.57 s at any size drawn, since hayro decodes the picture whole and shrinks it after, and zune-jpeg's NEON code is
for 64-bit ARM only. `hayro-syntax`'s JPEG filter (`src/filter/dct.rs`) now decodes a picture drawn smaller than it is coded at
1/8, 1/4 or 1/2 of its size, the least that still covers the size it is drawn at, with the reduced IDCT that zune-jpeg has on its
`dev` branch only (`decode_scaled_into`, zune-image PR #498); MuPDF does the same with libjpeg's `scale_num`. The interpreter
already scales a smaller result back (`scale_factors`), as for JPEG 2000. Anything that fails falls back to the whole picture.
The other ratios (3/8, 5/8, 3/4, 7/8) are left out: they were slower than the whole picture. `set_reduced_jpeg_slack` lets a
picture fall a little below the size drawn (0 per cent by default). Measured: docs/development.md, "A JPEG decoded smaller".

Changes besides `dct.rs`: `src/filter/mod.rs` exports `set_reduced_jpeg_slack`; `hayro-syntax/Cargo.toml` takes `zune-jpeg` from
`../zune-jpeg`; the two zune crates allow two warnings of their own in their `Cargo.toml` (`unused_attributes`, `dead_code`).

**When to drop it.** When zune-jpeg publishes the reduced IDCT (0.5.16 or later), `zune-jpeg` and `zune-core` go and
`hayro-syntax` takes zune-jpeg from crates.io; when hayro decodes a JPEG at a reduced size itself, `hayro-syntax` goes too (the
change is to be offered to hayro). A newer hayro is taken by copying its `hayro-syntax` here again and making the change again.

**`hayro`: a picture shrunk by an area average on 32-bit ARM (27 November 2026).** hayro shrinks a picture drawn smaller than it
is with `pic-scale`, whose NEON is for `aarch64` only (Rust's NEON intrinsics for 32-bit ARM are unstable): a magazine page's
picture took 275 ms of a page's 570 ms on a 32-bit tablet. `src/shrink.rs` shrinks an RGB picture without alpha to no less than
half by an area average in 16-bit numbers, in loops the compiler makes NEON of, and `src/image.rs` calls it on 32-bit ARM only;
anything else goes to `pic-scale` as before. 116 ms, or 52 ms with the feature `nightly-neon`: the pass across a row in NEON
written by hand, the same bytes. That feature needs `RUSTC_BOOTSTRAP=1` (the intrinsics are unstable), so it is off by default
and only a 32-bit ARM build that sets both turns it on; `src/lib.rs` denies unsafe code instead of forbidding it so that the
NEON module may allow it. `shrink_rgb` is public (hidden) for `core/tests/shrink.rs`. Measured: docs/development.md, "Where a
page with a whole picture spends its time on 32-bit ARM". Other changes: `Cargo.toml` without the example, with the feature.

**When to drop it.** When `pic-scale` has SIMD on 32-bit ARM (once Rust makes the intrinsics stable, rust-lang/rust#111800), or
hayro shrinks another way that is as fast there. A newer hayro is taken by copying it here again and making the change again.
