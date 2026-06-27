//! The capture schema trait, the visitor it pushes into, and impls for
//! primitives and standard containers.

use alloc::string::String;
use alloc::vec::Vec;

use crate::interpretation::Interpretation;

/// Dump-time configuration handed to each [`CaptureSchema::capture`] call. A
/// message decides, per field, what to emit at a given verbosity.
#[derive(Debug, Copy, Clone)]
pub struct CaptureConfig {
    /// Higher reveals more; a field is emitted only if its level is admitted.
    pub verbosity: u8,
}

/// Receives the structure and values a [`CaptureSchema`] pushes.
///
/// Implemented by encoders (and test recorders). The generic-`V` capture path
/// keeps it fully static — no `dyn` dispatch while walking a value.
pub trait FieldVisitor {
    /// An unsigned integer leaf.
    fn uint(&mut self, value: u64);
    /// A signed integer leaf.
    fn int(&mut self, value: i64);
    /// A floating-point leaf.
    fn float(&mut self, value: f64);
    /// A boolean leaf.
    fn boolean(&mut self, value: bool);
    /// A string leaf.
    fn string(&mut self, value: &str);

    /// Begin a named struct; followed by its `field`/value pairs and `end_struct`.
    fn begin_struct(&mut self, name: &'static str);
    /// Announce the next value is the struct field `name`, carrying `interpretation`.
    fn field(&mut self, name: &'static str, interpretation: Interpretation<'_>);
    /// End the current struct.
    fn end_struct(&mut self);

    /// Begin a sequence of `len` values; followed by `len` values and `end_seq`.
    fn begin_seq(&mut self, len: usize);
    /// End the current sequence.
    fn end_seq(&mut self);

    /// A present optional; followed by the inner value.
    fn some(&mut self);
    /// An absent optional.
    fn none(&mut self);
}

/// A value that can describe and emit itself for capture. Implemented for
/// primitives and standard containers here; derived for user structs.
pub trait CaptureSchema {
    /// Push this value's structure and contents into `visitor`, revealing only
    /// what `config` admits.
    fn capture<V: FieldVisitor>(&self, visitor: &mut V, config: &CaptureConfig);
}

macro_rules! impl_uint {
    ($($t:ty),*) => {$(
        impl CaptureSchema for $t {
            fn capture<V: FieldVisitor>(&self, visitor: &mut V, _config: &CaptureConfig) {
                visitor.uint(*self as u64);
            }
        }
    )*};
}
impl_uint!(u8, u16, u32, u64, usize);

macro_rules! impl_int {
    ($($t:ty),*) => {$(
        impl CaptureSchema for $t {
            fn capture<V: FieldVisitor>(&self, visitor: &mut V, _config: &CaptureConfig) {
                visitor.int(*self as i64);
            }
        }
    )*};
}
impl_int!(i8, i16, i32, i64, isize);

macro_rules! impl_float {
    ($($t:ty),*) => {$(
        impl CaptureSchema for $t {
            fn capture<V: FieldVisitor>(&self, visitor: &mut V, _config: &CaptureConfig) {
                visitor.float(*self as f64);
            }
        }
    )*};
}
impl_float!(f32, f64);

impl CaptureSchema for bool {
    fn capture<V: FieldVisitor>(&self, visitor: &mut V, _config: &CaptureConfig) {
        visitor.boolean(*self);
    }
}

impl CaptureSchema for str {
    fn capture<V: FieldVisitor>(&self, visitor: &mut V, _config: &CaptureConfig) {
        visitor.string(self);
    }
}

impl CaptureSchema for String {
    fn capture<V: FieldVisitor>(&self, visitor: &mut V, _config: &CaptureConfig) {
        visitor.string(self.as_str());
    }
}

impl<T: CaptureSchema + ?Sized> CaptureSchema for &T {
    fn capture<V: FieldVisitor>(&self, visitor: &mut V, config: &CaptureConfig) {
        (**self).capture(visitor, config);
    }
}

impl<T: CaptureSchema> CaptureSchema for Vec<T> {
    fn capture<V: FieldVisitor>(&self, visitor: &mut V, config: &CaptureConfig) {
        visitor.begin_seq(self.len());
        for element in self {
            element.capture(visitor, config);
        }
        visitor.end_seq();
    }
}

impl<T: CaptureSchema> CaptureSchema for Option<T> {
    fn capture<V: FieldVisitor>(&self, visitor: &mut V, config: &CaptureConfig) {
        match self {
            Some(value) => {
                visitor.some();
                value.capture(visitor, config);
            }
            None => visitor.none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpretation::Scalar;
    use alloc::format;
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    /// Records the visitor calls as a flat event log, so tests pin the exact
    /// push protocol.
    #[derive(Default)]
    struct Recorder {
        events: Vec<String>,
    }

    impl FieldVisitor for Recorder {
        fn uint(&mut self, value: u64) {
            self.events.push(format!("uint({value})"));
        }
        fn int(&mut self, value: i64) {
            self.events.push(format!("int({value})"));
        }
        fn float(&mut self, value: f64) {
            self.events.push(format!("float({value})"));
        }
        fn boolean(&mut self, value: bool) {
            self.events.push(format!("bool({value})"));
        }
        fn string(&mut self, value: &str) {
            self.events.push(format!("str({value})"));
        }
        fn begin_struct(&mut self, name: &'static str) {
            self.events.push(format!("begin_struct({name})"));
        }
        fn field(&mut self, name: &'static str, interpretation: Interpretation<'_>) {
            if interpretation.is_none() {
                self.events.push(format!("field({name})"));
            } else {
                self.events.push(format!("field({name},{})", interpretation.name));
            }
        }
        fn end_struct(&mut self) {
            self.events.push("end_struct".into());
        }
        fn begin_seq(&mut self, len: usize) {
            self.events.push(format!("begin_seq({len})"));
        }
        fn end_seq(&mut self) {
            self.events.push("end_seq".into());
        }
        fn some(&mut self) {
            self.events.push("some".into());
        }
        fn none(&mut self) {
            self.events.push("none".into());
        }
    }

    fn record_at<T: CaptureSchema + ?Sized>(value: &T, verbosity: u8) -> Vec<String> {
        let mut recorder = Recorder::default();
        value.capture(&mut recorder, &CaptureConfig { verbosity });
        recorder.events
    }

    fn record<T: CaptureSchema + ?Sized>(value: &T) -> Vec<String> {
        record_at(value, 0)
    }

    #[test]
    fn primitives_emit_their_leaf_kind() {
        assert_eq!(record(&5u32), vec!["uint(5)"]);
        assert_eq!(record(&-3i32), vec!["int(-3)"]);
        assert_eq!(record(&true), vec!["bool(true)"]);
        assert_eq!(record("hi"), vec!["str(hi)"]);
    }

    #[test]
    fn seq_frames_its_elements() {
        assert_eq!(
            record(&vec![1u32, 2, 3]),
            vec!["begin_seq(3)", "uint(1)", "uint(2)", "uint(3)", "end_seq"]
        );
    }

    #[test]
    fn option_marks_present_or_absent() {
        assert_eq!(record(&Some(7u32)), vec!["some", "uint(7)"]);
        assert_eq!(record(&None::<u32>), vec!["none"]);
    }

    // A hand-written impl standing in for what the derive will generate, to pin
    // the struct framing + per-field verbosity gating + interpretation passthrough.
    struct Frame {
        id: u32,
        samples: Vec<u32>,
        note: Option<u32>,
    }

    impl CaptureSchema for Frame {
        fn capture<V: FieldVisitor>(&self, visitor: &mut V, config: &CaptureConfig) {
            visitor.begin_struct("Frame");
            visitor.field("id", Interpretation::NONE);
            self.id.capture(visitor, config);
            if config.verbosity >= 1 {
                visitor.field(
                    "samples",
                    Interpretation {
                        name: "audio",
                        params: &[("channels", Scalar::Uint(2))],
                    },
                );
                self.samples.capture(visitor, config);
            }
            visitor.field("note", Interpretation::NONE);
            self.note.capture(visitor, config);
            visitor.end_struct();
        }
    }

    #[test]
    fn struct_gates_fields_by_verbosity() {
        let frame = Frame {
            id: 1,
            samples: vec![9],
            note: None,
        };

        assert_eq!(
            record_at(&frame, 0),
            vec![
                "begin_struct(Frame)",
                "field(id)",
                "uint(1)",
                "field(note)",
                "none",
                "end_struct",
            ],
            "at verbosity 0 the audio samples are withheld",
        );
        assert_eq!(
            record_at(&frame, 1),
            vec![
                "begin_struct(Frame)",
                "field(id)",
                "uint(1)",
                "field(samples,audio)",
                "begin_seq(1)",
                "uint(9)",
                "end_seq",
                "field(note)",
                "none",
                "end_struct",
            ],
            "at verbosity 1 samples appear with their audio interpretation",
        );
    }
}
