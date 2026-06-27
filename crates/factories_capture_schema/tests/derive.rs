#![cfg(feature = "derive")]

//! `#[derive(CaptureSchema)]`: gating, skip, rename, and a `self`-aware
//! interpretation that folds sibling fields into one synthetic field.

use factories_capture_schema::predicate::min_verbosity;
use factories_capture_schema::{CaptureConfig, CaptureSchema, FieldVisitor, Interpretation, Scalar};

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
            let params: Vec<String> = interpretation
                .params
                .iter()
                .map(|(key, value)| format!("{key}={value:?}"))
                .collect();
            self.events
                .push(format!("field({name},{},[{}])", interpretation.name, params.join(",")));
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

#[derive(CaptureSchema)]
struct Frame {
    id: u32,
    #[capture(if = min_verbosity(1))]
    debug_note: u32,
    // The next two are folded into the synthetic "audio" field below.
    #[capture(skip)]
    rate: u32,
    #[capture(
        rename = "audio",
        interpret = Interpretation { name: "audio", params: &[("rate", Scalar::Uint(self.rate as u64))] }
    )]
    data: Vec<u8>,
}

fn record(frame: &Frame, verbosity: u8) -> Vec<String> {
    let mut recorder = Recorder::default();
    frame.capture(&mut recorder, &CaptureConfig::new(verbosity));
    recorder.events
}

#[test]
fn derive_gates_skips_renames_and_interprets_with_self() {
    let frame = Frame {
        id: 7,
        debug_note: 99,
        rate: 48_000,
        data: vec![1, 2],
    };

    // verbosity 0: debug_note gated out; rate skipped; data → "audio" with rate
    // pulled from the sibling field via `self`.
    assert_eq!(
        record(&frame, 0),
        vec![
            "begin_struct(Frame)",
            "field(id)",
            "uint(7)",
            "field(audio,audio,[rate=Uint(48000)])",
            "begin_seq(2)",
            "uint(1)",
            "uint(2)",
            "end_seq",
            "end_struct",
        ]
    );

    // verbosity 1: the gated debug_note now appears.
    assert_eq!(
        record(&frame, 1),
        vec![
            "begin_struct(Frame)",
            "field(id)",
            "uint(7)",
            "field(debug_note)",
            "uint(99)",
            "field(audio,audio,[rate=Uint(48000)])",
            "begin_seq(2)",
            "uint(1)",
            "uint(2)",
            "end_seq",
            "end_struct",
        ]
    );
}
