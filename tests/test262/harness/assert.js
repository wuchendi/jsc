// jsc-compatible replacement for upstream test262 harness/assert.js.
// jsc functions cannot carry properties yet, so `assert.<name>` helpers are
// top-level functions named `assert_<name>`; the runner rewrites every
// `assert.<name>(` call in the test source accordingly.
// It also defines compareArray / compareArray_format / assert_compareArray
// (as upstream does; compareArray.js is an empty include).
// jsc rejects a top-level function that references a top-level binding when it
// is called from another top-level function, and miscompiles such a call from
// a nested arrow/function expression (the callee may not throw). So every
// assertion function and helper here is a leaf over its arguments and
// builtins only: failures throw `new Error("Test262Error: " + message)`
// instead of `new Test262Error(message)`. The one exception is
// assert_throws_Test262Error, which must reference the Test262Error class for
// `instanceof` and is therefore still affected by that jsc bug when called
// from a nested function.
// jsc also requires exact arity when calling a capture-free top-level
// function, so every optional `message` parameter defaults to `undefined`
// (same behaviour as omitting it).

function isNegativeZero(value) {
  return value === 0 && 1 / value === -Infinity;
}

function isPrimitive(value) {
  return !value || (typeof value !== "object" && typeof value !== "function");
}

function assert__toString(value) {
  if (value === null) {
    return "null";
  }
  if (typeof value === "string") {
    return '"' + value + '"';
  }
  if (isNegativeZero(value)) {
    return "-0";
  }
  return String(value);
}

function assert(mustBeTrue, message = undefined) {
  if (mustBeTrue === true) {
    return;
  }
  if (message === undefined) {
    message = "Expected true but got " + assert__toString(mustBeTrue);
  }
  throw new Error("Test262Error: " + message);
}

function assert__isSameValue(a, b) {
  if (a === b) {
    // Handle +/-0 vs. -/+0
    return a !== 0 || 1 / a === 1 / b;
  }
  // Handle NaN vs. NaN
  return a !== a && b !== b;
}

function assert_sameValue(actual, expected, message = undefined) {
  if (assert__isSameValue(actual, expected)) {
    return;
  }
  if (message === undefined) {
    message = "";
  } else {
    message += " ";
  }
  message += "Expected SameValue(«" + assert__toString(actual) + "», «" + assert__toString(expected) + "») to be true";
  throw new Error("Test262Error: " + message);
}

function assert_notSameValue(actual, unexpected, message = undefined) {
  if (!assert__isSameValue(actual, unexpected)) {
    return;
  }
  if (message === undefined) {
    message = "";
  } else {
    message += " ";
  }
  message += "Expected SameValue(«" + assert__toString(actual) + "», «" + assert__toString(unexpected) + "») to be false";
  throw new Error("Test262Error: " + message);
}

function assert__throwsMessage(message) {
  return message === undefined ? "" : message + " ";
}

// Runs func and returns `{ failure, thrown }`: `failure` is "" when func threw
// an object, otherwise the upstream failure text for `name`.
function assert__runThrows(func, name) {
  if (typeof func !== "function") {
    return { failure: "assert.throws requires two arguments: the error constructor and a function to run", thrown: undefined };
  }
  let threw = false;
  let thrown;
  try {
    func();
  } catch (e) {
    threw = true;
    thrown = e;
  }
  if (!threw) {
    return { failure: "Expected a " + name + " to be thrown but no exception was thrown at all", thrown: undefined };
  }
  if (typeof thrown !== "object" || thrown === null) {
    return { failure: "Thrown value was not an object!", thrown: thrown };
  }
  return { failure: "", thrown: thrown };
}

// Same check as upstream (`thrown.constructor` identity). jsc does not yet
// expose `.constructor`, and builtin error constructors are not first-class
// values, so on jsc this only works once those land; until then use the
// `assert_throws_<Name>` variants below.
function assert_throws(expectedErrorConstructor, func, message = undefined) {
  let r = assert__runThrows(func, expectedErrorConstructor.name);
  if (r.failure !== "") {
    throw new Error("Test262Error: " + assert__throwsMessage(message) + r.failure);
  }
  if (r.thrown.constructor !== expectedErrorConstructor) {
    throw new Error("Test262Error: " + assert__throwsMessage(message) + "Expected a " + expectedErrorConstructor.name + " but got a " + r.thrown.constructor.name);
  }
}

// Variants of `assert.throws(<Name>, func, message)` for the constructors jsc
// can check today: a runner may rewrite `assert.throws(<Name>,` to
// `assert_throws_<Name>(`. jsc's `instanceof` only accepts a literal user
// class or Error/TypeError/RangeError on its right-hand side, so each variant
// names its constructor directly. Builtin variants also compare `.name` so a
// TypeError/RangeError does not satisfy an expected plain Error, keeping
// upstream's exact-constructor semantics.
function assert_throws_Test262Error(func, message = undefined) {
  let r = assert__runThrows(func, "Test262Error");
  if (r.failure !== "") {
    throw new Error("Test262Error: " + assert__throwsMessage(message) + r.failure);
  }
  if (!(r.thrown instanceof Test262Error)) {
    throw new Error("Test262Error: " + assert__throwsMessage(message) + "Expected a Test262Error but got a " + r.thrown.name);
  }
}

function assert_throws_Error(func, message = undefined) {
  let r = assert__runThrows(func, "Error");
  if (r.failure !== "") {
    throw new Error("Test262Error: " + assert__throwsMessage(message) + r.failure);
  }
  if (!(r.thrown instanceof Error) || r.thrown.name !== "Error") {
    throw new Error("Test262Error: " + assert__throwsMessage(message) + "Expected a Error but got a " + r.thrown.name);
  }
}

function assert_throws_TypeError(func, message = undefined) {
  let r = assert__runThrows(func, "TypeError");
  if (r.failure !== "") {
    throw new Error("Test262Error: " + assert__throwsMessage(message) + r.failure);
  }
  if (!(r.thrown instanceof TypeError) || r.thrown.name !== "TypeError") {
    throw new Error("Test262Error: " + assert__throwsMessage(message) + "Expected a TypeError but got a " + r.thrown.name);
  }
}

function assert_throws_RangeError(func, message = undefined) {
  let r = assert__runThrows(func, "RangeError");
  if (r.failure !== "") {
    throw new Error("Test262Error: " + assert__throwsMessage(message) + r.failure);
  }
  if (!(r.thrown instanceof RangeError) || r.thrown.name !== "RangeError") {
    throw new Error("Test262Error: " + assert__throwsMessage(message) + "Expected a RangeError but got a " + r.thrown.name);
  }
}

function compareArray(a, b) {
  if (b.length !== a.length) {
    return false;
  }
  for (let i = 0; i < a.length; i++) {
    if (!assert__isSameValue(b[i], a[i])) {
      return false;
    }
  }
  return true;
}

function compareArray_format(arrayLike) {
  let parts = [];
  for (let i = 0; i < arrayLike.length; i++) {
    parts.push(String(arrayLike[i]));
  }
  return "[" + parts.join(", ") + "]";
}

function assert_compareArray(actual, expected, message = undefined) {
  message = message === undefined ? "" : message;
  if (isPrimitive(actual)) {
    throw new Error("Test262Error: Actual argument [" + actual + "] shouldn't be primitive. " + String(message));
  } else if (isPrimitive(expected)) {
    throw new Error("Test262Error: Expected argument [" + expected + "] shouldn't be primitive. " + String(message));
  }
  if (compareArray(actual, expected)) {
    return;
  }
  throw new Error("Test262Error: Actual " + compareArray_format(actual) + " and expected " + compareArray_format(expected) + " should have the same contents. " + String(message));
}
