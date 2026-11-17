# Changelog

Cambios y decisiones de vidrio por versión, en términos del producto.

## [Sin publicar]

### K2 — Motor JavaScript `vjs` medido con test262

#### Añadido

- **Parser:** analizador léxico y sintáctico de ECMAScript con errores tempranos, scripts y módulos.
- **Motor:** heap con recolector mark-sweep, modelo de objetos, compilador a bytecode e intérprete, módulos y llamadas de cola en funciones estrictas.
- **Built-ins:** `Object`, `Function`, errores, `Symbol`, `Reflect`, `Proxy`, `Number`, `Math`, `BigInt`, `String` con tablas de Unicode 17.0.0, `RegExp` con motor propio, `Array`, ayudantes de iteradores, colecciones con clave y referencias débiles, `ArrayBuffer`, TypedArrays, `DataView`, `Atomics`, `Promise`, `JSON`, `Date`, Temporal (con lectura de TZif de la PC), `DisposableStack` y `AsyncDisposableStack`, y lo del Anexo B (funciones en bloques, `escape`, `unescape`, métodos HTML de `String` y `getYear` y `setYear` de `Date`).
- **Shell `vjs`:** ejecuta scripts y módulos (`--strict`, `--module`) y, con `--test262`, instala `$262` con agentes para el corredor de test262.

#### Medido

- **test262:** `vjs` pasa 91 753 de 92 012 escenarios (`P` = 0.9972), contra 90 598 de Boa 0.22.0 (`P_boa` = 0.9846), con el mismo corredor, la misma lista de exclusión y la misma revisión `9ae074624e2d`.
- **Por área:** `test/language` 43 738 de 43 802, `test/built-ins` 46 698 de 46 885 y `test/annexB` 1 317 de 1 325. Ninguna de las 90 áreas de segundo nivel de `test/language` y `test/built-ins` queda en cero, y ningún escenario falló por tiempo límite.
- **Dependencias:** el binario `vjs` solo enlaza bibliotecas permitidas.

#### Corregido

- `let`, `const` o `class` en el nivel superior de un `eval` ya no provocan un pánico.
- Las funciones en bloques del Anexo B.3.2 crean su enlace `var` también en el código global.
- El redondeo de Temporal con signo negativo redondea el valor con signo, no su magnitud.

### K1 — Arnés de medición y referencias

#### Añadido

- **Esqueleto:** los cuatro componentes de vidrio (motor JS, red, gráficos y navegador) y los binarios `vidrio` y `vjs`, que por ahora solo responden a `--version`. Los binarios solo enlazan bibliotecas permitidas y el proyecto no tiene ninguna dependencia prohibida.
- **Revisiones fijadas:** WPT, test262, Firefox, geckodriver, Chromium, chromedriver, Servo, Boa, Node y Xvfb, con descarga local, comprobación de versiones y comprobación de que los 26 directorios del alcance tienen pruebas.
- **Corredor de test262:** propio, igual para Node, Boa y `vjs`, y validado contra Node con doce archivos de control.
- **Corredor de web-platform-tests:** sin red externa y sin sudo, con el pico de memoria registrado por corrida y el módulo de producto `vidrio` para el arnés oficial `./wpt run`.
- **Cálculo de proporciones y estabilidad:** `R`, `R_servo`, `P` y `P_boa`, y comparación de dos corridas contra la lista de pruebas inestables.

#### Medido

- **Piso de web-platform-tests:** `R_servo(total)` = 0.9869 (Servo 0.8.0 contra Firefox 159.0.1, 26 directorios).
- **Piso de test262:** `P_boa` = 0.9846 (Boa 0.22.0: 90 598 de 92 012 escenarios).
- **Memoria:** las tres referencias corrieron los 26 directorios bajo el límite de 10 GiB por corrida; el pico más alto fue 8 883 073 024 B, en Chromium.

#### Cambiado

- **Hosts de prueba de Chromium:** como el arnés fijado exige que los hosts de prueba estén en `/etc/hosts`, Chromium corre con una copia privada de ese archivo; el archivo real no cambia.
- **Reftests de Firefox:** usan el comparador externo por capturas, porque el paquete de Arch no trae el corredor interno.

### Decidido

- Rust sin crates de terceros para el navegador y el motor JS; el runtime de GCC se enlaza estático para que el binario solo dependa de glibc y libxcb.
- Un solo proceso con DOM en arena, motor JS de bytecode con recolector mark-sweep compartido con el DOM, y lista de pintado propia rasterizada por software.
- Integración con web-platform-tests mediante un servidor WebDriver propio y los ejecutores WebDriver genéricos de wptrunner.
- Revisiones fijadas: web-platform-tests `77787fe5fe23`, test262 `9ae074624e2d`, Firefox 159.0.1, Chromium 156.0.8078.79, Servo 0.8.0, Boa 0.22.0.
- Medición sin red externa y sin sudo, con un espacio de red propio por corrida.
