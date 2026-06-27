//! One self-contained *segment* of the capture stream.
//!
//! A segment is the unit a single producer buffer flushes: a header carrying
//! the timestamp anchor and clock identity, its own string table (type names,
//! deduplicated and referenced by index), and a run of records. Because it
//! carries its own anchor and string table, a segment decodes standalone - the
//! stream layer is just a header followed by a concatenation of these.
//!
//! Layout (all integers are unsigned LEB128 varints unless noted):
//!
//! ```text
//! seg_len : u32 little-endian   -- length of everything after this field
//! ── header ──
//! mode    : u8                  -- ClockMode tag (informational)
//! open_unix, open_mono, open_tick, close_mono, close_tick : varint
//! ── string table ──
//! count   : varint
//!   (len : varint, utf8 bytes) * count
//! ── records ──
//! record*                       -- until seg_len is consumed
//! ```

use alloc::string::String;
use alloc::vec::Vec;
use core::num::NonZeroU64;

use crate::error::DecodeError;
use crate::varint::{read_uvarint, write_uvarint};

/// Record type tags - the first byte of every record. The high range is
/// reserved for future record types (we are the only consumer, so the stream
/// `version` gates additions rather than a per-record skip mechanism).
const TAG_SPAWNED: u8 = 0;
const TAG_DIED: u8 = 1;
const TAG_MESSAGE: u8 = 2;

/// Which clock produced a segment's tick values. Recorded in every segment so a
/// capture decodes correctly regardless of the machine that wrote it.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ClockMode {
    /// x86-64 invariant TSC via `rdtsc`.
    Tsc,
    /// AArch64 generic timer via `cntvct_el0`.
    Cntvct,
    /// Portable `CLOCK_MONOTONIC`; ticks are nanoseconds.
    Monotonic,
    /// No per-event clock available; the segment's anchor is the only timestamp.
    SegmentStamp,
}

impl ClockMode {
    /// The wire tag for this mode.
    pub fn to_tag(self) -> u8 {
        match self {
            ClockMode::Tsc => 0,
            ClockMode::Cntvct => 1,
            ClockMode::Monotonic => 2,
            ClockMode::SegmentStamp => 3,
        }
    }

    /// Recover a mode from its wire tag, or `None` if the tag is unknown.
    pub fn from_tag(tag: u8) -> Option<Self> {
        Some(match tag {
            0 => ClockMode::Tsc,
            1 => ClockMode::Cntvct,
            2 => ClockMode::Monotonic,
            3 => ClockMode::SegmentStamp,
            _ => return None,
        })
    }
}

/// The clock readings captured when a segment opened. Combined with the close
/// readings stored on the decoded [`Segment`], event ticks map to wall-clock by
/// interpolation - no global clock-frequency calibration is needed, because the
/// segment's own open/close `mono` readings give its real elapsed duration.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct SegmentAnchor {
    /// Which clock produced the tick values (informational; reconstruction is
    /// frequency-agnostic).
    pub mode: ClockMode,
    /// Wall-clock microseconds since the Unix epoch at open (labels the absolute axis).
    pub unix_micros: u64,
    /// Monotonic microseconds at open (a jump-free relative axis).
    pub mono_micros: u64,
    /// The clock's tick value at open; record deltas fold onto this.
    pub tick: u64,
}

/// A reference to another event - a `caused_by` link or a child's causing event.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct EventRef {
    /// The actor that emitted the referenced event (never zero).
    pub actor: NonZeroU64,
    /// That actor's sequence number for the referenced event.
    pub seq: u64,
}

/// A decoded record. Type names are indices into the owning [`Segment`]'s
/// `strings` table. Actor ids are [`NonZeroU64`] — real actor ids are never zero
/// — so an absent `parent`/`from`/`caused_by` is just `None`, the value the
/// wire's `0` decodes to.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Record {
    /// An actor was born.
    Spawned {
        ts_delta: u64,
        ev_seq: u64,
        actor_id: NonZeroU64,
        actor_type: u32,
        parent: Option<NonZeroU64>,
        caused_by: Option<EventRef>,
    },
    /// An actor terminated.
    Died {
        ts_delta: u64,
        ev_seq: u64,
        actor_id: NonZeroU64,
        actor_type: u32,
        reason: u8,
        caused_by: Option<EventRef>,
    },
    /// A message was delivered to an actor.
    Message {
        ts_delta: u64,
        ev_seq: u64,
        actor_id: NonZeroU64,
        from: Option<NonZeroU64>,
        message_type: u32,
        dispatch: u8,
        caused_by: Option<EventRef>,
        payload: Vec<u8>,
    },
}

impl Record {
    /// The tick delta from the previous record in the same segment.
    pub fn ts_delta(&self) -> u64 {
        match self {
            Record::Spawned { ts_delta, .. }
            | Record::Died { ts_delta, .. }
            | Record::Message { ts_delta, .. } => *ts_delta,
        }
    }
}

/// A decoded segment: its open/close clock readings, string table, and records.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Segment {
    /// The clock readings captured at open.
    pub anchor: SegmentAnchor,
    /// Monotonic microseconds at close - the far end of the interpolation bracket.
    pub close_mono_micros: u64,
    /// The clock's tick value at close.
    pub close_tick: u64,
    /// Deduplicated type names, referenced by index from records.
    pub strings: Vec<String>,
    /// Interned interpretation blobs (payload field hints), referenced by id.
    pub interpretations: Vec<Vec<u8>>,
    /// Interned schema (type-tree) blobs, referenced by a payload's `schema_id`.
    pub schemas: Vec<Vec<u8>>,
    /// The records, in the order they were emitted.
    pub records: Vec<Record>,
}

impl Segment {
    /// Reconstruct each record's wall-clock time, in microseconds since the Unix
    /// epoch, by interpolating its absolute tick within the open/close bracket.
    ///
    /// Frequency-agnostic: the scale is the segment's measured `mono` span over
    /// its tick span, never a calibrated rate. If the tick span is zero (a single
    /// event, or a segment closed instantly) every event maps to the open time.
    pub fn event_unix_micros(&self) -> Vec<u64> {
        let open_tick = self.anchor.tick;
        let tick_span = self.close_tick.saturating_sub(open_tick);
        let mono_span = self.close_mono_micros.saturating_sub(self.anchor.mono_micros);

        let mut abs_tick = open_tick;
        let mut times = Vec::with_capacity(self.records.len());
        for record in &self.records {
            abs_tick = abs_tick.saturating_add(record.ts_delta());
            let elapsed_micros = if tick_span == 0 {
                0
            } else {
                let into_segment = u128::from(abs_tick.saturating_sub(open_tick));
                // u128 intermediate: `into_segment * mono_span` overflows u64 for
                // long-open segments otherwise.
                ((into_segment * u128::from(mono_span)) / u128::from(tick_span)) as u64
            };
            times.push(self.anchor.unix_micros + elapsed_micros);
        }
        times
    }
}

/// Builds one segment's bytes. The caller feeds it absolute tick values; the
/// encoder stores per-record deltas. Type names are interned on the fly.
pub struct SegmentEncoder {
    anchor: SegmentAnchor,
    last_tick: u64,
    strings: Vec<String>,
    interpretations: Vec<Vec<u8>>,
    schemas: Vec<Vec<u8>>,
    records: Vec<u8>,
}

impl SegmentEncoder {
    /// Start a segment with the given anchor.
    pub fn new(anchor: SegmentAnchor) -> Self {
        Self {
            anchor,
            last_tick: anchor.tick,
            strings: Vec::new(),
            interpretations: Vec::new(),
            schemas: Vec::new(),
            records: Vec::new(),
        }
    }

    /// Intern an interpretation blob, returning its table index (deduplicated).
    pub fn intern_interpretation(&mut self, blob: &[u8]) -> u32 {
        intern_blob(&mut self.interpretations, blob)
    }

    /// Intern a schema (type-tree) blob, returning its `schema_id` (deduplicated).
    pub fn intern_schema(&mut self, blob: &[u8]) -> u32 {
        intern_blob(&mut self.schemas, blob)
    }

    /// Bytes of records accumulated so far (excludes the header and string
    /// table) - a cheap proxy for deciding when a segment is large enough to flush.
    pub fn encoded_len(&self) -> usize {
        self.records.len()
    }

    /// Finish the segment with the clock readings taken as it closed, returning
    /// its complete bytes (length-prefixed). The open/close pair brackets every
    /// record, so their ticks interpolate to wall-clock without any calibration.
    pub fn finish(self, close_mono_micros: u64, close_tick: u64) -> Vec<u8> {
        let mut body = Vec::new();
        body.push(self.anchor.mode.to_tag());
        write_uvarint(&mut body, self.anchor.unix_micros);
        write_uvarint(&mut body, self.anchor.mono_micros);
        write_uvarint(&mut body, self.anchor.tick);
        write_uvarint(&mut body, close_mono_micros);
        write_uvarint(&mut body, close_tick);

        write_uvarint(&mut body, self.strings.len() as u64);
        for s in &self.strings {
            write_uvarint(&mut body, s.len() as u64);
            body.extend_from_slice(s.as_bytes());
        }

        write_blob_table(&mut body, &self.interpretations);
        write_blob_table(&mut body, &self.schemas);

        body.extend_from_slice(&self.records);

        let mut out = Vec::with_capacity(4 + body.len());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// Intern a type name, returning its index in the segment's string table.
    fn intern(&mut self, name: &str) -> u32 {
        if let Some(i) = self.strings.iter().position(|existing| existing == name) {
            return i as u32;
        }
        let i = self.strings.len() as u32;
        self.strings.push(String::from(name));
        i
    }

    /// Consume an absolute tick, returning its delta from the previous record
    /// and advancing the running tick. Saturating, because a near-synced core
    /// migration can momentarily yield `tick < last` - harmless for a
    /// display-only timeline, and the decoder tolerates a zero delta.
    fn delta(&mut self, tick: u64) -> u64 {
        let delta = tick.saturating_sub(self.last_tick);
        self.last_tick = tick;
        delta
    }

    /// Record an actor spawn.
    pub fn spawned(
        &mut self,
        tick: u64,
        ev_seq: u64,
        actor_id: NonZeroU64,
        actor_type: &str,
        parent: Option<NonZeroU64>,
        caused_by: Option<EventRef>,
    ) {
        let delta = self.delta(tick);
        let type_idx = self.intern(actor_type);
        let out = &mut self.records;
        out.push(TAG_SPAWNED);
        write_uvarint(out, delta);
        write_uvarint(out, ev_seq);
        write_uvarint(out, actor_id.get());
        write_uvarint(out, u64::from(type_idx));
        write_uvarint(out, parent.map_or(0, NonZeroU64::get));
        write_cause(out, caused_by);
    }

    /// Record an actor death.
    pub fn died(
        &mut self,
        tick: u64,
        ev_seq: u64,
        actor_id: NonZeroU64,
        actor_type: &str,
        reason: u8,
        caused_by: Option<EventRef>,
    ) {
        let delta = self.delta(tick);
        let type_idx = self.intern(actor_type);
        let out = &mut self.records;
        out.push(TAG_DIED);
        write_uvarint(out, delta);
        write_uvarint(out, ev_seq);
        write_uvarint(out, actor_id.get());
        write_uvarint(out, u64::from(type_idx));
        out.push(reason);
        write_cause(out, caused_by);
    }

    /// Record a delivered message; `payload` is empty in metadata-only mode.
    // The parameters are exactly the wire fields of a message record; grouping
    // them into a struct would only move the same fields behind a name.
    #[allow(clippy::too_many_arguments)]
    pub fn message(
        &mut self,
        tick: u64,
        ev_seq: u64,
        actor_id: NonZeroU64,
        from: Option<NonZeroU64>,
        message_type: &str,
        dispatch: u8,
        caused_by: Option<EventRef>,
        payload: &[u8],
    ) {
        let delta = self.delta(tick);
        let type_idx = self.intern(message_type);
        let out = &mut self.records;
        out.push(TAG_MESSAGE);
        write_uvarint(out, delta);
        write_uvarint(out, ev_seq);
        write_uvarint(out, actor_id.get());
        write_uvarint(out, from.map_or(0, NonZeroU64::get));
        write_uvarint(out, u64::from(type_idx));
        out.push(dispatch);
        write_cause(out, caused_by);
        write_uvarint(out, payload.len() as u64);
        out.extend_from_slice(payload);
    }
}

/// Decode one segment from the front of `input`, returning it and the number of
/// bytes consumed (including the 4-byte length prefix), or `None` if malformed.
pub fn decode_segment(input: &[u8]) -> Result<(Segment, usize), DecodeError> {
    let len_bytes: [u8; 4] = input
        .get(0..4)
        .ok_or(DecodeError::UnexpectedEof)?
        .try_into()
        .map_err(|_| DecodeError::UnexpectedEof)?;
    let total = 4 + u32::from_le_bytes(len_bytes) as usize;
    let body = input.get(4..total).ok_or(DecodeError::UnexpectedEof)?;

    let mut pos = 0usize;
    let tag = *body.get(pos).ok_or(DecodeError::UnexpectedEof)?;
    let mode = ClockMode::from_tag(tag).ok_or(DecodeError::UnknownClockMode(tag))?;
    pos += 1;
    let (unix_micros, n) = read_uvarint(&body[pos..])?;
    pos += n;
    let (mono_micros, n) = read_uvarint(&body[pos..])?;
    pos += n;
    let (tick, n) = read_uvarint(&body[pos..])?;
    pos += n;
    let (close_mono_micros, n) = read_uvarint(&body[pos..])?;
    pos += n;
    let (close_tick, n) = read_uvarint(&body[pos..])?;
    pos += n;

    let (count, n) = read_uvarint(&body[pos..])?;
    pos += n;
    // Grow as strings are read rather than pre-allocating `count` (which is
    // attacker-controlled and could request a huge allocation).
    let mut strings = Vec::new();
    for _ in 0..count {
        let (len, n) = read_uvarint(&body[pos..])?;
        pos += n;
        let bytes = body
            .get(pos..pos + len as usize)
            .ok_or(DecodeError::UnexpectedEof)?;
        pos += len as usize;
        strings.push(String::from(
            core::str::from_utf8(bytes).map_err(|_| DecodeError::InvalidUtf8)?,
        ));
    }

    let (interpretations, next) = read_blob_table(body, pos)?;
    pos = next;
    let (schemas, next) = read_blob_table(body, pos)?;
    pos = next;

    let mut records = Vec::new();
    while pos < body.len() {
        let (record, n) = decode_record(&body[pos..])?;
        pos += n;
        records.push(record);
    }
    // The records must tile the body exactly; a leftover tail means corruption.
    if pos != body.len() {
        return Err(DecodeError::SegmentLengthMismatch);
    }

    Ok((
        Segment {
            anchor: SegmentAnchor {
                mode,
                unix_micros,
                mono_micros,
                tick,
            },
            close_mono_micros,
            close_tick,
            strings,
            interpretations,
            schemas,
            records,
        },
        total,
    ))
}

/// Decode one record from the front of `input` (no length prefix; records tile
/// the segment body and are self-delimiting via their tag and field widths).
fn decode_record(input: &[u8]) -> Result<(Record, usize), DecodeError> {
    let tag = *input.first().ok_or(DecodeError::UnexpectedEof)?;
    let mut pos = 1usize;
    let (ts_delta, n) = read_uvarint(&input[pos..])?;
    pos += n;
    let (ev_seq, n) = read_uvarint(&input[pos..])?;
    pos += n;
    let (actor_id_raw, n) = read_uvarint(&input[pos..])?;
    pos += n;
    // A record's own actor is always a real (non-zero) id; a zero here is corruption.
    let actor_id = NonZeroU64::new(actor_id_raw).ok_or(DecodeError::ZeroActorId)?;

    let record = match tag {
        TAG_SPAWNED => {
            let (type_idx, n) = read_uvarint(&input[pos..])?;
            pos += n;
            let (parent_raw, n) = read_uvarint(&input[pos..])?;
            pos += n;
            let (caused_by, n) = read_cause(&input[pos..])?;
            pos += n;
            Record::Spawned {
                ts_delta,
                ev_seq,
                actor_id,
                actor_type: type_idx as u32,
                parent: NonZeroU64::new(parent_raw),
                caused_by,
            }
        }
        TAG_DIED => {
            let (type_idx, n) = read_uvarint(&input[pos..])?;
            pos += n;
            let reason = *input.get(pos).ok_or(DecodeError::UnexpectedEof)?;
            pos += 1;
            let (caused_by, n) = read_cause(&input[pos..])?;
            pos += n;
            Record::Died {
                ts_delta,
                ev_seq,
                actor_id,
                actor_type: type_idx as u32,
                reason,
                caused_by,
            }
        }
        TAG_MESSAGE => {
            let (from_raw, n) = read_uvarint(&input[pos..])?;
            pos += n;
            let (type_idx, n) = read_uvarint(&input[pos..])?;
            pos += n;
            let dispatch = *input.get(pos).ok_or(DecodeError::UnexpectedEof)?;
            pos += 1;
            let (caused_by, n) = read_cause(&input[pos..])?;
            pos += n;
            let (payload_len, n) = read_uvarint(&input[pos..])?;
            pos += n;
            let payload = input
                .get(pos..pos + payload_len as usize)
                .ok_or(DecodeError::UnexpectedEof)?
                .to_vec();
            pos += payload_len as usize;
            Record::Message {
                ts_delta,
                ev_seq,
                actor_id,
                from: NonZeroU64::new(from_raw),
                message_type: type_idx as u32,
                dispatch,
                caused_by,
                payload,
            }
        }
        _ => return Err(DecodeError::UnknownRecordTag(tag)),
    };
    Ok((record, pos))
}

/// Encode an optional `caused_by`: the actor id (`0` = absent), followed by the
/// sequence only when present. Relies on real actor ids being non-zero.
fn write_cause(out: &mut Vec<u8>, caused_by: Option<EventRef>) {
    match caused_by {
        Some(EventRef { actor, seq }) => {
            write_uvarint(out, actor.get());
            write_uvarint(out, seq);
        }
        None => write_uvarint(out, 0),
    }
}

/// Decode an optional `caused_by` written by [`write_cause`]: a `0` actor id is
/// "no cause", any other value carries a following sequence.
fn read_cause(input: &[u8]) -> Result<(Option<EventRef>, usize), DecodeError> {
    let (actor_raw, n) = read_uvarint(input)?;
    match NonZeroU64::new(actor_raw) {
        None => Ok((None, n)),
        Some(actor) => {
            let (seq, m) = read_uvarint(&input[n..])?;
            Ok((Some(EventRef { actor, seq }), n + m))
        }
    }
}

/// Intern `blob` into `table` (linear dedup), returning its index.
fn intern_blob(table: &mut Vec<Vec<u8>>, blob: &[u8]) -> u32 {
    if let Some(index) = table.iter().position(|existing| existing == blob) {
        return index as u32;
    }
    let index = table.len() as u32;
    table.push(blob.to_vec());
    index
}

/// Write a length-prefixed table of length-delimited blobs.
fn write_blob_table(body: &mut Vec<u8>, table: &[Vec<u8>]) {
    write_uvarint(body, table.len() as u64);
    for blob in table {
        write_uvarint(body, blob.len() as u64);
        body.extend_from_slice(blob);
    }
}

/// Read a blob table written by [`write_blob_table`], returning it and the new
/// position.
fn read_blob_table(body: &[u8], mut pos: usize) -> Result<(Vec<Vec<u8>>, usize), DecodeError> {
    let (count, n) = read_uvarint(&body[pos..])?;
    pos += n;
    let mut table = Vec::new();
    for _ in 0..count {
        let (len, n) = read_uvarint(&body[pos..])?;
        pos += n;
        let blob = body
            .get(pos..pos + len as usize)
            .ok_or(DecodeError::UnexpectedEof)?
            .to_vec();
        pos += len as usize;
        table.push(blob);
    }
    Ok((table, pos))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn sample_anchor() -> SegmentAnchor {
        SegmentAnchor {
            mode: ClockMode::Tsc,
            unix_micros: 1_750_000_000_000_000,
            mono_micros: 42_000_000,
            tick: 9_000_000,
        }
    }

    fn nz(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("test ids are non-zero")
    }

    #[test]
    fn interpretation_and_schema_tables_round_trip_and_dedup() {
        let a = sample_anchor();
        let mut enc = SegmentEncoder::new(a);
        assert_eq!(enc.intern_interpretation(b"audio"), 0);
        assert_eq!(enc.intern_interpretation(b"image"), 1);
        assert_eq!(enc.intern_interpretation(b"audio"), 0, "interpretations dedup");
        assert_eq!(enc.intern_schema(b"\x05\x01"), 0);
        assert_eq!(enc.intern_schema(b"\x05\x01"), 0, "schemas dedup");

        let bytes = enc.finish(a.mono_micros + 1000, a.tick + 1000);
        let (seg, _) = decode_segment(&bytes).expect("decodes");

        assert_eq!(seg.interpretations, vec![b"audio".to_vec(), b"image".to_vec()]);
        assert_eq!(seg.schemas, vec![b"\x05\x01".to_vec()]);
    }

    #[test]
    fn empty_segment_round_trips_its_anchor() {
        let bytes = SegmentEncoder::new(sample_anchor()).finish(43_000_000, 9_500_000);
        let (segment, consumed) = decode_segment(&bytes).expect("decodes");
        assert_eq!(consumed, bytes.len(), "consumes exactly the whole segment");
        assert_eq!(segment.anchor, sample_anchor());
        assert_eq!(segment.close_mono_micros, 43_000_000);
        assert_eq!(segment.close_tick, 9_500_000);
        assert!(segment.strings.is_empty(), "no strings");
        assert!(segment.records.is_empty(), "no records");
    }

    #[test]
    fn spawned_records_round_trip_links_and_intern_types() {
        let a = sample_anchor();
        let mut enc = SegmentEncoder::new(a);
        enc.spawned(a.tick + 100, 1, nz(5), "Widget", None, None);
        enc.spawned(
            a.tick + 250,
            1,
            nz(9),
            "Gadget",
            Some(nz(5)),
            Some(EventRef { actor: nz(5), seq: 1 }),
        );
        enc.spawned(a.tick + 260, 2, nz(12), "Widget", Some(nz(5)), None); // reuses "Widget"
        let bytes = enc.finish(a.mono_micros + 1000, a.tick + 100_000);
        let (seg, consumed) = decode_segment(&bytes).expect("decodes");
        assert_eq!(consumed, bytes.len());

        assert_eq!(seg.strings.len(), 2, "\"Widget\" is interned once");
        assert_eq!(seg.strings[0], "Widget");
        assert_eq!(seg.strings[1], "Gadget");

        assert_eq!(
            seg.records[0],
            Record::Spawned {
                ts_delta: 100,
                ev_seq: 1,
                actor_id: nz(5),
                actor_type: 0,
                parent: None,
                caused_by: None,
            }
        );
        assert_eq!(
            seg.records[1],
            Record::Spawned {
                ts_delta: 150, // 250 - 100, delta from the previous record
                ev_seq: 1,
                actor_id: nz(9),
                actor_type: 1,
                parent: Some(nz(5)),
                caused_by: Some(EventRef { actor: nz(5), seq: 1 }),
            }
        );
        assert_eq!(
            seg.records[2],
            Record::Spawned {
                ts_delta: 10,
                ev_seq: 2,
                actor_id: nz(12),
                actor_type: 0, // reused index
                parent: Some(nz(5)),
                caused_by: None,
            }
        );
    }

    #[test]
    fn message_records_round_trip_payload_and_external_sender() {
        let a = sample_anchor();
        let mut enc = SegmentEncoder::new(a);
        enc.message(a.tick + 5, 1, nz(7), None, "Ping", 0, None, &[0xde, 0xad]);
        enc.message(
            a.tick + 9,
            2,
            nz(8),
            Some(nz(7)),
            "Pong",
            1,
            Some(EventRef { actor: nz(7), seq: 1 }),
            &[],
        );
        let bytes = enc.finish(a.mono_micros + 1000, a.tick + 100_000);
        let (seg, _) = decode_segment(&bytes).expect("decodes");

        assert_eq!(
            seg.records[0],
            Record::Message {
                ts_delta: 5,
                ev_seq: 1,
                actor_id: nz(7),
                from: None,
                message_type: 0,
                dispatch: 0,
                caused_by: None,
                payload: Vec::from([0xde_u8, 0xad]),
            }
        );
        assert_eq!(
            seg.records[1],
            Record::Message {
                ts_delta: 4,
                ev_seq: 2,
                actor_id: nz(8),
                from: Some(nz(7)),
                message_type: 1,
                dispatch: 1,
                caused_by: Some(EventRef { actor: nz(7), seq: 1 }),
                payload: Vec::new(),
            }
        );
    }

    #[test]
    fn died_record_round_trips() {
        let a = sample_anchor();
        let mut enc = SegmentEncoder::new(a);
        enc.died(
            a.tick + 20,
            3,
            nz(5),
            "Widget",
            2,
            Some(EventRef { actor: nz(5), seq: 1 }),
        );
        let bytes = enc.finish(a.mono_micros + 1000, a.tick + 100_000);
        let (seg, _) = decode_segment(&bytes).expect("decodes");
        assert_eq!(
            seg.records[0],
            Record::Died {
                ts_delta: 20,
                ev_seq: 3,
                actor_id: nz(5),
                actor_type: 0,
                reason: 2,
                caused_by: Some(EventRef { actor: nz(5), seq: 1 }),
            }
        );
    }

    #[test]
    fn event_times_interpolate_within_the_open_close_bracket() {
        // 1 micro per tick: mono span 1000us over tick span 1000.
        let anchor = SegmentAnchor {
            mode: ClockMode::Tsc,
            unix_micros: 1_000_000,
            mono_micros: 500,
            tick: 1000,
        };
        let mut enc = SegmentEncoder::new(anchor);
        enc.spawned(1000, 1, nz(1), "A", None, None); // at open -> +0us
        enc.spawned(1500, 2, nz(2), "A", None, None); // halfway -> +500us
        enc.spawned(2000, 3, nz(3), "A", None, None); // at close -> +1000us
        let bytes = enc.finish(1500, 2000);
        let (seg, _) = decode_segment(&bytes).expect("decodes");

        assert_eq!(
            seg.event_unix_micros(),
            Vec::from([1_000_000, 1_000_500, 1_001_000])
        );
    }

    #[test]
    fn zero_tick_span_maps_every_event_to_open_time() {
        let anchor = SegmentAnchor {
            mode: ClockMode::Monotonic,
            unix_micros: 5_000,
            mono_micros: 0,
            tick: 100,
        };
        let mut enc = SegmentEncoder::new(anchor);
        enc.spawned(100, 1, nz(1), "A", None, None);
        let bytes = enc.finish(0, 100); // close_tick == open_tick -> zero span
        let (seg, _) = decode_segment(&bytes).expect("decodes");

        assert_eq!(seg.event_unix_micros(), Vec::from([5_000]));
    }
}
