//! Errors returned when decoding a capture stream.

/// Why decoding a varint, record, segment, or stream failed.
///
/// `#[non_exhaustive]`: new failure modes may be added without it being a
/// breaking change, so external matches need a wildcard arm.
#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum DecodeError {
    /// The input ended in the middle of a value.
    #[error("input ended mid-value")]
    UnexpectedEof,
    /// A varint ran past the ten bytes a `u64` needs.
    #[error("varint exceeds the 10 bytes a u64 needs")]
    OverlongVarint,
    /// The stream did not begin with the expected magic bytes.
    #[error("stream magic mismatch")]
    BadMagic,
    /// The stream's format version is not the one this codec supports.
    #[error("unsupported stream version {found} (this codec speaks {expected})")]
    UnsupportedVersion {
        /// The version found in the stream.
        found: u16,
        /// The version this codec speaks.
        expected: u16,
    },
    /// A segment header carried an unrecognized clock-mode tag.
    #[error("unknown clock-mode tag {0}")]
    UnknownClockMode(u8),
    /// A record carried an unrecognized type tag.
    #[error("unknown record tag {0}")]
    UnknownRecordTag(u8),
    /// A field that must hold a real (non-zero) actor id held zero.
    #[error("a required actor id was zero")]
    ZeroActorId,
    /// A string-table entry was not valid UTF-8.
    #[error("string-table entry was not valid UTF-8")]
    InvalidUtf8,
    /// A segment's records did not exactly fill its declared length.
    #[error("segment records did not fill its declared length")]
    SegmentLengthMismatch,
}
