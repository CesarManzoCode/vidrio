// Control file of bench/test262.py (vault/CONTRACT.md §5.4, §5.6).
/*---
description: >
  Declares a parse-phase SyntaxError on valid code; it fails because the code
  runs and reaches $DONOTEVALUATE().
negative:
  phase: parse
  type: SyntaxError
---*/

$DONOTEVALUATE();

var valid = 1;
