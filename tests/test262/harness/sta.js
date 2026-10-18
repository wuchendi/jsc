// jsc-compatible replacement for upstream test262 harness/sta.js.
// Upstream defines Test262Error as a plain-function constructor with
// properties on the function (Test262Error.thrower); jsc supports neither
// yet, so Test262Error is a class and `thrower` is omitted.

class Test262Error {
  constructor(message) {
    this.message = message || "";
  }

  toString() {
    return "Test262Error: " + this.message;
  }
}

function $DONOTEVALUATE() {
  throw "Test262: This statement should not be evaluated.";
}
