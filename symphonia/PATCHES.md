# Symphonia 0.5.5, patched for Openamp 2.5.1

Source: the crates.io 0.5.5 packages (upstream commit 6d533f26150953a882a6a111ebd13f0abf7129d5), license MPL-2.0
(the modified files stay under MPL-2.0). Wired in through `[patch.crates-io]` in `core/Cargo.toml`; the other
Symphonia crates (bundle-mp3, codec-alac, codec-pcm, codec-vorbis, format-ogg, utils-xiph, symphonia) still come
from crates.io unmodified.

Why: external fuzzing of Openamp 2.4.0 found allocations sized by untrusted file fields (multi-GB, the process
aborts: allocation failure is not a panic and cannot be caught) and panics in release builds. 0.6.x is a breaking API
rewrite, too large for a hotfix. Every change is marked `Openamp patch (2.5.1)` in the source.

| Crate / file | Change | Finding |
|---|---|---|
| core `src/io/mod.rs` | `read_boxed_slice_exact` grows the buffer in 1 MiB steps as data arrives; `read_boxed_slice` allocates at most 1 MiB up front (`OA_MAX_EAGER_ALLOC`). A length larger than the stream ends in `UnexpectedEof`. | FZ-MP3-1, FZ-TAG-4 (ID3v2 frames), FLAC PICTURE data, MP4 atoms |
| metadata `src/vorbis.rs` | comment bytes read with `read_boxed_slice_exact` instead of `vec![0; comment_length]` | FZ-FLAC-4, FZ-TAG-4 |
| metadata `src/flac.rs` | PICTURE media type / description read with `read_boxed_slice_exact` | FLAC PICTURE (same family) |
| format-riff `src/wave/chunks.rs` | WAV `fmt` sample rate 0 is a decode error; INFO values read with `read_boxed_slice_exact` | FZ-TAG-5 (TimeBase panic) |
| format-riff `src/aiff/chunks.rs` | AIFF/AIFC sample rate 0 is a decode error | same family as FZ-TAG-5 |
| codec-aac `src/aac/ics/mod.rs` | section counter bounded by `MAX_SFBS` (zero-length sections never advanced) | FZ-FLAC-2 (release panic) |
| codec-aac `src/adts.rs` | `approximate_frame_count`: checked `len - pos`, `total_len` no longer subtracted twice, step at least 1 | FZ-FLAC-3, FZ-OPUS-1, FZ-OPUS-2, FZ-TAG-6 |
| bundle-flac `src/demuxer.rs` | seek-table offsets saturating, search range clamped to the stream and ordered, midpoint without overflow | FZ-FLAC-1, FZ-TAG-7 |
| format-isomp4 `src/atoms/mod.rs` | next atom position saturating | FZ-TAG-8 |
| format-isomp4 `src/atoms/mdhd.rs` | timescale 0 is a decode error | TimeBase panic (same family as FZ-TAG-5) |
| format-isomp4 `src/atoms/{stco,co64,stsc,stsz,stts}.rs` | `Vec::with_capacity(count)` capped at 65,536 entries (the vector still grows as entries are read) | unbounded allocation from a 32-bit count |

Regression tests: `crates/oa-decode/tests/hardening.rs` (fixtures in `fixtures/fuzz/`), corpus replay
`crates/oa-decode/tests/corpus.rs`. To update Symphonia: replace these directories with the new release and re-apply
(or drop) each patch above.

## 2.5.2 (external review of 2.5.1)

Also vendored now (unmodified first, then patched): `symphonia-codec-vorbis`, `symphonia-format-ogg`,
`symphonia-codec-alac` (0.5.5). Changes are marked `Openamp patch (2.5.2)`.

| Crate / file | Change | Finding |
|---|---|---|
| codec-vorbis `src/codebook.rs` | 0-dimension codebooks rejected; `lookup1_values` exact integer search returning an error (no float `assert!`); type-2 `entries x dimensions` checked; unpacked VQ table capped at 2^22 floats per codebook | A1-VORBIS-1, -2, -4 |
| codec-vorbis `src/lib.rs` | all VQ tables of a stream together capped at 2^24 floats | A1-VORBIS-1 |
| codec-vorbis `src/residue.rs` | residue classbook checked against the number of codebooks | A1-VORBIS-3 |
| codec-vorbis `src/floor.rs` | floor 0 book index checked against the number of books | A1-VORBIS-5 |
| format-ogg `src/mappings/vorbis.rs` | the demuxer's codebook skipper: same `lookup1_values`, 0-dimension rejection, checked products | A1-VORBIS-1, -2, -4 |
| codec-alac `src/lib.rs` | cookie frame length must be 1..=2^20; partial frame longer than the frame length rejected; predictor warm-up bounded by the block length; clip-bit count saturating (33-bit channel pairs) | A1-ALAC-1, -2, -3 |
| codec-alac `src/lib.rs` | mid-side shift of 32 or more is a decode error; `decorrelate_mid_side` uses wrapping arithmetic and a masked shift | FZ2A-MP4-2 (verification of 2.5.1) |
| codec-aac `src/aac/ics/mod.rs` | `max_sfb` checked against the scale factor band table of the window shape and rate | A1-AAC-1 |
| codec-aac `src/aac/mod.rs` | explicit sampling frequency (index 15) read as the 24-bit field; channel configuration without a map is an error instead of an `unwrap` panic | A1-AAC-2 (+ found while there) |
| codec-aac `src/adts.rs` | `approximate_frame_count` sampling step converted with `usize::try_from` (never 0 on 32-bit) | G-11 |
| bundle-flac `src/decoder.rs` | block larger than STREAMINFO's maximum rejected before `render_reserved`; predictor order larger than the block rejected before the warm-up slice; Rice quotient that overflows 32 bits rejected | A1-FLAC-1, A1-FLAC-2 |
| format-isomp4 `src/atoms/trun.rs` | "first sample duration/size present" now means the per-sample list exists (it tested a bit of the first sample's flags value and indexed an empty list); no per-sample loop when no per-sample fields are present | A1-MP4-1 |
| format-isomp4 `src/atoms/stsc.rs`, `src/stream.rs` | first chunk 0 rejected; `first_sample` computed checked; chunk index sum checked | A1-MP4-2, G-06 |
| format-isomp4 `src/atoms/{mod,ilst,hdlr,ftyp,meta,stsd,esds}.rs` | `AtomHeader::data_len_after`: payload left after fixed fields, decode error when the atom is shorter (no wrap, no read-to-EOF); esds descriptor lengths checked | A1-MP4-3, A1-MP4-5; also covers FZ2A-MP4-1 of the 2.5.1 verification (its 5 sites `ilst.rs:512/536`, `meta.rs:49`, `hdlr.rs:75`, `esds.rs:60` all use it; its 9 repro files run in `oa-decode/tests/verify251.rs`) |
| format-isomp4 `src/atoms/stts.rs` | u64 / saturating accumulators; a sample number past u32 is None | A1-MP4-4 |
| format-riff `src/wave/chunks.rs` | extensible PCM with 0 bits per coded sample is a decode error (was `unreachable!()`); ADPCM frames per block in u32 with a checked subtraction; sample formats size packets by the frame length from the format, not `block_align` (0 divided by zero, 65535 made truncated 75 MB packets) | A1-WAV-1, A1-WAV-2, A1-RIFF-1 |

Tests: `format-isomp4 src/atoms/openamp_tests.rs`, `stts.rs` openamp_tests, `codec-vorbis src/openamp_tests.rs` (setup pieces written bit by bit), `codec-alac` `openamp_tests`
module, plus the decoder-level tests in `crates/oa-decode/tests/hardening.rs`.

Inline markers (2.5.2, verification of 2.5.1 Area 6 note): the two 2.5.1 sub-hunks that sat inside a marked change
without their own `Openamp patch` line now carry one: format-isomp4 `src/atoms/mod.rs` (`base_pos.saturating_add` in
`AtomIterator::next`) and metadata `src/flac.rs` (`desc_buf` read with `read_boxed_slice_exact`).

## 2.5.3 (Portal device log: remote seeks)

| Crate / file | Change | Why |
|---|---|---|
| bundle-flac `src/demuxer.rs` | seek: safeguarded interpolation search (byte offset interpolated from the timestamps at both ends of the range, aimed half a block before the target's block; once a probe shrinks the range by less than half, the rest bisects) instead of a plain binary search; the SEEKTABLE points' timestamps feed it too | a server song is read through HTTP byte ranges, every probe is a new request: ~13 for a 100 MB FLAC without a SEEKTABLE (a resume stayed silent 25 s). Same landing frame as before. Test: `crates/oa-decode/tests/remote_seek.rs` |
| bundle-flac `src/parser.rs` | `PacketParser::block_lens()` (min/max block length) for the search | same |

## 2.5.2 (spec A6 formats wave)

| Crate / file | Change | Finding |
|---|---|---|
| codec-vorbis `src/codebook.rs` | Huffman tree construction: a full tree (root toggles) is a decode error instead of indexing `next_codeword[-1]` (overflow panic in debug, out-of-bounds panic in release, both caught by the guard but the file failed as "malformed (open failed)") | found by the damaged-file test of Vorbis in Matroska (`oa-decode/tests/spec_formats.rs` `mka_codecs`); reachable from Ogg Vorbis too |

## 2.5.2 (fuzz T-0004 of main)

| Crate / file | Change | Finding |
|---|---|---|
| format-riff `src/aiff/chunks.rs` | AIFF-C COMM compression-name padding computed as `u16::from(str_len) + 1` (u8 `str_len + 1` overflowed for a 255-byte name: panic with overflow checks, silent wrap without) | T4-1 |

Tests: format-riff `src/aiff/chunks.rs` `openamp_tests` (COMM parser, name lengths 0/1/2/253/254/255), oa-decode
`tests/hardening.rs` `aifc_with_a_255_byte_compression_name_opens_and_plays` (whole AIFF-C through the decoder).
