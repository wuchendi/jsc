//! Integration tests: the host renderer's cycle guard. A cyclic structure
//! renders Node-console style (`<ref *1>` / `[Circular *1]`) instead of
//! overflowing the native stack. Expected texts are Node's `console.log` output.
#![allow(clippy::expect_used)]

use jsc_core::run::{compile_and_run, format_value};
mod common;
use common::assert_output;

/// An array that contains itself. Built with in-bounds index writes: `push`
/// reallocates the array today, so `a.push(a)` stores the old block.
#[test]
fn self_array() {
    assert_output(
        "let a = [1, 0]; a[1] = a; console.log(a)",
        "<ref *1> [ 1, [Circular *1] ]\n",
    );
    assert_output(
        "let a = [1, 0, 0]; a[1] = a; a[2] = a; console.log(a)",
        "<ref *1> [ 1, [Circular *1], [Circular *1] ]\n",
    );
}

/// An object whose existing key is reassigned to itself.
#[test]
fn self_object() {
    assert_output(
        "let o = {self: null, x: 1}; o.self = o; console.log(o)",
        "<ref *1> { self: [Circular *1], x: 1 }\n",
    );
    assert_output(
        "let o = {self: null}; o.self = o; console.log([o])",
        "[ <ref *1> { self: [Circular *1] } ]\n",
    );
}

/// A cycle through an object and a nested array.
#[test]
fn object_array_cycle() {
    assert_output(
        "let o = {x: [0]}; o.x[0] = o; console.log(o)",
        "<ref *1> { x: [ [Circular *1] ] }\n",
    );
}

/// Two objects that point at each other.
#[test]
fn mutual_cycle() {
    assert_output(
        "let a = {b: null}; let b = {a: a}; a.b = b; console.log(a)",
        "<ref *1> { b: { a: [Circular *1] } }\n",
    );
}

/// Distinct cycle targets are numbered in the order their cycles are found.
#[test]
fn multiple_targets() {
    assert_output(
        "let a = [1, 0, 0]; let b = [2, 0]; a[1] = b; b[1] = b; a[2] = a; console.log(a)",
        "<ref *2> [ 1, <ref *1> [ 2, [Circular *1] ], [Circular *2] ]\n",
    );
    assert_output(
        "let a = [1, 0]; let b = [2, a, 0]; a[1] = b; b[2] = b; console.log(a)",
        "<ref *1> [ 1, <ref *2> [ 2, [Circular *1], [Circular *2] ] ]\n",
    );
}

/// A shared, non-cyclic reference renders in full both times.
#[test]
fn shared_reference_is_not_circular() {
    assert_output(
        "let s = {v: 1}; let o = {p: s, q: s}; console.log(o)",
        "{ p: { v: 1 }, q: { v: 1 } }\n",
    );
}

/// The completion value (`jsc -p`) goes through the same guard.
#[test]
fn completion_value() {
    let value = compile_and_run("let a = [1, 0]; a[1] = a; a").expect("runs");
    assert_eq!(format_value(&value), "<ref *1> [ 1, [Circular *1] ]");
}
