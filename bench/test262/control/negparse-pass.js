// Control file of bench/test262.py (vault/CONTRACT.md §5.4, §5.6).
/*---
description: Early error (duplicate lexical declaration), so the code never runs.
negative:
  phase: parse
  type: SyntaxError
---*/

$DONOTEVALUATE();

let duplicate;
let duplicate;
