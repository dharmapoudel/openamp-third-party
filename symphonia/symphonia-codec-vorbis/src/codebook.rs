// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{decode_error, Result};
use symphonia_core::io::{
    vlc::{BitOrder, Codebook, CodebookBuilder, Entry32x32},
    ReadBitsRtl,
};

use super::common::*;

/// As defined in section 9.2.2 of the Vorbis I specification.
///
/// `float32_unpack` is intended to translate the packed binary representation of a Vorbis
/// codebook float value into the representation used by the decoder for floating point numbers.
#[inline(always)]
fn float32_unpack(x: u32) -> f32 {
    let mantissa = x & 0x1fffff;
    let sign = x & 0x80000000;
    let exponent = (x & 0x7fe00000) >> 21;
    let value = (mantissa as f32) * 2.0f32.powi(exponent as i32 - 788);
    if sign == 0 {
        value
    }
    else {
        -value
    }
}

/// As defined in section 9.2.3 of the Vorbis I specification: the greatest `v` with `v^dimensions <= entries`.
///
/// Openamp patch (2.5.2): integer search instead of `powf` + `assert!` (float rounding made the asserts fail for
/// e.g. entries = 2^24 - 1, dimensions = 2, and `dimensions == 0` gave `1/0`), and an error instead of a panic.
fn lookup1_values(entries: u32, dimensions: u16) -> Result<u32> {
    if dimensions == 0 {
        return decode_error("vorbis: codebook with zero dimensions");
    }
    let fits = |v: u32| v.checked_pow(u32::from(dimensions)).map(|p| p <= entries).unwrap_or(false);
    // Start from the float estimate, then correct it exactly.
    let mut v = (f64::from(entries)).powf(1.0 / f64::from(dimensions)).floor() as u32;
    while v > 0 && !fits(v) {
        v -= 1;
    }
    while fits(v + 1) {
        v += 1;
    }
    Ok(v)
}

/// Openamp patch (2.5.2): largest unpacked VQ table (entries x dimensions floats) of one codebook, 16 MiB. Encoder
/// codebooks are a few thousand values; the field widths allow 2^24 x 65535.
pub(crate) const MAX_VQ_TABLE_LEN: u64 = 1 << 22;

/// As defined in section 3.2.1 of the Vorbis I specification.
fn unpack_vq_lookup_type1(
    multiplicands: &[u16],
    min_value: f32,
    delta_value: f32,
    sequence_p: bool,
    codebook_entries: u32,
    codebook_dimensions: u16,
    lookup_values: u32,
) -> Vec<f32> {
    let mut vq_lookup = vec![0.0; codebook_entries as usize * codebook_dimensions as usize];

    for (v, value_vector) in vq_lookup.chunks_exact_mut(codebook_dimensions as usize).enumerate() {
        let lookup_offset = v as u32;

        let mut last = 0.0;
        let mut index_divisor = 1;

        for value in value_vector.iter_mut() {
            let multiplicand_offset = ((lookup_offset / index_divisor) % lookup_values) as usize;

            *value = f32::from(multiplicands[multiplicand_offset]) * delta_value + min_value + last;

            if sequence_p {
                last = *value;
            }

            index_divisor *= lookup_values;
        }
    }

    vq_lookup
}

/// As defined in section 3.2.1 of the Vorbis I specification.
fn unpack_vq_lookup_type2(
    multiplicands: &[u16],
    min_value: f32,
    delta_value: f32,
    sequence_p: bool,
    codebook_entries: u32,
    codebook_dimensions: u16,
) -> Vec<f32> {
    let mut vq_lookup = vec![0.0; codebook_entries as usize * codebook_dimensions as usize];

    for (lookup_offset, value_vector) in
        vq_lookup.chunks_exact_mut(codebook_dimensions as usize).enumerate()
    {
        let mut last = 0.0;
        let mut multiplicand_offset = lookup_offset * codebook_dimensions as usize;

        for value in value_vector.iter_mut() {
            *value = f32::from(multiplicands[multiplicand_offset]) * delta_value + min_value + last;

            if sequence_p {
                last = *value;
            }

            multiplicand_offset += 1;
        }
    }

    vq_lookup
}

fn synthesize_codewords(code_lens: &[u8]) -> Result<Vec<u32>> {
    // This codeword generation algorithm works by maintaining a table of the next valid codeword for
    // each codeword length.
    //
    // Consider a huffman tree. Each level of the tree correlates to a specific length of codeword.
    // For example, given a leaf node at level 2 of the huffman tree, that codeword would be 2 bits
    // long. Therefore, the table being maintained contains the codeword that would identify the next
    // available left-most node in the huffman tree at a given level. Therefore, this table can be
    // interrogated to get the next codeword in a simple lookup and the tree will fill-out in the
    // canonical order.
    //
    // Note however that, after selecting a codeword, C, of length N, all codewords of length > N
    // cannot use C as a prefix anymore. Therefore, all table entries for codeword lengths > N must
    // be updated such that these codewords are skipped over. Likewise, the table must be updated for
    // lengths < N to account for jumping between nodes.
    //
    // This algorithm is a modified version of the one found in the Vorbis reference implementation.
    let mut codewords = Vec::new();

    let mut next_codeword = [0u32; 33];

    let mut num_sparse = 0;

    for &len in code_lens.iter() {
        // This should always be true.
        debug_assert!(len <= 32);

        if len == 0 {
            num_sparse += 1;
            codewords.push(0);
            continue;
        }

        // The codeword length, N.
        let codeword_len = usize::from(len);

        // The selected codeword, C.
        let codeword = next_codeword[codeword_len];

        if len < 32 && (codeword >> len) > 0 {
            return decode_error("vorbis: codebook overspecified");
        }

        for i in (0..codeword_len + 1).rev() {
            // If the least significant bit (LSb) of the next codeword for codewords of length N
            // toggles from 1 to 0, that indicates the next-least-LSb will toggle. This means that
            // the next codeword will branch off a new parent node. Therefore, the next codeword for
            // codewords of length N will use the next codeword for codewords of length N-1 as its
            // prefix.
            if next_codeword[i] & 1 == 1 {
                // Openamp patch (2.5.2, spec-fmt): the root toggling means the tree is full (more codewords than
                // the lengths allow): an error, not an index of -1 (overflow panic; found by the MKA damage test).
                if i == 0 {
                    return decode_error("vorbis: codebook overspecified");
                }
                next_codeword[i] = next_codeword[i - 1] << 1;
                break;
            }

            // Otherwise, simply increment the next codeword for codewords of length N by 1. Iterate
            // again since there is now 1 branch dangling off the parent node. The parent must now be
            // incremented updated in the same way.
            next_codeword[i] += 1;
        }

        // Given a codeword, C, of length N bits, the codeword is a leaf on the tree and cannot have
        // any branches. Otherwise, another codeword would have C as its prefix and that is not
        // allowed. Therefore, if the next codeword for codewords of length N+1 uses codeword C as a
        // prefix, then the next codeword for codewords of length N+1 must be modified to branch off
        // the next codeword of length N instead. Then this modification must be propagated down the
        // tree in a similar pattern. In this way, all next codewords for codewords of lengths > N
        // that would've used C as a prefix are skipped over and can't be selected regardless of the
        // length of the next codeword.
        let branch = next_codeword[codeword_len];

        for (i, next) in next_codeword[codeword_len..].iter_mut().enumerate().skip(1) {
            // If the next codeword for this length of codewords is using the selected codeword, C,
            // as a prefix, move it to the next branch.
            if *next == codeword << i {
                *next = branch << i;
            }
            else {
                break;
            }
        }

        // Push the codeword.
        codewords.push(codeword);
    }

    // Check that the tree is fully specified and complete. This means that the next codeword for
    // codes of length 1 to 32, inclusive, are saturated.
    let is_underspecified =
        next_codeword.iter().enumerate().skip(1).any(|(i, &c)| c & (u32::MAX >> (32 - i)) != 0);

    // Single entry codebooks are technically invalid, but must be supported as a special-case
    // per Vorbis I specification, errate 20150226.
    let is_single_entry_codebook = code_lens.len() - num_sparse == 1;

    if is_underspecified && !is_single_entry_codebook {
        return decode_error("vorbis: codebook underspecified");
    }

    Ok(codewords)
}

pub struct VorbisCodebook {
    codebook: Codebook<Entry32x32>,
    dimensions: u16,
    vq_vec: Option<Vec<f32>>,
}

impl VorbisCodebook {
    pub fn read<B: ReadBitsRtl>(bs: &mut B) -> Result<Self> {
        // Verify codebook synchronization word.
        let sync = bs.read_bits_leq32(24)?;

        if sync != 0x564342 {
            return decode_error("vorbis: invalid codebook sync");
        }

        // Read codebook number of dimensions and entries.
        let codebook_dimensions = bs.read_bits_leq32(16)? as u16;
        let codebook_entries = bs.read_bits_leq32(24)?;

        // Openamp patch (2.5.2): a 0-dimension codebook is meaningless and made the residue (`step_by(0)`) and
        // floor 0 decoders panic later (external review A1-VORBIS-4).
        if codebook_dimensions == 0 {
            return decode_error("vorbis: codebook with zero dimensions");
        }

        // Ordered flag.
        let is_length_ordered = bs.read_bool()?;

        let mut code_lens = Vec::<u8>::with_capacity(codebook_entries as usize);

        if !is_length_ordered {
            // Codeword list is not length ordered.
            let is_sparse = bs.read_bool()?;

            if is_sparse {
                // Sparsely packed codeword entry list.
                for _ in 0..codebook_entries {
                    let is_used = bs.read_bool()?;

                    let code_len = if is_used {
                        // Entry is used.
                        bs.read_bits_leq32(5)? as u8 + 1
                    }
                    else {
                        // Unused entries have a length of 0.
                        0
                    };

                    code_lens.push(code_len);
                }
            }
            else {
                // Densely packed codeword entry list.
                for _ in 0..codebook_entries {
                    let code_len = bs.read_bits_leq32(5)? as u8 + 1;
                    code_lens.push(code_len)
                }
            }
        }
        else {
            // Codeword list is length ordered.
            let mut cur_entry = 0;
            let mut cur_len = bs.read_bits_leq32(5)? + 1;

            loop {
                let num_bits = if codebook_entries > cur_entry {
                    ilog(codebook_entries - cur_entry)
                }
                else {
                    0
                };

                let num = bs.read_bits_leq32(num_bits)?;

                code_lens.extend(std::iter::repeat(cur_len as u8).take(num as usize));

                cur_len += 1;
                cur_entry += num;

                if cur_entry > codebook_entries {
                    return decode_error("vorbis: invalid codebook");
                }

                if cur_entry == codebook_entries {
                    break;
                }
            }
        }

        // Read and unpack vector quantization (VQ) lookup table.
        let lookup_type = bs.read_bits_leq32(4)?;

        let vq_vec = match lookup_type & 0xf {
            0 => None,
            1 | 2 => {
                let min_value = float32_unpack(bs.read_bits_leq32(32)?);
                let delta_value = float32_unpack(bs.read_bits_leq32(32)?);
                let value_bits = bs.read_bits_leq32(4)? + 1;
                let sequence_p = bs.read_bool()?;

                // Lookup type is either 1 or 2 as per outer match.
                // Openamp patch (2.5.2): the type-2 product wrapped (entries 2^23 x dimensions 512 = 2^32 -> 0
                // values read, then a 17 GB table was allocated and the process aborted; external review
                // A1-VORBIS-1). Checked now, and the unpacked table (entries x dimensions floats) is capped.
                let lookup_values = match lookup_type {
                    1 => lookup1_values(codebook_entries, codebook_dimensions)?,
                    2 => match codebook_entries.checked_mul(u32::from(codebook_dimensions)) {
                        Some(n) => n,
                        None => return decode_error("vorbis: codebook lookup table too large"),
                    },
                    _ => unreachable!(),
                };

                let table_len = u64::from(codebook_entries) * u64::from(codebook_dimensions);
                if table_len > MAX_VQ_TABLE_LEN {
                    return decode_error("vorbis: codebook lookup table too large");
                }

                let mut multiplicands = Vec::<u16>::new();

                for _ in 0..lookup_values {
                    multiplicands.push(bs.read_bits_leq32(value_bits)? as u16);
                }

                let vq_lookup = match lookup_type {
                    1 => unpack_vq_lookup_type1(
                        &multiplicands,
                        min_value,
                        delta_value,
                        sequence_p,
                        codebook_entries,
                        codebook_dimensions,
                        lookup_values,
                    ),
                    2 => unpack_vq_lookup_type2(
                        &multiplicands,
                        min_value,
                        delta_value,
                        sequence_p,
                        codebook_entries,
                        codebook_dimensions,
                    ),
                    _ => unreachable!(),
                };

                Some(vq_lookup)
            }
            _ => return decode_error("vorbis: invalid codeword lookup type"),
        };

        // Generate a canonical list of codewords given the set of codeword lengths.
        let code_words = synthesize_codewords(&code_lens)?;

        // Generate the values associated for each codeword.
        // TODO: Should unused entries be 0 or actually the correct value?
        let values: Vec<u32> = (0..codebook_entries).collect();

        // Finally, generate the codebook with a reverse (LSb) bit order.
        let mut builder = CodebookBuilder::new_sparse(BitOrder::Reverse);

        // Read in 8-bit blocks.
        builder.bits_per_read(8);

        let codebook = builder.make::<Entry32x32>(&code_words, &code_lens, &values)?;

        Ok(VorbisCodebook { codebook, dimensions: codebook_dimensions, vq_vec })
    }

    #[inline(always)]
    pub fn read_scalar<B: ReadBitsRtl>(&self, bs: &mut B) -> Result<u32> {
        // An entry in a scalar codebook is just the value.
        Ok(bs.read_codebook(&self.codebook)?.0)
    }

    #[inline(always)]
    pub fn read_vq<B: ReadBitsRtl>(&self, bs: &mut B) -> Result<&[f32]> {
        // An entry in a VQ codebook is the index of the VQ vector.
        let entry = bs.read_codebook(&self.codebook)?.0;

        if let Some(vq) = &self.vq_vec {
            let dim = self.dimensions as usize;
            let start = dim * entry as usize;

            Ok(&vq[start..start + dim])
        }
        else {
            decode_error("vorbis: not a vq codebook")
        }
    }

    /// Openamp patch (2.5.2): floats held by the unpacked VQ table (0 for a scalar codebook).
    pub fn vq_len(&self) -> usize {
        self.vq_vec.as_ref().map(|v| v.len()).unwrap_or(0)
    }

    #[inline(always)]
    pub fn dimensions(&self) -> u16 {
        self.dimensions
    }
}

#[cfg(test)]
mod tests {
    use super::{ilog, lookup1_values, synthesize_codewords};

    #[test]
    fn verify_ilog() {
        assert_eq!(ilog(0), 0);
        assert_eq!(ilog(1), 1);
        assert_eq!(ilog(2), 2);
        assert_eq!(ilog(3), 2);
        assert_eq!(ilog(4), 3);
        assert_eq!(ilog(7), 3);
    }

    fn naive_lookup1_values(entries: u32, dimensions: u16) -> u32 {
        let mut x = 1u32;
        loop {
            let xpow = x.pow(u32::from(dimensions));
            if xpow > entries {
                break;
            }
            x += 1;
        }
        x - 1
    }

    #[test]
    fn verify_lookup1_values() {
        assert_eq!(lookup1_values(1, 1).unwrap(), naive_lookup1_values(1, 1));
        assert_eq!(lookup1_values(361, 2).unwrap(), naive_lookup1_values(361, 2));
    }

    // Openamp patch (2.5.2): external review A1-VORBIS-2 (float rounding tripped the asserts) and A1-VORBIS-4.
    #[test]
    fn lookup1_values_is_exact_at_rounding_boundaries_and_rejects_zero_dimensions() {
        assert_eq!(lookup1_values(16_777_215, 2).unwrap(), 4095);
        assert_eq!(lookup1_values(16_777_216, 2).unwrap(), 4096);
        assert_eq!(lookup1_values(16_777_215, 3).unwrap(), 255);
        assert_eq!(lookup1_values(16_777_215, 65535).unwrap(), 1);
        assert_eq!(lookup1_values(0, 4).unwrap(), 0);
        for e in [2u32, 80, 81, 82, 6560, 6561, 6562, 1 << 20, (1 << 24) - 1] {
            for d in 1u16..=8 {
                assert_eq!(lookup1_values(e, d).unwrap(), naive_lookup1_values(e, d), "{} {}", e, d);
            }
        }
        assert!(lookup1_values(100, 0).is_err());
    }

    #[test]
    fn verify_synthesize_codewords() {
        const CODEWORD_LENGTHS: &[u8] = &[2, 4, 4, 4, 4, 2, 3, 3];
        const EXPECTED_CODEWORDS: &[u32] = &[0, 0x4, 0x5, 0x6, 0x7, 0x2, 0x6, 0x7];
        let codewords = synthesize_codewords(CODEWORD_LENGTHS).unwrap();
        assert_eq!(&codewords, EXPECTED_CODEWORDS);
    }
}
