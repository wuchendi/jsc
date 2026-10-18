// jsc-compatible replacement for upstream test262 harness/assert.js.
// jsc functions cannot carry properties yet, so `assert.<name>` helpers are
// top-level functions named `assert_<name>`; the runner rewrites every
// `assert.<name>(` call in the test source accordingly.
// jsc also rejects calling a top-level function that references a top-level
// binding (here: the Test262Error class) from another top-level function, so
// no helper that throws Test262Error calls another such helper; shared logic
// lives in helpers that only touch their arguments.

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

function assert(mustBeTrue, message) {
  if (mustBeTrue === true) {
    return;
  }
  if (message === undefined) {
    message = "Expected true but got " + assert__toString(mustBeTrue);
  }
  throw new Test262Error(message);
}

function assert__isSameValue(a, b) {
  if (a === b) {
    // Handle +/-0 vs. -/+0
    return a !== 0 || 1 / a === 1 / b;
  }
  // Handle NaN vs. NaN
  return a !== a && b !== b;
}

function assert_sameValue(actual, expected, message) {
  if (assert__isSameValue(actual, expected)) {
    return;
  }
  if (message === undefined) {
    message = "";
  } else {
    message += " ";
  }
  message += "Expected SameValue(«" + assert__toString(actual) + "», «" + assert__toString(expected) + "») to be true";
  throw new Test262Error(message);
}

function assert_notSameValue(actual, unexpected, message) {
  if (!assert__isSameValue(actual, unexpected)) {
    return;
  }
  if (message === undefined) {
    message = "";
  } else {
    message += " ";
  }
  message += "Expected SameValue(«" + assert__toString(actual) + "», «" + assert__toString(unexpected) + "») to be false";
  throw new Test262Error(message);
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
function assert_throws(expectedErrorConstructor, func, message) {
  let r = assert__runThrows(func, expectedErrorConstructor.name);
  if (r.failure !== "") {
    throw new Test262Error(assert__throwsMessage(message) + r.failure);
  }
  if (r.thrown.constructor !== expectedErrorConstructor) {
    throw new Test262Error(assert__throwsMessage(message) + "Expected a " + expectedErrorConstructor.name + " but got a " + r.thrown.constructor.name);
  }
}

// Variants of `assert.throws(<Name>, func, message)` for the constructors jsc
// can check today: a runner may rewrite `assert.throws(<Name>,` to
// `assert_throws_<Name>(`. jsc's `instanceof` only accepts a literal user
// class or Error/TypeError/RangeError on its right-hand side, so each variant
// names its constructor directly. Builtin variants also compare `.name` so a
// TypeError/RangeError does not satisfy an expected plain Error, keeping
// upstream's exact-constructor semantics.
function assert_throws_Test262Error(func, message) {
  let r = assert__runThrows(func, "Test262Error");
  if (r.failure !== "") {
    throw new Test262Error(assert__throwsMessage(message) + r.failure);
  }
  if (!(r.thrown instanceof Test262Error)) {
    throw new Test262Error(assert__throwsMessage(message) + "Expected a Test262Error but got a " + r.thrown.name);
  }
}

function assert_throws_Error(func, message) {
  let r = assert__runThrows(func, "Error");
  if (r.failure !== "") {
    throw new Test262Error(assert__throwsMessage(message) + r.failure);
  }
  if (!(r.thrown instanceof Error) || r.thrown.name !== "Error") {
    throw new Test262Error(assert__throwsMessage(message) + "Expected a Error but got a " + r.thrown.name);
  }
}

function assert_throws_TypeError(func, message) {
  let r = assert__runThrows(func, "TypeError");
  if (r.failure !== "") {
    throw new Test262Error(assert__throwsMessage(message) + r.failure);
  }
  if (!(r.thrown instanceof TypeError) || r.thrown.name !== "TypeError") {
    throw new Test262Error(assert__throwsMessage(message) + "Expected a TypeError but got a " + r.thrown.name);
  }
}

function assert_throws_RangeError(func, message) {
  let r = assert__runThrows(func, "RangeError");
  if (r.failure !== "") {
    throw new Test262Error(assert__throwsMessage(message) + r.failure);
  }
  if (!(r.thrown instanceof RangeError) || r.thrown.name !== "RangeError") {
    throw new Test262Error(assert__throwsMessage(message) + "Expected a RangeError but got a " + r.thrown.name);
  }
}
