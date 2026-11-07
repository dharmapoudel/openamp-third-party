// Openamp patch (2.5.2): regression tests for the external review of Openamp 2.5.1 (A1-VORBIS-1..5). The setup
// header pieces are written bit by bit (Vorbis packs LSB first) and fed to the same readers `read_setup` uses.

use symphonia_core::io::BitReaderRtl;

use crate::codebook::VorbisCodebook;
use crate::floor::Floor0;
use crate::residue::Residue;

/// LSB-first bit writer (the Vorbis packing).
#[derive(Default)]
struct Bits {
    out: Vec<u8>,
    n: usize,
}

impl Bits {
    fn put(&mut self, v: u64, bits: u32) -> &mut Self {
        for i in 0..bits {
            if self.n % 8 == 0 {
                self.out.push(0);
            }
            if (v >> i) & 1 != 0 {
                *self.out.last_mut().unwrap() |= 1 << (self.n % 8);
            }
            self.n += 1;
        }
        self
    }
    fn bytes(&mut self) -> Vec<u8> {
        self.out.extend_from_slice(&[0; 16]);
        std::mem::take(&mut self.out)
    }
}

/// Codebook header with a length-ordered entry list (one run of `entries` codes of length 1 bit is not a valid
/// tree, but the readers under test fail before the tree is built, or the test uses 2 entries).
fn codebook_head(b: &mut Bits, dims: u64, entries: u64) {
    b.put(0x564342, 24).put(dims, 16).put(entries, 24);
    b.put(1, 1); // length ordered
    b.put(0, 5); // first length 1
    let bits = 32 - (entries as u32).leading_zeros(); // ilog(entries)
    b.put(entries, bits);
}

fn vq_head(b: &mut Bits, lookup_type: u64) {
    b.put(lookup_type, 4).put(0, 32).put(0, 32).put(0, 4).put(0, 1); // min, delta, value bits 1, sequence_p
}

fn read_codebook(data: &[u8]) -> symphonia_core::errors::Result<VorbisCodebook> {
    VorbisCodebook::read(&mut BitReaderRtl::new(data))
}

/// A1-VORBIS-1: entries 2^23 x dimensions 512 = 2^32 wrapped to 0 in u32, nothing was read, and the type-2 table
/// (17 GB of f32) was allocated: the process aborted.
#[test]
fn type2_codebook_whose_size_wraps_u32_is_an_error() {
    let mut b = Bits::default();
    codebook_head(&mut b, 512, 1 << 23);
    vq_head(&mut b, 2);
    assert!(read_codebook(&b.bytes()).is_err());
}

/// Same family: a type-1 table needs only lookup1_values (= 1 here) multiplicands, but unpacks to entries x
/// dimensions floats (2^32 here): capped.
#[test]
fn type1_codebook_with_a_huge_unpacked_table_is_an_error() {
    let mut b = Bits::default();
    codebook_head(&mut b, 512, 1 << 23);
    vq_head(&mut b, 1);
    b.put(0, 1); // the one multiplicand
    assert!(read_codebook(&b.bytes()).is_err());
}

/// A1-VORBIS-2: entries 2^24 - 1, dimensions 2: `powf` rounded up to 4096 and the assert panicked.
#[test]
fn type1_codebook_at_the_float_rounding_boundary_does_not_panic() {
    let mut b = Bits::default();
    codebook_head(&mut b, 2, (1 << 24) - 1);
    vq_head(&mut b, 1);
    let data = b.bytes();
    let r = std::panic::catch_unwind(|| read_codebook(&data).map(|_| ()));
    // 4095 multiplicands of 1 bit are not in the packet: an error, not a panic.
    assert!(r.expect("codebook reader panicked").is_err());
}

/// A1-VORBIS-4: a 0-dimension codebook is rejected when read (it panicked later in residue / floor 0 decoding).
#[test]
fn zero_dimension_codebook_is_an_error() {
    for lookup in [0u64, 1, 2] {
        let mut b = Bits::default();
        codebook_head(&mut b, 0, 2);
        vq_head(&mut b, lookup);
        assert!(read_codebook(&b.bytes()).is_err(), "lookup type {}", lookup);
    }
}

/// A valid 2-entry, 1-dimension VQ codebook (both codes 1 bit long).
fn small_vq_codebook() -> VorbisCodebook {
    let mut b = Bits::default();
    b.put(0x564342, 24).put(1, 16).put(2, 24);
    b.put(0, 1).put(0, 1); // not ordered, dense
    b.put(0, 5).put(0, 5); // two lengths of 1
    vq_head(&mut b, 1);
    b.put(0, 1).put(1, 1); // lookup1_values(2, 1) = 2 multiplicands
    read_codebook(&b.bytes()).expect("valid codebook")
}

/// A1-VORBIS-3: the residue classbook was not checked against the number of codebooks (panic at the first audio
/// packet).
#[test]
fn residue_classbook_past_the_codebooks_is_an_error() {
    for residue_type in 0..3u16 {
        let mut b = Bits::default();
        b.put(0, 24).put(16, 24).put(0, 24); // begin, end, partition size 1
        b.put(0, 6); // 1 classification
        b.put(200, 8); // classbook 200 of 2
        b.put(0, 3).put(0, 1); // no VQ books used
        let data = b.bytes();
        assert!(Residue::try_read(&mut BitReaderRtl::new(&data), residue_type, 2).is_err());
        // The same residue with classbook 1 is accepted.
        let mut b = Bits::default();
        b.put(0, 24).put(16, 24).put(0, 24).put(0, 6).put(1, 8).put(0, 3).put(0, 1);
        let data = b.bytes();
        assert!(Residue::try_read(&mut BitReaderRtl::new(&data), residue_type, 2).is_ok());
    }
}

/// A1-VORBIS-5: a floor 0 book index past the books in use silently picked codebook 0 (9 books, index 12), and
/// with 16 books (a 5-bit index) indexed past the 16-entry list.
#[test]
fn floor0_book_index_past_the_book_list_is_an_error() {
    let books = [small_vq_codebook()];
    for (num_books, idx, idx_bits) in [(9u64, 12u64, 4u32), (16, 20, 5), (9, 8, 4)] {
        let mut b = Bits::default();
        b.put(2, 8).put(44100, 16).put(32, 16); // order 2, rate, bark map size
        b.put(8, 6).put(0, 8); // amplitude bits, offset
        b.put(num_books - 1, 4);
        for _ in 0..num_books {
            b.put(0, 8);
        }
        let data = b.bytes();
        let mut floor = Floor0::try_read(&mut BitReaderRtl::new(&data), 8, 11, 1).expect("floor setup");
        let mut p = Bits::default();
        p.put(1, 8).put(idx, idx_bits); // amplitude 1, book index
        let packet = p.bytes();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            floor.read_channel(&mut BitReaderRtl::new(&packet), &books)
        }));
        let r = r.expect("floor 0 panicked");
        if idx < num_books {
            assert!(r.is_ok(), "index {} of {}: {:?}", idx, num_books, r);
        } else {
            assert!(r.is_err(), "index {} of {}", idx, num_books);
        }
    }
}
