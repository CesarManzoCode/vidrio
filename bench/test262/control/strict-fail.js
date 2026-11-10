// Control file of bench/test262.py (vault/CONTRACT.md §5.4, §5.6).
/*---
description: >
  Asserts sloppy-mode behavior under onlyStrict; it fails because the runner
  inserts "use strict"; before the code.
flags: [onlyStrict]
---*/

var thisValue = (function () {
  return this;
})();
assert.sameValue(thisValue, this, "this of a plain call is the global object only in sloppy code");
