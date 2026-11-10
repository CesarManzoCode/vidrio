// Control file of bench/test262.py (vault/CONTRACT.md §5.4, §5.6).
/*---
description: Asynchronous test that completes, so it prints Test262:AsyncTestComplete through $DONE.
flags: [async]
---*/

Promise.resolve(1)
  .then(function (value) {
    assert.sameValue(value, 1, "resolved value");
  })
  .then($DONE, $DONE);
