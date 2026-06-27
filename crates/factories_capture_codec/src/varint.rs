//! Unsigned LEB128 varints - the integer encoding used throughout the stream.
//!
//! Every id, sequence number, length, and timestamp delta in the format is an
//! unsigned base-128 varint: 7 payload bits per byte, low group first, with the
//! high bit set on every byte except the last. Small values - the common case,
//! since these are process-local ids and sub-microsecond tick deltas - take one
//! or two bytes, and the encoding is trivial to read from JavaScript.

use alloc::vec::Vec;

use crate::error::DecodeError;

/// The most bytes an unsigned LEB128 `u64` can occupy: `ceil(64 / 7)`.
const MAX_LEN: usize = u64::BITS.div_ceil(7) as usize;

/// Append `value` to `out` as an unsigned LEB128 varint.
pub fn write_uvarint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Read an unsigned LEB128 varint from the front of `input`.
///
/// Returns the decoded value and the number of bytes consumed, or
/// [`DecodeError::UnexpectedEof`] if `input` ends mid-varint, or
/// [`DecodeError::OverlongVarint`] if it runs past the 10 bytes a `u64` needs.
pub fn read_uvarint(input: &[u8]) -> Result<(u64, usize), DecodeError> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    for (i, &byte) in input.iter().take(MAX_LEN).enumerate() {
        result |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok((result, i + 1));
        }
        shift += 7;
    }
    // No terminator: either all MAX_LEN bytes asked to continue (overlong), or
    // the input ran out before a terminator (truncated).
    if input.len() >= MAX_LEN {
        Err(DecodeError::OverlongVarint)
    } else {
        Err(DecodeError::UnexpectedEof)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn uvarint_rejects_truncated_and_overlong_input() {
        assert_eq!(read_uvarint(&[]), Err(DecodeError::UnexpectedEof), "empty input");
        assert_eq!(read_uvarint(&[0x80]), Err(DecodeError::UnexpectedEof), "continuation, no next");
        assert_eq!(read_uvarint(&[0x80, 0x80]), Err(DecodeError::UnexpectedEof), "truncated");
        assert_eq!(read_uvarint(&[0x80; 10]), Err(DecodeError::OverlongVarint), "overlong");
    }

    #[test]
    fn uvarint_round_trips_across_byte_boundaries() {
        for value in [
            0u64, 1, 127, 128, 300, 16_383, 16_384, 2_097_151, 2_097_152, u64::MAX,
        ] {
            let mut buf = Vec::new();
            write_uvarint(&mut buf, value);
            let (decoded, consumed) = read_uvarint(&buf).expect("decodes what was written");
            assert_eq!(decoded, value, "value {value} round-trips");
            assert_eq!(
                consumed,
                buf.len(),
                "reader consumes exactly the written bytes for {value}"
            );
        }
    }
}
