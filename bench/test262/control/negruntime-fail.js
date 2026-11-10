// Control file of bench/test262.py (vault/CONTRACT.md §5.4, §5.6).
/*---
description: >
  Declares a runtime ReferenceError but throws a TypeError; it fails because
  the name of the uncaught error differs from negative.type.
negative:
  phase: runtime
  type: ReferenceError
---*/

null.property;
