//! `#[derive(Message)]`: answer types and RTTI names.

use factories::message::Message;

use crate::actor::{Get, Hit};
use crate::util::assert_type_eq;

/// Message with everything defaulted - the answer type must fall back to `()`.
#[derive(Debug, Message)]
struct Tick;

#[test]
fn message_answer_types() {
    assert_type_eq::<<Tick as Message>::Answer, ()>();
    assert_type_eq::<<Get as Message>::Answer, u32>();
    assert_type_eq::<<Hit as Message>::Answer, u32>();
}

#[test]
fn message_rtti_names() {
    assert_eq!(<Tick as Message>::RTTI.name(), "Tick");
    assert_eq!(<Get as Message>::RTTI.name(), "Get");
    assert_eq!(<Hit as Message>::RTTI.name(), "hit");
}
