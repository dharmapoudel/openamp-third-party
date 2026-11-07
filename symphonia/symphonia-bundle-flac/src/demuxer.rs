// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::{Seek, SeekFrom};

use symphonia_core::support_format;

use symphonia_core::codecs::{CodecParameters, VerificationCheck, CODEC_TYPE_FLAC};
use symphonia_core::errors::{
    decode_error, seek_error, unsupported_error, Error, Result, SeekErrorKind,
};
use symphonia_core::formats::prelude::*;
use symphonia_core::formats::util::{SeekIndex, SeekSearchResult};
use symphonia_core::io::*;
use symphonia_core::meta::{Metadata, MetadataBuilder, MetadataLog};
use symphonia_core::probe::{Descriptor, Instantiate, QueryDescriptor};

use symphonia_utils_xiph::flac::metadata::*;

use log::{debug, info};

use super::parser::PacketParser;

/// The FLAC start of stream marker: "fLaC" in ASCII.
const FLAC_STREAM_MARKER: [u8; 4] = *b"fLaC";

/// Free Lossless Audio Codec (FLAC) native frame reader.
pub struct FlacReader {
    reader: MediaSourceStream,
    metadata: MetadataLog,
    tracks: Vec<Track>,
    cues: Vec<Cue>,
    index: Option<SeekIndex>,
    first_frame_offset: u64,
    parser: PacketParser,
}

impl FlacReader {
    /// Reads all the metadata blocks, returning a fully populated `FlacReader`.
    fn init_with_metadata(source: MediaSourceStream) -> Result<Self> {
        let mut metadata_builder = MetadataBuilder::new();

        let mut reader = source;
        let mut tracks = Vec::new();
        let mut cues = Vec::new();
        let mut index = None;
        let mut parser = Default::default();

        loop {
            let header = MetadataBlockHeader::read(&mut reader)?;

            // Create a scoped bytestream to error if the metadata block read functions exceed the
            // stated length of the block.
            let mut block_stream = ScopedStream::new(&mut reader, u64::from(header.block_len));

            match header.block_type {
                MetadataBlockType::Application => {
                    // TODO: Store vendor data.
                    read_application_block(&mut block_stream, header.block_len)?;
                }
                // SeekTable blocks are parsed into a SeekIndex.
                MetadataBlockType::SeekTable => {
                    // Check if a SeekTable has already be parsed. If one has, then the file is
                    // invalid, atleast for seeking. Either way, it's a violation of the
                    // specification.
                    if index.is_none() {
                        let mut new_index = SeekIndex::new();
                        read_seek_table_block(&mut block_stream, header.block_len, &mut new_index)?;
                        index = Some(new_index);
                    }
                    else {
                        return decode_error("flac: found more than one seek table block");
                    }
                }
                // VorbisComment blocks are parsed into Tags.
                MetadataBlockType::VorbisComment => {
                    read_comment_block(&mut block_stream, &mut metadata_builder)?;
                }
                // Cuesheet blocks are parsed into Cues.
                MetadataBlockType::Cuesheet => {
                    read_cuesheet_block(&mut block_stream, &mut cues)?;
                }
                // Picture blocks are read as Visuals.
                MetadataBlockType::Picture => {
                    read_picture_block(&mut block_stream, &mut metadata_builder)?;
                }
                // StreamInfo blocks are parsed into Streams.
                MetadataBlockType::StreamInfo => {
                    read_stream_info_block(&mut block_stream, &mut tracks, &mut parser)?;
                }
                // Padding blocks are skipped.
                MetadataBlockType::Padding => {
                    block_stream.ignore_bytes(u64::from(header.block_len))?;
                }
                // Unknown block encountered. Skip these blocks as they may be part of a future
                // version of FLAC, but  print a message.
                MetadataBlockType::Unknown(id) => {
                    block_stream.ignore_bytes(u64::from(header.block_len))?;
                    info!("ignoring {} bytes of block width id={}.", header.block_len, id);
                }
            }

            // If the stated block length is longer than the number of bytes from the block read,
            // ignore the remaining unread data.
            let block_unread_len = block_stream.bytes_available();

            if block_unread_len > 0 {
                info!("under read block by {} bytes.", block_unread_len);
                block_stream.ignore_bytes(block_unread_len)?;
            }

            // Exit when the last header is read.
            if header.is_last {
                break;
            }
        }

        // Commit any read metadata to the metadata log.
        let mut metadata = MetadataLog::default();
        metadata.push(metadata_builder.metadata());

        // Synchronize the packet parser to the first audio frame.
        let _ = parser.resync(&mut reader)?;

        // The first frame offset is the byte offset from the beginning of the stream after all the
        // metadata blocks have been read.
        let first_frame_offset = reader.pos();

        Ok(FlacReader { reader, metadata, tracks, cues, index, first_frame_offset, parser })
    }
}

impl QueryDescriptor for FlacReader {
    fn query() -> &'static [Descriptor] {
        &[support_format!(
            "flac",
            "Free Lossless Audio Codec Native",
            &["flac"],
            &["audio/flac"],
            &[b"fLaC"]
        )]
    }

    fn score(_context: &[u8]) -> u8 {
        255
    }
}

impl FormatReader for FlacReader {
    fn try_new(mut source: MediaSourceStream, _options: &FormatOptions) -> Result<Self> {
        // Read the first 4 bytes of the stream. Ideally this will be the FLAC stream marker.
        let marker = source.read_quad_bytes()?;

        if marker != FLAC_STREAM_MARKER {
            return unsupported_error("flac: missing flac stream marker");
        }

        // Strictly speaking, the first metadata block must be a StreamInfo block. There is
        // no technical need for this from the reader's point of view. Additionally, if the
        // reader is fed a stream mid-way there is no StreamInfo block. Therefore, just read
        // all metadata blocks and handle the StreamInfo block as it comes.
        let flac = Self::init_with_metadata(source)?;

        // Make sure that there is atleast one StreamInfo block.
        if flac.tracks.is_empty() {
            return decode_error("flac: no stream info block");
        }

        Ok(flac)
    }

    fn next_packet(&mut self) -> Result<Packet> {
        self.parser.parse(&mut self.reader)
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn cues(&self) -> &[Cue] {
        &self.cues
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn seek(&mut self, _mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        if self.tracks.is_empty() {
            return seek_error(SeekErrorKind::Unseekable);
        }

        let params = &self.tracks[0].codec_params;

        // Get the timestamp of the desired audio frame.
        let ts = match to {
            // Frame timestamp given.
            SeekTo::TimeStamp { ts, .. } => ts,
            // Time value given, calculate frame timestamp from sample rate.
            SeekTo::Time { time, .. } => {
                // Use the sample rate to calculate the frame timestamp. If sample rate is not
                // known, the seek cannot be completed.
                if let Some(sample_rate) = params.sample_rate {
                    TimeBase::new(1, sample_rate).calc_timestamp(time)
                }
                else {
                    return seek_error(SeekErrorKind::Unseekable);
                }
            }
        };

        debug!("seeking to frame_ts={}", ts);

        // If the total number of frames in the stream is known, verify the desired frame timestamp
        // does not exceed it.
        if let Some(n_frames) = params.n_frames {
            if ts > n_frames {
                return seek_error(SeekErrorKind::OutOfRange);
            }
        }

        // If the reader supports seeking, coarsely seek to the nearest packet with a timestamp
        // lower than the desired timestamp using a binary search.
        if self.reader.is_seekable() {
            // The range formed by start_byte_offset..end_byte_offset defines an area where the
            // binary search for the packet containing the desired timestamp will be performed. The
            // lower bound is set to the byte offset of the first frame, while the upper bound is
            // set to the length of the stream.
            let mut start_byte_offset = self.first_frame_offset;
            let mut end_byte_offset =
                self.reader.byte_len().ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;

            // Openamp patch (2.5.3): the timestamps at the ends of the search range, when known (stream start =
            // 0, stream end = n_frames, or the SEEKTABLE points around the target), for the interpolation search below.
            let mut lo_ts: Option<u64> = Some(0);
            let mut hi_ts: Option<u64> = params.n_frames;

            // If there is an index, use it to refine the binary search range.
            if let Some(ref index) = self.index {
                // Search the index for the timestamp. Adjust the search based on the result.
                match index.search(ts) {
                    // Search from the start of stream up-to an ending point.
                    SeekSearchResult::Upper(upper) => {
                        end_byte_offset = self.first_frame_offset.saturating_add(upper.byte_offset);
                        hi_ts = Some(upper.frame_ts);
                    }
                    // Search from a starting point up-to the end of the stream.
                    SeekSearchResult::Lower(lower) => {
                        start_byte_offset = self.first_frame_offset.saturating_add(lower.byte_offset);
                        lo_ts = Some(lower.frame_ts);
                    }
                    // Search between two points of the stream.
                    SeekSearchResult::Range(lower, upper) => {
                        start_byte_offset = self.first_frame_offset.saturating_add(lower.byte_offset);
                        end_byte_offset = self.first_frame_offset.saturating_add(upper.byte_offset);
                        lo_ts = Some(lower.frame_ts);
                        hi_ts = Some(upper.frame_ts);
                    }
                    // Search the entire stream (default behaviour, so do nothing).
                    SeekSearchResult::Stream => (),
                }
            }

            // Openamp patch (2.5.1): SEEKTABLE offsets come from the file; keep the range inside the stream and
            // ordered, and use checked math (an astronomic offset made start > end: a subtract overflow).
            let stream_len = self.reader.byte_len().unwrap_or(u64::MAX);
            end_byte_offset = end_byte_offset.min(stream_len);
            start_byte_offset = start_byte_offset.min(end_byte_offset);

            // Openamp patch (2.5.3): a safeguarded interpolation search instead of a plain binary search. Every
            // probe is a jump in the file; on a network source (a server song read through byte ranges) each jump
            // is a new HTTP request, and a binary search over a 100 MB FLAC without a SEEKTABLE made ~13 of them
            // (device log: a resume 16 s into a 192 kHz FLAC stayed silent for 25 s). FLAC bitrate is nearly
            // constant, so the byte offset interpolated from the timestamps at both ends of the range (aimed half
            // a block before the target's block) usually lands in the right frame on the first or second probe.
            // Safeguard: once an interpolated probe shrinks the range by less than half (bitrate far from even, e.g.
            // silence next to dense music), the rest of the search bisects, so the worst case is the binary search
            // plus that one probe. The result (the frame containing `ts`, or
            // the linear search below) is the same as before; only the number of probes changes.
            let (block_min, block_max) = self.parser.block_lens();
            let block = u64::from(block_max).max(1);
            let fixed_block = if block_min == block_max { Some(u64::from(block_max)) } else { None };
            let mut interpolate = true;
            while end_byte_offset.saturating_sub(start_byte_offset) > 2 * 8096 {
                let span = end_byte_offset - start_byte_offset;
                let interp = match (lo_ts, hi_ts) {
                    (Some(a), Some(b)) if interpolate && b > a && ts >= a && ts < b => {
                        let aligned = match fixed_block {
                            Some(n) if n > 0 => ts - ts % n,
                            _ => ts,
                        };
                        let aim = aligned.saturating_sub(block / 2).max(a);
                        let frac = (aim - a) as f64 / (b - a) as f64;
                        let off = (frac * span as f64) as u64;
                        // Keep the probe strictly inside the range.
                        Some(start_byte_offset + off.clamp(1, span - 1))
                    }
                    _ => None,
                };
                let mid_byte_offset = interp.unwrap_or(start_byte_offset + span / 2);
                self.reader.seek(SeekFrom::Start(mid_byte_offset))?;

                let sync = self.parser.resync(&mut self.reader)?;
                let frame_pos = self.reader.pos();

                if ts < sync.ts {
                    end_byte_offset = mid_byte_offset;
                    hi_ts = Some(sync.ts);
                }
                else if ts >= sync.ts && ts < sync.ts.saturating_add(sync.dur) {
                    debug!("seeked to ts={} (delta={})", sync.ts, sync.ts as i64 - ts as i64);

                    return Ok(SeekedTo { track_id: 0, actual_ts: sync.ts, required_ts: ts });
                }
                else {
                    // The frame containing `ts` starts after this frame's start.
                    start_byte_offset = frame_pos.max(mid_byte_offset).min(end_byte_offset);
                    lo_ts = Some(sync.ts);
                }
                let left = end_byte_offset.saturating_sub(start_byte_offset);
                if interp.is_some() && left > span / 2 {
                    interpolate = false;
                }
            }

            // The binary search did not find an exact frame, but the range has been narrowed. Seek
            // to the start of the range, and continue with a linear search.
            self.reader.seek(SeekFrom::Start(start_byte_offset))?;
        }

        // Linearly search the stream packet-by-packet for the packet that contains the desired
        // timestamp. This search is used to find the exact packet containing the desired timestamp
        // after the search range was narrowed by the binary search. It is also the ONLY way for a
        // unseekable stream to be "seeked" forward.
        let packet = loop {
            let sync = self.parser.resync(&mut self.reader)?;

            // The desired timestamp precedes the current packet's timestamp.
            if ts < sync.ts {
                // Attempted to seek backwards on an unseekable stream.
                if !self.reader.is_seekable() {
                    return seek_error(SeekErrorKind::ForwardOnly);
                }
                // Overshot a regular seek, or the stream is corrupted, not necessarily an error
                // per-say.
                else {
                    break sync;
                }
            }
            // The desired timestamp is contained within the current packet.
            else if ts >= sync.ts && ts < sync.ts.saturating_add(sync.dur) {
                break sync;
            }

            // Advance the reader such that the next iteration will sync to a different frame.
            self.reader.read_byte()?;
        };

        debug!("seeked to packet_ts={} (delta={})", packet.ts, packet.ts as i64 - ts as i64);

        Ok(SeekedTo { track_id: 0, actual_ts: packet.ts, required_ts: ts })
    }

    fn into_inner(self: Box<Self>) -> MediaSourceStream {
        self.reader
    }
}

/// Reads a StreamInfo block and populates the reader with stream information.
fn read_stream_info_block<B: ReadBytes + FiniteStream>(
    reader: &mut B,
    tracks: &mut Vec<Track>,
    parser: &mut PacketParser,
) -> Result<()> {
    // Only one StreamInfo block, and therefore only one Track, is allowed per media source stream.
    if tracks.is_empty() {
        // Ensure the block length is correct for a stream information block before allocating a
        // buffer for it.
        if !StreamInfo::is_valid_size(reader.byte_len()) {
            return decode_error("flac: invalid stream info block size");
        }

        // Read the stream information block as a boxed slice so that it may be attached as extra
        // data on the codec parameters.
        let extra_data = reader.read_boxed_slice_exact(reader.byte_len() as usize)?;

        // Parse the stream info block.
        let info = StreamInfo::read(&mut BufReader::new(&extra_data))?;

        // Populate the codec parameters with the basic audio parameters of the track.
        let mut codec_params = CodecParameters::new();

        codec_params
            .for_codec(CODEC_TYPE_FLAC)
            .with_packet_data_integrity(true)
            .with_extra_data(extra_data)
            .with_sample_rate(info.sample_rate)
            .with_time_base(TimeBase::new(1, info.sample_rate))
            .with_bits_per_sample(info.bits_per_sample)
            .with_channels(info.channels);

        if let Some(md5) = info.md5 {
            codec_params.with_verification_code(VerificationCheck::Md5(md5));
        }

        // Total samples per channel (the total number of frames) is optional.
        if let Some(n_frames) = info.n_samples {
            codec_params.with_n_frames(n_frames);
        }

        // Reset the packet parser.
        parser.reset(info);

        // Add the track.
        tracks.push(Track::new(0, codec_params));
    }
    else {
        return decode_error("flac: found more than one stream info block");
    }

    Ok(())
}
