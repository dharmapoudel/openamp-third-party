// Openamp patch (2.5.2): regression tests for the external review of Openamp 2.5.1 (A1-MP4-1, -2, -3, -5, G-06).
// Atoms are read from in-memory payloads with the same `Atom::read` the demuxer uses.

use symphonia_core::io::BufReader;

use super::ilst::{MetaTagDataAtom, MetaTagNamespaceAtom};
use super::*;

fn header(atype: AtomType, data_len: u64) -> AtomHeader { AtomHeader { atype, atom_len: data_len + 8, data_len } }

fn read<A: Atom>(atype: AtomType, data_len: u64, payload: &[u8]) -> Result<A> {
    let r = std::panic::catch_unwind(|| A::read(&mut BufReader::new(payload), header(atype, data_len)).map(|a| a));
    r.unwrap_or_else(|_| panic!("atom reader panicked"))
}

fn be(words: &[u32]) -> Vec<u8> { words.iter().flat_map(|w| w.to_be_bytes()).collect() }

/// A1-MP4-1: trun with first-sample flags (atom flag 0x4) but no per-sample durations / sizes, and a first-sample
/// flags value with bit 0x4 set: the "first sample duration present" test read that value bit and indexed the
/// empty duration list.
#[test]
fn trun_with_first_sample_flags_only_uses_the_defaults() {
    let payload = be(&[0x0000_0004, 3, 0x0000_0004]);
    let trun: TrunAtom = read(AtomType::TrackFragmentRun, payload.len() as u64, &payload).unwrap();
    let r = std::panic::catch_unwind(|| {
        (trun.total_duration(1024), trun.total_size(100), trun.sample_timing(2, 1024), trun.sample_size(0, 100), trun.sample_offset(2, 100), trun.ts_sample(2500, 1024))
    });
    let (dur, size, timing, s0, off, ts) = r.expect("trun accessors panicked");
    assert_eq!((dur, size), (3 * 1024, 300));
    assert_eq!(timing, (2048, 1024));
    assert_eq!(s0, 100);
    assert_eq!(off, (200, 100));
    assert_eq!(ts, 2);
    // With per-sample durations and sizes the lists are used.
    let payload = be(&[0x0000_0304, 2, 0x0000_0004, 10, 7, 20, 9]);
    let trun: TrunAtom = read(AtomType::TrackFragmentRun, payload.len() as u64, &payload).unwrap();
    assert_eq!((trun.total_duration(1024), trun.total_size(100)), (30, 16));
    assert_eq!(trun.sample_timing(1, 1024), (10, 20));
}

/// A huge sample count with no per-sample fields reads nothing and returns at once (it looped 2^32 times).
#[test]
fn trun_with_a_huge_count_and_no_sample_fields_is_quick() {
    let payload = be(&[0, u32::MAX]);
    let t0 = std::time::Instant::now();
    let trun: TrunAtom = read(AtomType::TrackFragmentRun, payload.len() as u64, &payload).unwrap();
    assert!(t0.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(trun.total_duration(1), u64::from(u32::MAX));
}

fn stsc(entries: &[[u32; 3]]) -> Result<StscAtom> {
    let mut words = vec![0, entries.len() as u32];
    for e in entries {
        words.extend_from_slice(e);
    }
    let payload = be(&words);
    read(AtomType::SampleToChunk, payload.len() as u64, &payload)
}

/// A1-MP4-2 / G-06: a 0 first-chunk field wrapped to u32::MAX (a single entry passed the monotonic check), and
/// `n * samples_per_chunk` wrapped `first_sample`.
#[test]
fn stsc_zero_first_chunk_and_sample_overflow_are_errors() {
    assert!(stsc(&[[0, 10, 1]]).is_err());
    assert!(stsc(&[[1, 10, 1], [0, 10, 1]]).is_err());
    assert!(stsc(&[[1, u32::MAX, 1], [3, 10, 1]]).is_err());
    let ok = stsc(&[[1, 10, 1], [3, 4, 1], [5, 2, 1]]).unwrap();
    let firsts: Vec<u32> = ok.entries.iter().map(|e| e.first_sample).collect();
    assert_eq!(firsts, vec![0, 20, 28]);
    assert_eq!(ok.entries[1].first_chunk, 2);
}

/// A1-MP4-3 / A1-MP4-5 family: atoms shorter than their fixed fields. The subtraction wrapped and the reader (not
/// scoped to the atom) read on to the end of the file: here a 4 MiB tail stands in for the rest of a file.
#[test]
fn atoms_shorter_than_their_fixed_fields_are_errors() {
    let mut payload = vec![0u8; 24];
    payload[8..12].copy_from_slice(b"soun");
    payload.resize(4 << 20, 0x41);
    assert!(read::<HdlrAtom>(AtomType::Handler, 10, &payload).is_err());
    assert!(read::<HdlrAtom>(AtomType::Handler, 0, &payload).is_err());
    assert_eq!(read::<HdlrAtom>(AtomType::Handler, 28, &payload).unwrap().name.len(), 4);
    let mut data = vec![0, 0, 0, 1];
    data.resize(4 << 20, 0x41);
    assert!(read::<MetaTagDataAtom>(AtomType::MetaTagData, 6, &data).is_err());
    assert!(read::<MetaTagNamespaceAtom>(AtomType::MetaTagName, 2, &data).is_err());
    assert!(read::<FtypAtom>(AtomType::FileType, 4, &data).is_err());
    assert!(read::<EsdsAtom>(AtomType::Esds, 2, &data).is_err());
}
