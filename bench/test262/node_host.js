'use strict';
// Node host for bench/test262.py (vault/CONTRACT.md §5.4).
//
//   node --experimental-vm-modules --expose-gc node_host.js [--module] FILE
//
// Runs FILE (already prepared by the runner: strict prefix and prelude) in a
// fresh realm made with the vm module, with the host-defined `print` and
// `$262` of INTERPRETING.md (createRealm, evalScript, detachArrayBuffer, gc,
// global; no agent, IsHTMLDDA or AbstractModuleSource). Module specifiers are
// resolved relative to the importing file; `.json` files become JSON modules.
//
// An uncaught error prints one line `Uncaught <Name>: <message>` to stderr
// and exits with 1. <Name> is the name of the thrown value's constructor (its
// `name` property if it has none); a thrown primitive reports its type, so a
// thrown string never looks like an error constructor. Unhandled promise
// rejections are not test failures and are ignored.

const fs = require('fs');
const path = require('path');
const vm = require('vm');

const argv = process.argv.slice(2);
const asModule = argv[0] === '--module';
const file = path.resolve(asModule ? argv[1] : argv[0]);

const transferBuffer = ArrayBuffer.prototype.transfer;

process.on('unhandledRejection', () => {});

function isObject(value) {
  return (typeof value === 'object' && value !== null) || typeof value === 'function';
}

function errorName(value) {
  if (!isObject(value)) {
    return typeof value;
  }
  try {
    const ctor = value.constructor;
    if (typeof ctor === 'function' && typeof ctor.name === 'string' && ctor.name !== '') {
      return ctor.name;
    }
  } catch {}
  try {
    return String(value.name);
  } catch {
    return 'Object';
  }
}

function errorMessage(value) {
  try {
    return String(isObject(value) ? value.message : value);
  } catch {
    return '';
  }
}

function reportUncaught(value) {
  const message = errorMessage(value).split('\n')[0];
  process.stderr.write(`Uncaught ${errorName(value)}: ${message}\n`);
  process.exitCode = 1;
}

function print(value) {
  process.stdout.write(`${String(value)}\n`);
}

function resolveSpecifier(referrer, specifier) {
  return path.resolve(path.dirname(referrer), String(specifier));
}

// Errors raised by the host itself (compiling a script or a module, reading a
// file) belong to Node's main realm; tests compare constructors of the test
// realm, so they are rethrown as the same kind of error of `realm`.
function toRealmError(realm, err) {
  if (err instanceof Error) {
    const ctor = realm.errors[err.name] || realm.errors.Error;
    return new ctor(err.message);
  }
  return err;
}

function createRealm() {
  const realm = {};
  realm.context = vm.createContext(undefined, {
    importModuleDynamically: (specifier, referrer, attributes) =>
      realm.loader.importDynamic(specifier, file),
  });
  realm.global = vm.runInContext('this', realm.context);
  realm.errors = vm.runInContext(
    '({ Error, EvalError, RangeError, ReferenceError, SyntaxError, TypeError, URIError })',
    realm.context,
  );
  realm.parseJSON = vm.runInContext('JSON.parse', realm.context);
  realm.loader = createLoader(realm);

  const $262 = vm.runInContext('({})', realm.context);
  $262.createRealm = () => createRealm().$262;
  $262.evalScript = (source) => evalScript(realm, source);
  $262.detachArrayBuffer = (buffer) => {
    if (typeof transferBuffer === 'function') {
      transferBuffer.call(buffer);
    } else {
      structuredClone(buffer, { transfer: [buffer] });
    }
    return null;
  };
  $262.gc = () => {
    if (typeof globalThis.gc !== 'function') {
      throw new realm.errors.TypeError('gc is not exposed (run node with --expose-gc)');
    }
    globalThis.gc();
  };
  $262.global = realm.global;
  realm.$262 = $262;

  const install = vm.runInContext(
    '(function (name, value) {' +
      ' Object.defineProperty(globalThis, name,' +
      ' { value: value, writable: true, configurable: true, enumerable: false }); })',
    realm.context,
  );
  install('print', print);
  install('$262', $262);
  return realm;
}

function evalScript(realm, source) {
  let script;
  try {
    script = new vm.Script(String(source), {
      filename: 'evalScript',
      importModuleDynamically: (specifier) => realm.loader.importDynamic(specifier, file),
    });
  } catch (err) {
    throw toRealmError(realm, err);
  }
  return script.runInContext(realm.context);
}

function createLoader(realm) {
  const cache = new Map();

  function load(filename) {
    let mod = cache.get(filename);
    if (mod) {
      return mod;
    }
    const text = fs.readFileSync(filename, 'utf8');
    if (filename.endsWith('.json')) {
      mod = new vm.SyntheticModule(
        ['default'],
        function () {
          this.setExport('default', realm.parseJSON(text));
        },
        { context: realm.context, identifier: filename },
      );
    } else {
      mod = new vm.SourceTextModule(text, {
        context: realm.context,
        identifier: filename,
        importModuleDynamically: (specifier) => importDynamic(specifier, filename),
      });
    }
    cache.set(filename, mod);
    return mod;
  }

  function linkGraph(mod, seen) {
    if (seen.has(mod) || !(mod instanceof vm.SourceTextModule) || mod.status !== 'unlinked') {
      return;
    }
    seen.add(mod);
    const deps = mod.moduleRequests.map((request) =>
      load(resolveSpecifier(mod.identifier, request.specifier)),
    );
    mod.linkRequests(deps);
    for (const dep of deps) {
      linkGraph(dep, seen);
    }
  }

  async function link(mod) {
    if (mod.status !== 'unlinked') {
      return;
    }
    if (typeof mod.link === 'function') {
      await mod.link((specifier, referrer) => load(resolveSpecifier(referrer.identifier, specifier)));
    } else {
      linkGraph(mod, new Set());
      mod.instantiate();
    }
  }

  async function importDynamic(specifier, referrer) {
    let mod;
    try {
      mod = load(resolveSpecifier(referrer, specifier));
      await link(mod);
    } catch (err) {
      throw toRealmError(realm, err);
    }
    if (mod.status === 'linked') {
      await mod.evaluate();
    }
    return mod;
  }

  return { load, link, importDynamic };
}

async function main() {
  const realm = createRealm();
  if (asModule) {
    const mod = realm.loader.load(file);
    await realm.loader.link(mod);
    await mod.evaluate();
  } else {
    const script = new vm.Script(fs.readFileSync(file, 'utf8'), {
      filename: file,
      importModuleDynamically: (specifier) => realm.loader.importDynamic(specifier, file),
    });
    script.runInContext(realm.context);
  }
  // Let pending jobs (promise reactions, dynamic imports) run before exit.
  await new Promise((resolve) => setImmediate(resolve));
}

main().catch(reportUncaught);
