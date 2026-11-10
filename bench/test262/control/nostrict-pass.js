// Control file of bench/test262.py (vault/CONTRACT.md §5.4, §5.6).
/*---
description: Runs only in non-strict mode, where with statements and a global this in calls are allowed.
flags: [noStrict]
---*/

var thisValue = (function () {
  return this;
})();
assert.sameValue(thisValue, this, "this of a plain call in sloppy code");
var scope = { inner: 1 };
with (scope) {
  assert.sameValue(inner, 1, "with statement");
}
