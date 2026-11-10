// Control file of bench/test262.py (vault/CONTRACT.md §5.4, §5.6).
/*---
description: >
  Asserts script behavior under the module flag; it fails because the runner
  must evaluate it as module code, where top-level this is undefined.
flags: [module]
---*/

assert.notSameValue(this, undefined, "top-level this is the global object only in a script");
