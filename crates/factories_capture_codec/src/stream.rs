//! Stream-level framing: a fixed header followed by a concatenation of
//! [`Segment`]s.
//!
//! A capture stream (a file, a socket, anything) is one header, then segments
//! appended in the order a writer received them from the producer buffers.
//! Because segments are self-contained and length-prefixed, a reader skips or
//! iterates them without any cross-segment state.

use alloc::vec::Vec;

use crate::error::DecodeError;
use crate::segment::{Segment, decode_segment};

/// Magic bytes at the start of every capture stream.
pub const MAGIC: [u8; 4] = *b"FCAP";

/// The format version this codec reads and writes. Bumped on any wire change;
/// since the capture producer and its reader are versioned together, a mismatch
/// is rejected outright rather than skipped.
pub const VERSION: u16 = 1;

/// Append the 8-byte stream header (magic + version + reserved flags) to `out`.
pub fn write_stream_header(out: &mut Vec<u8>) {
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // flags, reserved
}

/// Validate the stream header at the front of `input`, returning the remaining
/// bytes (the segment body), or a [`DecodeError`] if the magic or version
/// doesn't match (or the input is too short).
pub fn read_stream_header(input: &[u8]) -> Result<&[u8], DecodeError> {
    if input.get(0..4).ok_or(DecodeError::UnexpectedEof)? != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    let version_bytes: [u8; 2] = input
        .get(4..6)
        .ok_or(DecodeError::UnexpectedEof)?
        .try_into()
        .map_err(|_| DecodeError::UnexpectedEof)?;
    let version = u16::from_le_bytes(version_bytes);
    if version != VERSION {
        return Err(DecodeError::UnsupportedVersion {
            found: version,
            expected: VERSION,
        });
    }
    let _flags = input.get(6..8).ok_or(DecodeError::UnexpectedEof)?; // reserved
    Ok(&input[8..])
}

/// Iterator over the segments in a stream body (the bytes after the header).
/// Yields each segment as a `Result`; a decode failure is surfaced as one
/// `Err` and ends iteration (the stream can't be resynced past a bad segment).
pub struct Segments<'a> {
    rest: &'a [u8],
}

/// Iterate the segments of a stream body (see [`read_stream_header`]).
pub fn segments(body: &[u8]) -> Segments<'_> {
    Segments { rest: body }
}

impl Iterator for Segments<'_> {
    type Item = Result<Segment, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        match decode_segment(self.rest) {
            Ok((segment, consumed)) => {
                self.rest = &self.rest[consumed..];
                Some(Ok(segment))
            }
            Err(error) => {
                self.rest = &[]; // fuse: nothing meaningful follows a bad segment
                Some(Err(error))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::DecodeError;
    use crate::segment::{ClockMode, Record, SegmentAnchor, SegmentEncoder};
    use core::num::NonZeroU64;

    fn nz(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("test ids are non-zero")
    }

    fn anchor(tick: u64) -> SegmentAnchor {
        SegmentAnchor {
            mode: ClockMode::Monotonic,
            unix_micros: 1_750_000_000_000_000,
            mono_micros: 0,
            tick,
        }
    }

    #[test]
    fn stream_header_round_trips_and_rejects_foreign_input() {
        let mut buf = Vec::new();
        write_stream_header(&mut buf);
        assert_eq!(buf.len(), 8);
        assert!(read_stream_header(&buf).expect("valid header").is_empty());

        assert_eq!(
            read_stream_header(b"XXXX\x01\x00\x00\x00"),
            Err(DecodeError::BadMagic)
        );

        let mut wrong_version = Vec::new();
        wrong_version.extend_from_slice(&MAGIC);
        wrong_version.extend_from_slice(&999u16.to_le_bytes());
        wrong_version.extend_from_slice(&0u16.to_le_bytes());
        assert_eq!(
            read_stream_header(&wrong_version),
            Err(DecodeError::UnsupportedVersion {
                found: 999,
                expected: VERSION,
            })
        );
    }

    #[test]
    fn stream_iterates_multiple_segments() {
        let mut buf = Vec::new();
        write_stream_header(&mut buf);

        let mut e1 = SegmentEncoder::new(anchor(1000));
        e1.spawned(1100, 1, nz(5), "Widget", None, None);
        buf.extend_from_slice(&e1.finish(1, 1200));

        let mut e2 = SegmentEncoder::new(anchor(2000));
        e2.message(2050, 1, nz(9), None, "Ping", 0, None, &[]);
        buf.extend_from_slice(&e2.finish(2, 2100));

        let body = read_stream_header(&buf).expect("header");
        let segs: Vec<Segment> = segments(body)
            .collect::<Result<_, _>>()
            .expect("all segments decode");

        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].anchor.tick, 1000);
        assert_eq!(segs[1].anchor.tick, 2000);
        assert!(matches!(segs[0].records[0], Record::Spawned { actor_id, .. } if actor_id.get() == 5));
        assert!(matches!(segs[1].records[0], Record::Message { actor_id, .. } if actor_id.get() == 9));
    }

    #[test]
    fn corrupt_segment_surfaces_as_error_not_silent_truncation() {
        let mut buf = Vec::new();
        write_stream_header(&mut buf);
        let mut good = SegmentEncoder::new(anchor(1000));
        good.spawned(1100, 1, nz(5), "Widget", None, None);
        buf.extend_from_slice(&good.finish(1, 1200));
        // A bogus segment: a length prefix claiming far more body than exists.
        buf.extend_from_slice(&[0xff, 0xff, 0xff, 0xff]);

        let body = read_stream_header(&buf).expect("header");
        let mut it = segments(body);
        assert!(matches!(it.next(), Some(Ok(_))), "the good segment decodes");
        assert_eq!(
            it.next(),
            Some(Err(DecodeError::UnexpectedEof)),
            "the bogus segment surfaces an error"
        );
        assert!(it.next().is_none(), "iterator is fused after an error");
    }
}
