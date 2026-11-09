//! Integration tests: out-of-bounds array/string index reads yield `undefined`
//! (index `>= length` or negative), while in-range reads are unchanged.
#![allow(clippy::expect_used)]

mod common;
use common::{assert_number, assert_string, assert_type_str, eval_text};

/// An array index past the last element reads `undefined`.
#[test]
fn array_index_past_end_is_undefined() {
    let result = eval_text("let a=[1]; a[9]");
    assert_eq!(result, "undefined");
}

/// An array index exactly at `length` (one past the last valid index) is `undefined`.
#[test]
fn array_index_at_length_is_undefined() {
    let result = eval_text("let a=[1,2,3]; a[3]");
    assert_eq!(result, "undefined");
}

/// A string index past the last character reads `undefined`.
#[test]
fn string_index_past_end_is_undefined() {
    let result = eval_text("\"ab\"[9]");
    assert_eq!(result, "undefined");
}

/// A negative array index reads `undefined` (not the clamped element 0).
#[test]
fn array_negative_index_is_undefined() {
    let result = eval_text("let a=[1]; a[-1]");
    assert_eq!(result, "undefined");
}

/// In-range array access is unchanged.
#[test]
fn array_in_range_unchanged() {
    assert_number("[10,20,30][1]", 20.0);
}

/// In-range string access is unchanged.
#[test]
fn string_in_range_unchanged() {
    assert_string("\"abc\"[1]", "b");
}

/// `typeof` of an out-of-bounds read is `"undefined"`.
#[test]
fn typeof_oob_read_is_undefined() {
    assert_type_str("let a=[1]; typeof a[9]", "undefined");
}

/// A store past the length grows the array (dense part): the gap reads
/// `undefined` and `length` is the index plus one.
#[test]
fn array_store_past_end_grows() {
    assert_number("let a=[1]; a[3]=4; a[3]", 4.0);
    assert_number("let a=[1]; a[3]=4; a.length", 4.0);
    let gap = eval_text("let a=[1]; a[3]=4; a[2]");
    assert_eq!(gap, "undefined");
}

/// A hole renders as an empty-item run inside the array and reads `undefined`.
#[test]
fn array_hole_decodes_and_reads_undefined() {
    let arr = eval_text("[1,,3]");
    assert_eq!(arr, "[ 1, <1 empty item>, 3 ]");
    let read = eval_text("[1,,3][1]");
    assert_eq!(read, "undefined");
}
