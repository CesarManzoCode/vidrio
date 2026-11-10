# Contrato de vidrio

Este archivo dice qué debe hacer el sistema y fija las decisiones cerradas. Lo que aquí se fija no se reabre dentro de un mini sprint; si algo resulta inviable, se registra en `vault/adr/` (solo si cambia un resultado) y en `LIMITATIONS.md`.

Las fuentes y revisiones citadas están en `vault/RESEARCH.md`. Convención: **hecho** = observado en esta PC o en una fuente primaria; **diseño** = decisión de este contrato; **hipótesis** = algo que se comprobará al medir.

## 1. Qué es vidrio

vidrio es un motor de navegador completo escrito desde cero para Arch Linux x86_64: parser HTML, DOM, CSS (sintaxis, valores, selectores, cascada, herencia, CSSOM), layout block, inline con flotantes, posicionado, flex, grid y tablas, texto, pintado con rasterizado propio, red HTTP/1.1 y TLS 1.3 propios, y un motor de JavaScript propio (`vjs`) con bucle de eventos HTML. Su calidad se demuestra solo con suites externas oficiales —web-platform-tests (WPT) con su arnés oficial `./wpt run` y test262— medidas en la misma PC contra Firefox, Chromium, Servo y Boa.

Entregables:

- `vidrio`: el navegador. Abre una ventana X11 y pinta páginas; con `--webdriver=PUERTO` expone un servidor WebDriver para el arnés.
- `vjs`: shell de consola del motor JS, para correr test262.
- `bench/`: el arnés de medición (apoyo, nunca más grande que el producto).

## 2. Reglas duras del producto

- **Dependencias externas permitidas** (en tiempo de ejecución): kernel Linux, glibc (libc, libm, libpthread, libdl, ld-linux), libxcb y libxcb-shm (MIT-SHM) y sus dependencias transitivas de X (libXau, libXdmcp). Nada más. El runtime de GCC se enlaza estático (ver §7, D-01).
- **Prohibido** como dependencia, en código, manifiesto o enlace: V8, QuickJS, SpiderMonkey/mozjs, JavaScriptCore, Servo y sus crates (html5ever, stylo, cssparser, selectors), WebKit, Blink, Gecko, Taffy, Skia, cairo, HarfBuzz, FreeType, fontconfig, curl y OpenSSL.
- **Cero crates de terceros.** `Cargo.lock` solo contiene los crates del propio workspace. Todo lo demás (Unicode, compresión, criptografía, JSON, PNG, fuentes) es propio.
- Los archivos de fuente del sistema, la zona horaria (`/usr/share/zoneinfo`) y las tablas de datos de Unicode y WHATWG son **datos**, no código: se leen o se convierten a tablas Rust generadas.
- El arnés de WPT se usa sin cambios salvo el módulo de producto de vidrio, versionado como parche explícito en `bench/wpt/vidrio.patch`.

## 3. Arquitectura

### 3.1 Workspace

Un workspace de Cargo (Rust 1.98.1, edition 2024, fijado en `rust-toolchain.toml`) con cuatro crates; las flechas son las únicas dependencias permitidas entre ellos:

| Crate | Contenido | Depende de |
| --- | --- | --- |
| `crates/vjs` | Motor JS: lexer, parser, compilador a bytecode, intérprete, GC, built-ins, Temporal, módulos; binario `vjs` | — |
| `crates/vnet` | Sockets (std), HTTP/1.1, TLS 1.3, criptografía, X.509, inflate/deflate, brotli | — |
| `crates/vgfx` | OpenType (TrueType y CFF), shaping, rasterizado con antialiasing, decodificadores PNG/GIF/JPEG, codificador PNG | `vnet` (solo inflate y brotli) |
| `crates/vidrio` | Encoding, URL, HTML, DOM, bindings Web IDL, bucle de eventos, fetch, CSS, estilo, layout, texto, pintado, ventana X11, servidor WebDriver; binario `vidrio` | `vjs`, `vnet`, `vgfx` |

Los binarios `vidrio` y `vjs` tienen un `build.rs` que copia `libgcc_eh.a` de GCC como `libgcc_s.a` en `OUT_DIR` y lo añade a la ruta de enlace, para que el ejecutable no dependa de `libgcc_s.so.1` (hecho verificado en esta PC, ver RESEARCH).

### 3.2 Procesos e hilos

Un solo proceso. Hilo principal: bucle de eventos HTML, DOM, JS, estilo, layout, pintado y eventos X. Un hilo para el servidor WebDriver (pasa comandos al principal por canal). Hilos de red por petición (pool acotado a 8) que devuelven respuestas al principal por canal. Los agentes de `$262.agent` en `vjs` son hilos con heap propio que comparten la memoria de `SharedArrayBuffer`.

### 3.3 DOM

Arena por documento (`Vec` de nodos con `NodeId(u32)`, enlaces padre/hijos/hermanos), nombres internados (átomos), atributos en vector por elemento. El documento vive en un `Rc`; lo sostienen su navegación activa y los wrappers JS que lo referencian.

### 3.4 Motor JS (`vjs`)

- Parser a AST con todos los early errors de ECMA-262; compilador a bytecode de pila; intérprete sin recursión nativa en llamadas JS→JS (marcos en una pila propia; generadores y async guardan su marco). Sin JIT.
- Objetos con *shapes* (clases ocultas) y transición a modo diccionario; objetos exóticos del host (DOM) con métodos internos propios.
- Strings UTF-16 inmutables con ropes para concatenación.
- **GC**: mark-sweep preciso y no movible sobre arenas tipadas con índices. Solo corre en puntos seguros (entre instrucciones del intérprete y entre tareas del bucle de eventos), así el código nativo no necesita raíces explícitas. Soporta weak refs, `FinalizationRegistry` y finalizadores Rust.
- **GC compartido con el DOM**: un solo heap para todo el proceso (un agente, un realm por ventana o iframe). Cada nodo tiene a lo sumo un wrapper; la tabla nodo→wrapper del documento es raíz del GC mientras el documento esté activo. Al descartar el documento se sueltan esas raíces; los wrappers que sigan vivos sostienen la arena por su `Rc`.
- Unicode 17.0.0 (la versión de los tests generados de test262 fijado) para identificadores, mayúsculas, normalización y propiedades de RegExp.
- Fecha y Temporal leen TZif de `/usr/share/zoneinfo`.

### 3.5 Estilo, layout y pintado

- CSS: tokenizador y parser de css-syntax-3, hojas con `@import`, `@media`, `@supports`, `@layer`, `@font-face`, `@namespace`, `@property`; CSSOM completo.
- Estilo: valores computados en estructuras agrupadas compartidas por `Rc` (heredadas y no heredadas), emparejamiento de selectores de derecha a izquierda con filtro bloom; cascada con orígenes, capas e `!important`; `var()` con detección de ciclos.
- Layout: árbol de cajas → árbol de fragmentos inmutable por pasada. Recalcula estilo y layout completos en cada oportunidad de renderizado con cambios (sin invalidación incremental hasta que una medición lo exija).
- Pintado: lista de pintado plana ordenada por contextos de apilamiento según CSS 2 Apéndice E; rasterizado propio a un framebuffer RGBA8 premultiplicado.
- Viewport por omisión 800×600 (el que usa wptrunner).

### 3.6 Ventana

X11 con libxcb y MIT-SHM: el framebuffer se entrega con `xcb_shm_put_image`. `--headless` no abre conexión X; la captura de WebDriver sale del framebuffer propio codificado como PNG.

## 4. Integración con el arnés de WPT

- vidrio trae un **servidor WebDriver propio** dentro del binario: `vidrio --webdriver=PUERTO [--headless] [--ca-cert=RUTA]`, igual que Servo. Resuelve los hosts de prueba con el archivo indicado en la variable `HOST_FILE` (formato `/etc/hosts`).
- El módulo de producto `vidrio` se modela sobre `ladybird.py` y usa los ejecutores WebDriver genéricos de wptrunner: `WebDriverTestharnessExecutor` (testharness y test262), `WebDriverRefTestExecutor` y `WebDriverCrashtestExecutor`. El parche `bench/wpt/vidrio.patch` toca exactamente cuatro archivos, todos de registro del producto: crea `tools/wptrunner/wptrunner/browsers/vidrio.py`, añade `"vidrio"` a `BUILTIN_PRODUCTS` en `tools/wptrunner/wptrunner/products.py`, añade la clase `Vidrio(Browser)` en `tools/wpt/browser.py` y la clase `Vidrio(BrowserSetup)` con su entrada en la tabla de productos de `tools/wpt/run.py` (igual que Ladybird).
- Subconjunto WebDriver (W3C WebDriver clásico, JSON sobre HTTP/1.1):
  - `GET /status`; `POST /session`; `DELETE /session/{id}`
  - `GET|POST /session/{id}/timeouts`
  - `POST|GET /session/{id}/url`; `GET /session/{id}/title`; `POST /session/{id}/back|forward|refresh`
  - `GET|POST|DELETE /session/{id}/window`; `GET /session/{id}/window/handles`; `POST /session/{id}/window/new`; `GET|POST /session/{id}/window/rect`
  - `POST /session/{id}/frame`; `POST /session/{id}/frame/parent`
  - `POST /session/{id}/execute/sync`; `POST /session/{id}/execute/async` (serialización JSON de WebDriver, con referencias a elementos y ventanas)
  - `GET /session/{id}/screenshot`
  - `POST|DELETE /session/{id}/actions` (pausa, puntero y teclado básicos)
  - `POST /session/{id}/element`; `POST /session/{id}/element/{eid}/click`; `POST /session/{id}/element/{eid}/value`
  - `GET|DELETE /session/{id}/cookie`
  - Cualquier otro comando responde `unknown command` (404). Fuera: BiDi, impresión, permisos, WebAuthn.

## 5. Medición

### 5.1 Revisiones fijadas (`bench/pins.json`)

| Pieza | Valor fijado |
| --- | --- |
| web-platform-tests | `77787fe5fe2339a45c740638c5a65345911e80bf` |
| test262 | `9ae074624e2de0ac79e5e29945d2523c69f8582f` |
| Firefox | 159.0.1 (paquete Arch `firefox 159.0.1-1`) |
| geckodriver | 0.37.1 (paquete Arch `geckodriver 0.37.1-1`) |
| Chromium y chromedriver | 156.0.8078.79 (paquete Arch `chromium 156.0.8078.79-1`) |
| Servo | 0.8.0, binario oficial `servo-x86_64-linux-gnu.tar.gz`, sha256 `02a78892746e69bc39d471da2bb012fd8ce228e464cda5a017334d0e88c61de1`; `servoshell --version` imprime `Servo 0.8.0-4e136d8ef` |
| Boa | 0.22.0, binario oficial `boa-x86_64-unknown-linux-gnu` de v0.22, sha256 `d3484a63c9ee5f025de11379d055285a0bc015cf6d1d45b895e0d0f93566d671`; `boa --version` imprime `boa 0.22.0` |
| Node (corredor de control de test262) | 26.13.0 (paquete Arch `nodejs 26.13.0-1`) |
| Xvfb | 21.1.25 (paquete Arch `xorg-server-xvfb 21.1.25-2`) |
| Datos | Unicode 17.0.0 (`https://www.unicode.org/Public/17.0.0/`), WHATWG Encoding `be17ac5c49881336538acfc7b40dba3c8009144b` |

Los árboles y binarios se guardan en `bench/cache/` (ignorada por git). La medición no usa red externa.

### 5.2 Alcance

WPT (26 directorios, en `bench/scope.json`): `html/syntax/parsing`, `html/syntax/serializing-html-fragments`, `dom`, `encoding`, `url`, `html/semantics/scripting-1`, `html/webappapis`, `css/css-syntax`, `css/css-values`, `css/css-variables`, `css/selectors`, `css/css-cascade`, `css/cssom`, `css/cssom-view`, `css/CSS2`, `css/css-display`, `css/css-box`, `css/css-sizing`, `css/css-position`, `css/css-flexbox`, `css/css-grid`, `css/css-tables`, `css/css-text`, `css/css-fonts`, `css/css-backgrounds`, `css/css-color`.

- `html/syntax/serializing` no existe en la revisión fijada (hecho, verificado en el árbol); su sucesor registrado es `html/syntax/serializing-html-fragments`.
- `third_party/test262` de WPT se corre solo para el criterio de JS dentro del navegador; no entra en R.

test262: `test/language`, `test/built-ins`, `test/annexB`. Fuera: `test/intl402`, `test/staging`.

### 5.3 Fórmulas

- Unidad WPT: cada subtest con estado `PASS` cuenta 1; una prueba sin subtests cuenta como una unidad que pasa si su estado es `PASS` u `OK`.
- `R(D) = unidades PASS de vidrio en D / unidades PASS de Firefox en D`. `R(total) = Σ PASS de vidrio / Σ PASS de Firefox` sobre todo el alcance (cociente de sumas, no media de cocientes). Igual contra Chromium, y `R_servo(D)` = Servo contra Firefox.
- `P = escenarios que pasan / escenarios corridos` en test262, con la expansión estricto/no estricto de INTERPRETING.md.
- Pisos: `R(total) ≥ R_servo(total)`, ningún `R(D) = 0`, `P ≥ P_boa`. Los valores salen de la medición en la revisión fijada (K1).
- Conclusión permitida: **comparable** (misma pregunta y mismo arnés; los motores tienen garantías distintas). No se concluye nada de velocidad.

### 5.4 Corredor de test262 (`bench/test262.py`)

Propio, en Python con biblioteca estándar, el mismo para todos los motores. Aplica INTERPRETING.md: `assert.js` y `sta.js` más `includes`; `onlyStrict`, `noStrict`, `module`, `raw`, `async` (`Test262:AsyncTestComplete` y `doneprintHandle.js`), `CanBlockIsFalse`/`CanBlockIsTrue` (el hilo principal de los tres motores puede bloquear: las pruebas `CanBlockIsFalse` no corren y las `CanBlockIsTrue` sí), y negativos por fase. La fase se comprueba igual para todos: un negativo de `parse` o `resolution` que llega a ejecutar `$DONOTEVALUATE()` falla. Adaptadores por motor solo para invocar y extraer el nombre del error:

- Node: `node --experimental-vm-modules bench/test262/node_host.js` (implementa `$262` con `vm`).
- Boa: `boa --test262-object [--strict] [-m]`.
- vjs: `vjs --test262 [--strict] [--module]`.

Tiempo límite de 10 s por escenario (excedido = falla), 10 hilos. Validación: un conjunto de control en `bench/test262/control/` (estricta, no estricta, módulo, async, negativa de parseo, negativa de ejecución) con resultados esperados sobre Node.

### 5.5 Configuración de corrida WPT (`bench/wptrun.py`)

- Un navegador a la vez, en serie, un `./wpt run` por directorio del alcance, con `--processes=4`, `--headless`, `--log-wptreport`, `--log-raw`, `--no-manifest-update` y el manifiesto generado una vez en la revisión fijada.
- Hosts sin sudo: Firefox con la preferencia `network.dns.localDomains` (la pone wptrunner), Chromium con `--host-resolver-rules` (la pone wptrunner), Servo y vidrio con `HOST_FILE`.
- Firefox recibe `--prefs-root` con los perfiles descargados una vez por `bench/fetch.py`.
- **Sin red externa**: cada corrida se ejecuta dentro de `unshare -rn sh -c 'ip link set lo up && exec unshare --map-user=<uid> --map-group=<gid> …'` (espacio de red propio con solo loopback; sin sudo; verificado en esta PC).
- **Memoria**: cada corrida corre en `systemd-run --user --scope` y guarda el `memory.peak` de su cgroup. Límite: 10 GiB (10737418240 bytes) por corrida.
- Si un binario de referencia falta (su instalación requiere sudo), el script sale con 77.

### 5.6 Interfaz del arnés (la leen los checkers; no cambia sin cambiar este contrato)

| Comando | Efecto y salida |
| --- | --- |
| `python3 bench/fetch.py` | Descarga (con red) WPT y test262 en sus commits a `bench/cache/wpt` y `bench/cache/test262` (fetch superficial del commit), Servo a `bench/cache/servo/servoshell`, Boa a `bench/cache/boa/boa` (verificando sha256), crea el entorno de `./wpt`, genera `bench/cache/wpt/MANIFEST.json`, guarda los prefs de Firefox en `bench/cache/firefox-prefs` y aplica `bench/wpt/vidrio.patch`. Idempotente. |
| `python3 bench/pins_check.py` | Imprime `OK` y sale 0 si los árboles están en sus commits y cada binario reporta su versión fijada; si no, imprime el motivo y sale 1 (77 si falta un binario de paquete). |
| `python3 bench/scope_check.py` | `OK` si cada directorio del alcance (o su sucesor registrado) tiene pruebas en `MANIFEST.json`. |
| `python3 bench/deps.py ldd BIN…` | `OK` si las `NEEDED` del ELF (recursivas vía `ldd`) están en la lista permitida. |
| `python3 bench/deps.py forbidden` | `OK` si ningún nombre prohibido aparece como dependencia en `Cargo.toml`, `Cargo.lock`, `#[link(name=…)]`, `extern crate` o directivas `cargo:rustc-link-lib`. |
| `python3 bench/wptrun.py --product P (--all \| --dir D… \| --path T…) [--run N] [--force]` | `P ∈ {firefox, chromium, servo, vidrio}`. Escribe por directorio `bench/results/wpt/P/KEY/SLUG.json` (wptreport), `SLUG.log` (log crudo mozlog) y `SLUG.mem` (bytes de `memory.peak`); para vidrio también `SLUG.shots` (`--log-wptscreenshot`, capturas de reftests). `SLUG` = directorio con `/` → `__`. `KEY` = versión fijada para referencias, o los primeros 16 hex del sha256 de `target/release/vidrio`, seguido de `-` y los 12 primeros del commit de WPT, y `-rN` si `--run N` con N>1. Si el archivo ya existe y no hay `--force`, no vuelve a correr. |
| `python3 bench/wptrun.py --net-probe` | Dentro del mismo aislamiento de red imprime `loopback=ok|fail` y `external=blocked|reachable`. |
| `python3 bench/test262.py --engine E [--control]` | `E ∈ {node, boa, vjs}`. Escribe `bench/results/test262/E/KEY.json` (`KEY` = versión fijada o sha256[:16] de `target/release/vjs`), o `bench/results/test262/control-E.json` con `--control`. Esquema: `{"engine", "engine_version", "test262_commit", "excluded_features": [...], "scenarios": [{"file", "mode": "strict|sloppy|module|raw", "result": "pass|fail", "detail"}]}`; `file` es relativo a la raíz de test262 (`test/...`). Si el archivo ya existe y no hay `--force`, no vuelve a correr. El conjunto de control son doce archivos `bench/test262/control/<tipo>-<pass\|fail>.js` con `tipo ∈ {strict, nostrict, module, async, negparse, negruntime}`: los `-pass` deben pasar y los `-fail` deben fallar. |
| `python3 bench/ratio.py` | Lee los reportes y escribe `bench/results/ratio.json`: `{"vs_firefox": {D: R, …, "total": R}, "vs_chromium": {…}, "servo_vs_firefox": {…}, "P": …, "P_boa": …}`. |
| `python3 bench/stability.py --product vidrio --dir D` | Compara las corridas `-r1` y `-r2` de D y lista las diferencias por subtest. |

Archivos versionados del arnés: `bench/pins.json`, `bench/scope.json`, `bench/flaky.json`, `bench/excluded.json`, `bench/wpt/vidrio.patch`, `bench/test262/node_host.js`, `bench/test262/control/`.

### 5.7 Listas publicadas

- `bench/excluded.json`: pruebas excluidas porque fallan igual en Firefox y Chromium por configuración de la PC. Cada entrada lleva la prueba, el estado observado en ambos y la causa. Se quitan del conteo de todos los productos por igual. Empieza vacía. Las pruebas `https` nunca se excluyen por falta de TLS.
- `bench/flaky.json`: pruebas inestables. Una entrada solo entra con dos corridas del mismo binario y la misma configuración que difieren en esa prueba, con ambos estados anotados; no puede superar el 0.5% de las pruebas del directorio de estabilidad (`dom`). Las inestables no se excluyen de R.
- Formatos: `excluded.json` = `{"tests": [{"test": "/ruta", "firefox": "ESTADO", "chromium": "ESTADO", "cause": "…"}]}`; `flaky.json` = `{"tests": [{"test": "/ruta", "subtest": "nombre o null", "statuses": ["ESTADO1", "ESTADO2"]}]}`. `test` es la URL del wptreport.

### 5.8 Prueba del primer cuadro

`tests/first-frame/page.html` es una página fija hecha solo de cajas de color sólido (sin texto ni imágenes); `tests/first-frame/expected.png` (800×600, RGB de 8 bits, sin entrelazado) la genera `tests/first-frame/make_expected.py` a partir de la misma lista de rectángulos, sin usar vidrio. `vidrio file:///ruta/page.html` crea su ventana en (0,0) con 800×600. El checker arranca Xvfb, toma con libxcb la imagen de la ventana raíz antes y después de lanzar vidrio, y compara al píxel el primer cuadro que difiere del fondo contra `expected.png`.

## 6. Comando de chequeo y CI

`make check` es el comando único: formato (`cargo fmt --all --check`), tipos y tests (`cargo test --workspace --locked`), tests del arnés (`python3 -m unittest discover -s bench -p 'test_*.py'`) y dependencias prohibidas (`python3 bench/deps.py forbidden`). Cada paso corre solo si lo que revisa ya existe, así sale con 0 desde K0. El CI (`.github/workflows/ci.yml`, Ubuntu 24.04 con `libxcb1-dev` y `libxcb-shm0-dev`) corre el mismo comando. La medición no corre en CI: corre en la PC.

## 7. Decisiones cerradas

Cada decisión en una línea y su razón en la siguiente.

**D-01. Lenguaje del navegador y del motor JS: Rust 1.98.1 sin crates de terceros, con `libgcc_eh.a` estático en lugar de `libgcc_s.so`.**
Razón: seguridad de memoria en cientos de miles de líneas escritas por agentes, ya instalado en la PC, y se verificó que con el atajo de `build.rs` el ELF solo pide `libc.so.6` y `ld-linux` (más libxcb), dentro de la lista permitida.

**D-02. Arquitectura interna: un proceso; hilo principal para bucle de eventos, DOM, JS, estilo, layout y pintado; hilos auxiliares solo para WebDriver y red; DOM en arena por documento; estilos computados agrupados y compartidos por `Rc`; árbol de cajas → árbol de fragmentos → lista de pintado plana por Apéndice E.**
Razón: es la forma más simple que cubre todo el alcance, cabe holgada en 16 GB y evita sincronización entre hilos en el núcleo; el aislamiento por procesos no lo mide ninguna suite del alcance.

**D-03. Motor JS: intérprete de bytecode de pila con shapes, sin JIT; GC mark-sweep preciso, no movible, en puntos seguros; un heap por proceso compartido con el DOM mediante wrappers únicos enraizados por documento.**
Razón: el piso es de conformidad (test262), no de velocidad; correr el GC solo en puntos seguros elimina el enraizado manual en código nativo, que es la fuente principal de errores en motores escritos a mano.

**D-04. Integración con el arnés: servidor WebDriver propio dentro del binario (como Ladybird y Servo) con los ejecutores WebDriver genéricos de wptrunner y el subconjunto de §4.**
Razón: los ejecutores genéricos ya existen y no hay que mantener un ejecutor propio dentro de wptrunner; el parche queda en cuatro archivos de registro del producto.

**D-05. Orden de fases: K1 arnés y referencias; K2 motor JS (test262 mide); K3 red, documento, DOM y WebDriver (testharness de parsing y DOM miden) y cierre del piso test262; K4 CSS, layout, texto, pintado y ventana (reftests miden); K5 conformidad y pisos WPT.**
Razón: respeta que test262 y los testharness de parsing y DOM midan antes que los reftests, y cada frontera de K es un punto de medición que los mini sprints siguientes usan para dirigir el trabajo.

**D-06. Revisiones fijadas: WPT `77787fe5fe23`, test262 `9ae074624e2d`, Firefox 159.0.1, geckodriver 0.37.1, Chromium 156.0.8078.79, Servo 0.8.0, Boa 0.22.0 (§5.1).**
Razón: son las cabezas y versiones vigentes el 2026-11-09, verificadas con `git ls-remote`, la API de releases de GitHub, `pacman -Si` y ejecutando `--version` de Servo y Boa en esta PC.

**D-07. Hosts de prueba: Firefox por `network.dns.localDomains`, Chromium por `--host-resolver-rules`, Servo y vidrio por `HOST_FILE`; nunca `/etc/hosts`.**
Razón: los tres mecanismos están en el código de wptrunner de la revisión fijada (`firefox.py` línea 829, `chrome.py` líneas 132–137, `servo.py` línea 93), así que ninguna medición necesita sudo para resolver nombres.

**D-08. Servo se descarga como binario oficial fijado por sha256; no se compila.**
Razón: el release v0.8.0 publica `servo-x86_64-linux-gnu.tar.gz`, que corre en esta PC sin bibliotecas faltantes; compilar Servo cuesta horas y mucha RAM sin cambiar la medición.

**D-09. Corredor de test262: propio en Python (`bench/test262.py`), validado contra Node con un conjunto de control y corrido igual sobre Boa y vjs.**
Razón: `test262-harness` está sin publicar desde 2022 y depende de eshost, que no conoce vjs; el tipo `test262` de wptrunner solo cubre el subconjunto vendorizado en WPT.

**D-10. Features excluidas de test262: exactamente las listadas bajo `## Proposed language features` en `features.txt` del commit fijado (propuestas en etapa 3), las mismas para todos los motores.**
Razón: es una regla mecánica, sin elección a mano; todo lo estándar (incluido Temporal) cuenta.

**D-11. Procesos y memoria: `--processes=4` para todos los navegadores; límite de 10 GiB de `memory.peak` de cgroup por corrida de directorio. Si una referencia lo supera en K1, se baja a 2 para todos y se anota en `bench/pins.json`.**
Razón: la PC tiene 15.6 GB (MemTotal medido) y 12 núcleos; 10 GiB deja margen para el sistema, y la misma cantidad de procesos para todos es requisito de comparación.

**D-12. Pisos numéricos: `R_servo(total)` y `P_boa` no se fijan a mano; los calcula `bench/ratio.py` sobre las corridas de K1 y quedan en `bench/results/ratio.json`; README los publica.**
Razón: la idea exige que el número salga de la medición en la misma PC y revisión.

**D-13. Pruebas inestables: lista `bench/flaky.json` con la regla de §5.7 (dos corridas que difieren, estados anotados, tope de 0.5% del directorio, sin exclusión de R).**
Razón: el tope y la evidencia impiden que la lista convierta el criterio de estabilidad en trivial.

**D-14. Ventana: X11 con libxcb y MIT-SHM; la medición WPT corre con `--headless` (sin X) y la prueba de primer cuadro corre bajo Xvfb.**
Razón: libxcb es la única biblioteca de ventana permitida; en modo headless la captura sale del framebuffer propio, así que WPT no depende de un servidor X.

**D-15. Fuentes: se leen de `/usr/share/fonts` sin fontconfig. Por omisión `serif` = Liberation Serif, `sans-serif` = Liberation Sans, `monospace` = Liberation Mono (`system-ui`, `cursive` y `fantasy` → Liberation Sans); respaldo de glifos con las fuentes Noto instaladas; 16 px (13 px en monospace).**
Razón: Liberation está instalada en esta PC y es compatible en métricas con Times, Arial y Courier; los reftests usan Ahem servida por WPT.

**D-16. Red externa bloqueada con `unshare -rn` + loopback + vuelta al uid real; no requiere sudo.**
Razón: se probó en esta PC: loopback funciona y la conexión a 1.1.1.1:443 falla con `ENETUNREACH`.

**D-17. No se agregan al alcance css-transforms, css-animations, css-transitions ni css-writing-modes.**
Razón: no los pide la idea, y cada directorio añadido suma trabajo sin acercar ningún criterio de éxito.

**D-18. Huecos de la investigación cerrados: (a) los espacios de nombres de usuario sin privilegio están activos en esta PC (`max_user_namespaces` = 60863 y `unshare -rn` funciona), así que Chromium usa su sandbox normal; si falla dentro del espacio de red, se corre con `--no-sandbox` y se anota en LIMITATIONS; (b) los hosts se resuelven como en D-07; (c) la RAM por corrida se mide con `memory.peak` (D-11) en K1.**
Razón: (a) y (b) se verificaron en esta PC y en el código fijado; (c) solo se puede medir con los navegadores instalados.

**D-19. Árbol de datos de terceros: tablas Unicode 17.0.0, índices WHATWG Encoding y tabla IDNA se convierten con generadores en Python (biblioteca estándar) a archivos Rust generados y versionados; los datos crudos se bajan a `bench/cache/data/` y no se versionan.**
Razón: son datos de especificación, no código; versionar solo la salida mantiene el repo pequeño y la generación reproducible.

**D-20. Licencia BSD-2-Clause; repositorio público.**
Razón: la idea pide código abierto; es la licencia de Ladybird, la referencia más cercana de motor independiente.

## 8. Contradicciones resueltas

- La investigación lista solo `html/webappapis/timers`, `microtask-queuing` y `scripting/event-loops`; la idea pide `html/webappapis`. Gana la idea (producto): entra `html/webappapis` completo.
- La idea nombra `html/syntax/serializing`, que no existe en la revisión fijada; se usa su sucesor `html/syntax/serializing-html-fragments` (la idea misma prevé sucesores registrados).
- La idea dice que los criterios con sudo salen con 77; con D-07 y D-16 la medición no necesita sudo. El 77 queda solo para binarios de referencia no instalados, cuya instalación sí requiere sudo.
- El método pide verificación solo al inicio y cierre de cada K, y los mini sprints no corren pruebas. Por eso las mediciones largas (WPT y test262) las ejecutan los checkers de cada K, con caché por versión o binario, y los mini sprints de conformidad leen los resultados del cierre anterior en `bench/results/`.
