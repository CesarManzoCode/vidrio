// Control file of bench/test262.py (vault/CONTRACT.md §5.4, §5.6).
/*---
description: >
  Asserts strict-mode behavior under noStrict; it fails because the runner
  must not make the code strict.
flags: [noStrict]
---*/

var thisValue = (function () {
  return this;
})();
assert.sameValue(thisValue, undefined, "this of a plain call is undefined only in strict code");
