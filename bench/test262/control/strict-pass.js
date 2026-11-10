// Control file of bench/test262.py (vault/CONTRACT.md §5.4, §5.6).
/*---
description: Runs only in strict mode, so a plain function call has an undefined this.
flags: [onlyStrict]
---*/

var thisValue = (function () {
  return this;
})();
assert.sameValue(thisValue, undefined, "this of a plain call in strict code");
assert.throws(TypeError, function () {
  var frozen = Object.freeze({ property: 1 });
  frozen.property = 2;
}, "assignment to a non-writable property throws in strict code");
