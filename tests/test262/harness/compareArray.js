// jsc-compatible replacement for upstream test262 harness/compareArray.js
// (upstream now defines these in assert.js). jsc functions cannot carry
// properties yet, so `assert.compareArray` is `assert_compareArray` and
// `compareArray.format` is the top-level `compareArray_format`. Failures throw
// Test262Error directly instead of calling `assert` (see assert.js).

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

function assert_compareArray(actual, expected, message) {
  message = message === undefined ? "" : message;
  if (isPrimitive(actual)) {
    throw new Test262Error("Actual argument [" + actual + "] shouldn't be primitive. " + String(message));
  } else if (isPrimitive(expected)) {
    throw new Test262Error("Expected argument [" + expected + "] shouldn't be primitive. " + String(message));
  }
  if (compareArray(actual, expected)) {
    return;
  }
  throw new Test262Error("Actual " + compareArray_format(actual) + " and expected " + compareArray_format(expected) + " should have the same contents. " + String(message));
}
