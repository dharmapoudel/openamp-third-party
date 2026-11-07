// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::convert::TryFrom;

use symphonia_core::errors::Result;
use symphonia_core::io::ReadBytes;

use crate::atoms::{Atom, AtomHeader};

#[derive(Debug)]
pub struct SampleDurationEntry {
    pub sample_count: u32,
    pub sample_delta: u32,
}

/// Time-to-sample atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct SttsAtom {
    /// Atom header.
    header: AtomHeader,
    pub entries: Vec<SampleDurationEntry>,
    pub total_duration: u64,
}

impl SttsAtom {
    /// Get the timestamp and duration for the sample indicated by `sample_num`. Note, `sample_num`
    /// is indexed relative to the `SttsAtom`. Complexity of this function in O(N).
    pub fn find_timing_for_sample(&self, sample_num: u32) -> Option<(u64, u32)> {
        // Openamp patch (2.5.2): u64 / saturating accumulators. The u32 sample counter and the u64 timestamp sums
        // wrapped for tables whose counts add up past 2^32 (wrong timestamps and seek targets; external review
        // A1-MP4-4).
        let mut ts = 0u64;
        let mut next_entry_first_sample = 0u64;
        let sample_num64 = u64::from(sample_num);

        // The Stts atom compactly encodes a mapping between number of samples and sample duration.
        // Iterate through each entry until the entry containing the next sample is found. The next
        // packet timestamp is then the sum of the product of sample count and sample duration for
        // the n-1 iterated entries, plus the product of the number of consumed samples in the n-th
        // iterated entry and sample duration.
        for entry in &self.entries {
            let entry_first_sample = next_entry_first_sample;
            next_entry_first_sample += u64::from(entry.sample_count);

            if sample_num64 < next_entry_first_sample {
                let entry_sample_offset = sample_num64 - entry_first_sample;
                ts = ts.saturating_add(u64::from(entry.sample_delta).saturating_mul(entry_sample_offset));

                return Some((ts, entry.sample_delta));
            }

            ts = ts.saturating_add(u64::from(entry.sample_count) * u64::from(entry.sample_delta));
        }

        None
    }

    /// Get the sample that contains the timestamp indicated by `ts`. Note, the returned `sample_num`
    /// is indexed relative to the `SttsAtom`. Complexity of this function in O(N).
    pub fn find_sample_for_timestamp(&self, ts: u64) -> Option<u32> {
        // Openamp patch (2.5.2): see find_timing_for_sample; a sample number past u32 is None, not a wrapped one.
        let mut ts_accum = 0u64;
        let mut sample_num = 0u64;

        for entry in &self.entries {
            let delta = u64::from(entry.sample_delta) * u64::from(entry.sample_count);

            if ts_accum.saturating_add(delta) > ts {
                sample_num += (ts - ts_accum).checked_div(u64::from(entry.sample_delta))?;
                return u32::try_from(sample_num).ok();
            }

            ts_accum = ts_accum.saturating_add(delta);
            sample_num += u64::from(entry.sample_count);
        }

        None
    }
}

impl Atom for SttsAtom {
    fn header(&self) -> AtomHeader {
        self.header
    }

    fn read<B: ReadBytes>(reader: &mut B, header: AtomHeader) -> Result<Self> {
        let (_, _) = AtomHeader::read_extra(reader)?;

        let entry_count = reader.read_be_u32()?;

        let mut total_duration = 0;

        // TODO: Limit table length.
        // Openamp patch (2.5.1): the count comes from the file; reserve a bounded amount, grow as entries are read.
        let mut entries = Vec::with_capacity((entry_count as usize).min(1 << 16));

        for _ in 0..entry_count {
            let sample_count = reader.read_be_u32()?;
            let sample_delta = reader.read_be_u32()?;

            // Openamp patch (2.5.2): saturating (external review A1-MP4-4).
            total_duration = u64::saturating_add(total_duration, u64::from(sample_count) * u64::from(sample_delta));

            entries.push(SampleDurationEntry { sample_count, sample_delta });
        }

        Ok(SttsAtom { header, entries, total_duration })
    }
}

// Openamp patch (2.5.2): regression tests for the external review of Openamp 2.5.1 (A1-MP4-4).
#[cfg(test)]
mod openamp_tests {
    use super::*;
    use crate::atoms::AtomType;

    fn stts(entries: &[(u32, u32)]) -> SttsAtom {
        SttsAtom {
            header: AtomHeader { atype: AtomType::TimeToSample, atom_len: 0, data_len: 0 },
            entries: entries.iter().map(|&(sample_count, sample_delta)| SampleDurationEntry { sample_count, sample_delta }).collect(),
            total_duration: 0,
        }
    }

    #[test]
    fn sample_counts_past_u32_do_not_wrap() {
        // Two entries of 3e9 samples each: the second entry's samples are past 2^32.
        let t = stts(&[(3_000_000_000, 1024), (3_000_000_000, 512)]);
        // Sample 3e9 + 5 would be past u32; the first entry still maps exactly.
        assert_eq!(t.find_timing_for_sample(10), Some((10 * 1024, 1024)));
        // The largest u32 sample lies in the second entry: 3e9 * 1024 + (2^32 - 1 - 3e9) * 512.
        let n = u32::MAX;
        let expect = 3_000_000_000u64 * 1024 + (u64::from(n) - 3_000_000_000) * 512;
        assert_eq!(t.find_timing_for_sample(n), Some((expect, 512)));
        // Timestamp -> sample: inside the second entry but past u32 is None (it wrapped to a small sample before).
        let ts_far = 3_000_000_000u64 * 1024 + 2_000_000_000u64 * 512;
        assert_eq!(t.find_sample_for_timestamp(ts_far), None);
        assert_eq!(t.find_sample_for_timestamp(expect), Some(n));
        assert_eq!(t.find_sample_for_timestamp(2048), Some(2));
    }
}
