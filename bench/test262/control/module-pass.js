// Control file of bench/test262.py (vault/CONTRACT.md §5.4, §5.6).
/*---
description: >
  Runs as module code: top-level this is undefined, import.meta is an object
  and a relative import of the file itself is the same module.
flags: [module]
---*/

import * as self from "./module-pass.js";

export const marker = 1;

assert.sameValue(this, undefined, "top-level this of a module");
assert.sameValue(typeof import.meta, "object", "import.meta");
assert.sameValue(self.marker, 1, "self-import resolves to this module");
