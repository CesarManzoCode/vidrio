// Control file of bench/test262.py (vault/CONTRACT.md §5.4, §5.6).
/*---
description: >
  Asynchronous test that reports Test262:AsyncTestFailure through $DONE; the
  process ends without an uncaught error, so only the async rule fails it.
flags: [async]
---*/

Promise.resolve(1)
  .then(function () {
    throw new Test262Error("asynchronous failure");
  })
  .then($DONE, $DONE);
