// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// Previous Author: Kostya Shishkov <kostya.shiskov@gmail.com>
//
// This source file includes code originally written for the NihAV
// project. With the author's permission, it has been relicensed for,
// and ported to the Symphonia project.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::audio::{AsAudioBufferRef, AudioBuffer, AudioBufferRef, Signal, SignalSpec};
use symphonia_core::codecs::{CodecDescriptor, CodecParameters, CODEC_TYPE_AAC};
use symphonia_core::codecs::{Decoder, DecoderOptions, FinalizeResult};
use symphonia_core::errors::{unsupported_error, Result};
use symphonia_core::formats::Packet;
use symphonia_core::io::{BitReaderLtr, FiniteBitStream, ReadBitsLtr};
use symphonia_core::support_codec;
use symphonia_core::units::Duration;

mod codebooks;
mod common;
mod cpe;
mod dsp;
mod ics;
mod window;

use crate::common::*;
use common::*;

struct M4AInfo {
    otype: M4AType,
    srate: u32,
    channels: usize,
    samples: usize,
    sbr_ps_info: Option<(u32, usize)>,
    sbr_present: bool,
    ps_present: bool,
}

impl M4AInfo {
    fn new() -> Self {
        Self {
            otype: M4AType::None,
            srate: 0,
            channels: 0,
            samples: 0,
            sbr_ps_info: Option::None,
            sbr_present: false,
            ps_present: false,
        }
    }

    fn read_object_type<B: ReadBitsLtr>(bs: &mut B) -> Result<M4AType> {
        let otypeidx = match bs.read_bits_leq32(5)? {
            idx if idx < 31 => idx as usize,
            31 => (bs.read_bits_leq32(6)? + 32) as usize,
            _ => unreachable!(),
        };

        if otypeidx >= M4A_TYPES.len() {
            Ok(M4AType::Unknown)
        }
        else {
            Ok(M4A_TYPES[otypeidx])
        }
    }

    fn read_sampling_frequency<B: ReadBitsLtr>(bs: &mut B) -> Result<u32> {
        match bs.read_bits_leq32(4)? {
            idx if idx < 15 => Ok(AAC_SAMPLE_RATES[idx as usize]),
            _ => {
                // Openamp patch (2.5.2): an explicit rate is a 24-bit field; `(0xf << 20) & read(20)` was always 0,
                // so every MP4 AAC with an explicit rate was rejected (external review A1-AAC-2).
                let srate = bs.read_bits_leq32(24)?;
                Ok(srate)
            }
        }
    }

    fn read_channel_config<B: ReadBitsLtr>(bs: &mut B) -> Result<usize> {
        let chidx = bs.read_bits_leq32(4)? as usize;
        if chidx < AAC_CHANNELS.len() {
            Ok(AAC_CHANNELS[chidx])
        }
        else {
            Ok(chidx)
        }
    }

    fn read(&mut self, buf: &[u8]) -> Result<()> {
        let mut bs = BitReaderLtr::new(buf);

        self.otype = Self::read_object_type(&mut bs)?;
        self.srate = Self::read_sampling_frequency(&mut bs)?;

        validate!(self.srate > 0);

        self.channels = Self::read_channel_config(&mut bs)?;

        if (self.otype == M4AType::Sbr) || (self.otype == M4AType::PS) {
            let ext_srate = Self::read_sampling_frequency(&mut bs)?;
            self.otype = Self::read_object_type(&mut bs)?;

            let ext_chans = if self.otype == M4AType::ER_BSAC {
                Self::read_channel_config(&mut bs)?
            }
            else {
                0
            };

            self.sbr_ps_info = Some((ext_srate, ext_chans));
        }

        match self.otype {
            M4AType::Main
            | M4AType::Lc
            | M4AType::Ssr
            | M4AType::Scalable
            | M4AType::TwinVQ
            | M4AType::ER_AAC_LC
            | M4AType::ER_AAC_LTP
            | M4AType::ER_AAC_Scalable
            | M4AType::ER_TwinVQ
            | M4AType::ER_BSAC
            | M4AType::ER_AAC_LD => {
                // GASpecificConfig
                let short_frame = bs.read_bool()?;

                self.samples = if short_frame { 960 } else { 1024 };

                let depends_on_core = bs.read_bool()?;

                if depends_on_core {
                    let _delay = bs.read_bits_leq32(14)?;
                }

                let extension_flag = bs.read_bool()?;

                if self.channels == 0 {
                    return unsupported_error("aac: program config element");
                }

                if (self.otype == M4AType::Scalable) || (self.otype == M4AType::ER_AAC_Scalable) {
                    let _layer = bs.read_bits_leq32(3)?;
                }

                if extension_flag {
                    if self.otype == M4AType::ER_BSAC {
                        let _num_subframes = bs.read_bits_leq32(5)? as usize;
                        let _layer_length = bs.read_bits_leq32(11)?;
                    }

                    if (self.otype == M4AType::ER_AAC_LC)
                        || (self.otype == M4AType::ER_AAC_LTP)
                        || (self.otype == M4AType::ER_AAC_Scalable)
                        || (self.otype == M4AType::ER_AAC_LD)
                    {
                        let _section_data_resilience = bs.read_bool()?;
                        let _scalefactors_resilience = bs.read_bool()?;
                        let _spectral_data_resilience = bs.read_bool()?;
                    }

                    let extension_flag3 = bs.read_bool()?;

                    if extension_flag3 {
                        return unsupported_error("aac: version3 extensions");
                    }
                }
            }
            M4AType::Celp => {
                return unsupported_error("aac: CELP config");
            }
            M4AType::Hvxc => {
                return unsupported_error("aac: HVXC config");
            }
            M4AType::Ttsi => {
                return unsupported_error("aac: TTS config");
            }
            M4AType::MainSynth
            | M4AType::WavetableSynth
            | M4AType::GeneralMIDI
            | M4AType::Algorithmic => {
                return unsupported_error("aac: structured audio config");
            }
            M4AType::ER_CELP => {
                return unsupported_error("aac: ER CELP config");
            }
            M4AType::ER_HVXC => {
                return unsupported_error("aac: ER HVXC config");
            }
            M4AType::ER_HILN | M4AType::ER_Parametric => {
                return unsupported_error("aac: parametric config");
            }
            M4AType::Ssc => {
                return unsupported_error("aac: SSC config");
            }
            M4AType::MPEGSurround => {
                // bs.ignore_bits(1)?; // sacPayloadEmbedding
                return unsupported_error("aac: MPEG Surround config");
            }
            M4AType::Layer1 | M4AType::Layer2 | M4AType::Layer3 => {
                return unsupported_error("aac: MPEG Layer 1/2/3 config");
            }
            M4AType::Dst => {
                return unsupported_error("aac: DST config");
            }
            M4AType::Als => {
                // bs.ignore_bits(5)?; // fillBits
                return unsupported_error("aac: ALS config");
            }
            M4AType::Sls | M4AType::SLSNonCore => {
                return unsupported_error("aac: SLS config");
            }
            M4AType::ER_AAC_ELD => {
                return unsupported_error("aac: ELD config");
            }
            M4AType::SMRSimple | M4AType::SMRMain => {
                return unsupported_error("aac: symbolic music config");
            }
            _ => {}
        };

        match self.otype {
            M4AType::ER_AAC_LC
            | M4AType::ER_AAC_LTP
            | M4AType::ER_AAC_Scalable
            | M4AType::ER_TwinVQ
            | M4AType::ER_BSAC
            | M4AType::ER_AAC_LD
            | M4AType::ER_CELP
            | M4AType::ER_HVXC
            | M4AType::ER_HILN
            | M4AType::ER_Parametric
            | M4AType::ER_AAC_ELD => {
                let ep_config = bs.read_bits_leq32(2)?;

                if (ep_config == 2) || (ep_config == 3) {
                    return unsupported_error("aac: error protection config");
                }
                // if ep_config == 3 {
                //     let direct_mapping = bs.read_bit()?;
                //     validate!(direct_mapping);
                // }
            }
            _ => {}
        };

        if self.sbr_ps_info.is_some() && (bs.bits_left() >= 16) {
            let sync = bs.read_bits_leq32(11)?;

            if sync == 0x2B7 {
                let ext_otype = Self::read_object_type(&mut bs)?;
                if ext_otype == M4AType::Sbr {
                    self.sbr_present = bs.read_bool()?;
                    if self.sbr_present {
                        let _ext_srate = Self::read_sampling_frequency(&mut bs)?;
                        if bs.bits_left() >= 12 {
                            let sync = bs.read_bits_leq32(11)?;
                            if sync == 0x548 {
                                self.ps_present = bs.read_bool()?;
                            }
                        }
                    }
                }
                if ext_otype == M4AType::PS {
                    self.sbr_present = bs.read_bool()?;
                    if self.sbr_present {
                        let _ext_srate = Self::read_sampling_frequency(&mut bs)?;
                    }
                    let _ext_channels = bs.read_bits_leq32(4)?;
                }
            }
        }

        Ok(())
    }
}

impl std::fmt::Display for M4AInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MPEG 4 Audio {}, {} Hz, {} channels, {} samples per frame",
            self.otype, self.srate, self.channels, self.samples
        )
    }
}

/// Advanced Audio Coding (AAC) decoder.
///
/// Implements a decoder for Advanced Audio Decoding Low-Complexity (AAC-LC) as defined in
/// ISO/IEC 13818-7 and ISO/IEC 14496-3.
pub struct AacDecoder {
    // info: NACodecInfoRef,
    m4ainfo: M4AInfo,
    pairs: Vec<cpe::ChannelPair>,
    dsp: dsp::Dsp,
    sbinfo: GASubbandInfo,
    params: CodecParameters,
    buf: AudioBuffer<f32>,
}

impl AacDecoder {
    fn set_pair(&mut self, pair_no: usize, channel: usize, pair: bool) -> Result<()> {
        if self.pairs.len() <= pair_no {
            self.pairs.push(cpe::ChannelPair::new(pair, channel, self.sbinfo));
        }
        else {
            validate!(self.pairs[pair_no].channel == channel);
            validate!(self.pairs[pair_no].is_pair == pair);
        }
        validate!(if pair { channel + 1 } else { channel } < self.m4ainfo.channels);
        Ok(())
    }

    fn decode_ga<B: ReadBitsLtr + FiniteBitStream>(&mut self, bs: &mut B) -> Result<()> {
        let mut cur_pair = 0;
        let mut cur_ch = 0;
        while bs.bits_left() > 3 {
            let id = bs.read_bits_leq32(3)?;

            match id {
                0 => {
                    // ID_SCE
                    let _tag = bs.read_bits_leq32(4)?;
                    self.set_pair(cur_pair, cur_ch, false)?;
                    self.pairs[cur_pair].decode_ga_sce(bs, self.m4ainfo.otype)?;
                    cur_pair += 1;
                    cur_ch += 1;
                }
                1 => {
                    // ID_CPE
                    let _tag = bs.read_bits_leq32(4)?;
                    self.set_pair(cur_pair, cur_ch, true)?;
                    self.pairs[cur_pair].decode_ga_cpe(bs, self.m4ainfo.otype)?;
                    cur_pair += 1;
                    cur_ch += 2;
                }
                2 => {
                    // ID_CCE
                    return unsupported_error("aac: coupling channel element");
                }
                3 => {
                    // ID_LFE
                    let _tag = bs.read_bits_leq32(4)?;
                    self.set_pair(cur_pair, cur_ch, false)?;
                    self.pairs[cur_pair].decode_ga_sce(bs, self.m4ainfo.otype)?;
                    cur_pair += 1;
                    cur_ch += 1;
                }
                4 => {
                    // ID_DSE
                    let _id = bs.read_bits_leq32(4)?;
                    let align = bs.read_bool()?;
                    let mut count = bs.read_bits_leq32(8)?;
                    if count == 255 {
                        count += bs.read_bits_leq32(8)?;
                    }
                    if align {
                        bs.realign(); // ????
                    }
                    bs.ignore_bits(count * 8)?; // no SBR payload or such
                }
                5 => {
                    // ID_PCE
                    return unsupported_error("aac: program config");
                }
                6 => {
                    // ID_FIL
                    let mut count = bs.read_bits_leq32(4)? as usize;
                    if count == 15 {
                        count += bs.read_bits_leq32(8)? as usize;
                        count -= 1;
                    }
                    for _ in 0..count {
                        // ext payload
                        bs.ignore_bits(8)?;
                    }
                }
                7 => {
                    // ID_TERM
                    break;
                }
                _ => unreachable!(),
            };
        }
        let rate_idx = GASubbandInfo::find_idx(self.m4ainfo.srate);
        for pair in 0..cur_pair {
            self.pairs[pair].synth_audio(&mut self.dsp, &mut self.buf, rate_idx);
        }
        Ok(())
    }

    // fn flush(&mut self) {
    //     for pair in self.pairs.iter_mut() {
    //         pair.ics[0].delay = [0.0; 1024];
    //         pair.ics[1].delay = [0.0; 1024];
    //     }
    // }

    fn decode_inner(&mut self, packet: &Packet) -> Result<()> {
        // Clear the audio output buffer.
        self.buf.clear();
        self.buf.render_reserved(None);

        let mut bs = BitReaderLtr::new(packet.buf());

        // Choose decode step based on the object type.
        match self.m4ainfo.otype {
            M4AType::Lc => self.decode_ga(&mut bs)?,
            _ => return unsupported_error("aac: object type"),
        }

        Ok(())
    }
}

impl Decoder for AacDecoder {
    fn try_new(params: &CodecParameters, _: &DecoderOptions) -> Result<Self> {
        // This decoder only supports AAC.
        if params.codec != CODEC_TYPE_AAC {
            return unsupported_error("aac: invalid codec type");
        }

        let mut m4ainfo = M4AInfo::new();

        // If extra data present, parse the audio specific config
        if let Some(extra_data_buf) = &params.extra_data {
            validate!(extra_data_buf.len() >= 2);
            m4ainfo.read(extra_data_buf)?;
        }
        else {
            // Otherwise, assume there is no ASC and use the codec parameters for ADTS.
            m4ainfo.otype = M4AType::Lc;
            m4ainfo.samples = 1024;

            m4ainfo.srate = match params.sample_rate {
                Some(rate) => rate,
                None => return unsupported_error("aac: sample rate is required"),
            };

            m4ainfo.channels = if let Some(channels) = params.channels {
                channels.count()
            }
            else if let Some(layout) = params.channel_layout {
                layout.into_channels().count()
            }
            else {
                return unsupported_error("aac: channels or channel layout is required");
            };
        }

        //print!("edata:"); for s in edata.iter() { print!(" {:02X}", *s);}println!("");

        if (m4ainfo.otype != M4AType::Lc) || (m4ainfo.channels > 2) || (m4ainfo.samples != 1024) {
            return unsupported_error("aac: aac too complex");
        }

        // Openamp patch (2.5.2): channel configuration 0 (layout in a PCE) or > 2 has no map: an error, not an
        // `unwrap` panic.
        let channels = match map_channels(m4ainfo.channels as u32) {
            Some(c) => c,
            None => return unsupported_error("aac: unsupported channel configuration"),
        };
        let spec = SignalSpec::new(m4ainfo.srate, channels);

        let duration = m4ainfo.samples as Duration;
        let srate = m4ainfo.srate;

        Ok(AacDecoder {
            m4ainfo,
            pairs: Vec::new(),
            dsp: dsp::Dsp::new(),
            sbinfo: GASubbandInfo::find(srate),
            params: params.clone(),
            buf: AudioBuffer::new(duration, spec),
        })
    }

    fn reset(&mut self) {
        for pair in self.pairs.iter_mut() {
            pair.reset();
        }
    }

    fn supported_codecs() -> &'static [CodecDescriptor] {
        &[support_codec!(CODEC_TYPE_AAC, "aac", "Advanced Audio Coding")]
    }

    fn codec_params(&self) -> &CodecParameters {
        &self.params
    }

    fn decode(&mut self, packet: &Packet) -> Result<AudioBufferRef<'_>> {
        if let Err(e) = self.decode_inner(packet) {
            self.buf.clear();
            Err(e)
        }
        else {
            Ok(self.buf.as_audio_buffer_ref())
        }
    }

    fn finalize(&mut self) -> FinalizeResult {
        Default::default()
    }

    fn last_decoded(&self) -> AudioBufferRef<'_> {
        self.buf.as_audio_buffer_ref()
    }
}

// Openamp patch (2.5.2): regression tests for the external review of Openamp 2.5.1 (A1-AAC-1, A1-AAC-2).
#[cfg(test)]
mod openamp_tests {
    use super::*;

    #[derive(Default)]
    struct Bits {
        out: Vec<u8>,
        n: usize,
    }
    impl Bits {
        fn put(&mut self, v: u32, bits: u32) -> &mut Self {
            for i in (0..bits).rev() {
                if self.n % 8 == 0 {
                    self.out.push(0);
                }
                if (v >> i) & 1 != 0 {
                    *self.out.last_mut().unwrap() |= 0x80 >> (self.n % 8);
                }
                self.n += 1;
            }
            self
        }
        fn bytes(&mut self) -> Vec<u8> {
            let mut v = std::mem::take(&mut self.out);
            v.extend_from_slice(&[0; 32]);
            v
        }
    }

    /// One long-window ICS at 48 kHz with every band in the zero codebook: global gain, ics_info (max_sfb), one
    /// section of `max_sfb` bands, no pulse / TNS / gain control.
    fn long_ics(max_sfb: u32) -> Vec<u8> {
        let mut b = Bits::default();
        b.put(100, 8); // global gain
        b.put(0, 1).put(0, 2).put(0, 1).put(max_sfb, 6).put(0, 1); // reserved, ONLY_LONG, shape, max_sfb, no LTP
        b.put(0, 4); // section codebook ZERO_HCB
        let mut left = max_sfb;
        loop {
            let n = left.min(31);
            b.put(n, 5);
            left -= n;
            if n < 31 {
                break;
            }
        }
        b.put(0, 1).put(0, 1).put(0, 1); // pulse, TNS, gain control
        b.bytes()
    }

    fn decode_ics(data: &[u8]) -> Result<()> {
        let mut ics = ics::Ics::new(GASubbandInfo::find(48000));
        let mut lcg = Lcg::new(0x1f2e3d4c);
        ics.decode(&mut BitReaderLtr::new(data), &mut lcg, M4AType::Lc, false)
    }

    #[test]
    fn max_sfb_past_the_band_table_is_an_error() {
        // 48 kHz long windows: 49 bands. Up to 49 is fine; 50..=63 indexed past the table.
        assert!(decode_ics(&long_ics(49)).is_ok());
        for max_sfb in [50, 51, 63] {
            let data = long_ics(max_sfb);
            let r = std::panic::catch_unwind(|| decode_ics(&data));
            assert!(r.expect("ics decode panicked").is_err(), "max_sfb {}", max_sfb);
        }
        // Short windows: 14 bands at 48 kHz, max_sfb is 4 bits.
        let mut b = Bits::default();
        b.put(100, 8).put(0, 1).put(2, 2).put(0, 1).put(15, 4).put(0x7f, 7); // EIGHT_SHORT, max_sfb 15, one group
        let data = b.bytes();
        let r = std::panic::catch_unwind(|| decode_ics(&data));
        assert!(r.expect("ics decode panicked").is_err());
    }

    fn asc(srate_idx: u32, explicit: Option<u32>, chan_cfg: u32) -> Box<[u8]> {
        let mut b = Bits::default();
        b.put(2, 5).put(srate_idx, 4); // AAC LC
        if let Some(r) = explicit {
            b.put(r, 24);
        }
        b.put(chan_cfg, 4).put(0, 3); // channels, GASpecificConfig: 1024 frames, no core coder, no extension
        let mut v = std::mem::take(&mut b.out);
        v.resize(v.len().max(2), 0);
        v.into_boxed_slice()
    }

    fn open(extra: Box<[u8]>) -> Result<AacDecoder> {
        let mut p = CodecParameters::new();
        p.for_codec(CODEC_TYPE_AAC).with_extra_data(extra);
        AacDecoder::try_new(&p, &DecoderOptions::default())
    }

    #[test]
    fn explicit_sampling_frequency_is_read() {
        let d = open(asc(15, Some(192_000), 2)).expect("explicit 192 kHz");
        assert_eq!(d.m4ainfo.srate, 192_000);
        let d = open(asc(3, None, 2)).expect("table 48 kHz");
        assert_eq!(d.m4ainfo.srate, 48_000);
        // An explicit rate of 0 is still rejected.
        assert!(open(asc(15, Some(0), 2)).is_err());
    }

    #[test]
    fn channel_configuration_without_a_map_is_an_error_not_a_panic() {
        for cfg in [0u32, 3, 7, 15] {
            let r = std::panic::catch_unwind(|| open(asc(3, None, cfg)).map(|_| ()));
            assert!(r.expect("try_new panicked").is_err(), "channel config {}", cfg);
        }
    }
}
