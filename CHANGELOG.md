# Changelog

Cambios y decisiones de vidrio por versión, en términos del producto.

## [Sin publicar]

### K3 — Red, documento, DOM, bucle de eventos y WebDriver

#### Añadido

- **Red:** cliente HTTP/1.1 con conexiones persistentes, contenido gzip y deflate, y resolución de hosts por `HOST_FILE`. Cliente TLS 1.3 con criptografía propia y validación X.509 contra el almacén del sistema o un certificado dado con `--ca-cert`.
- **Encoding, URL y MIME:** las 40 codificaciones de WHATWG Encoding, el parser de URL con IDNA y el análisis de tipos MIME.
- **HTML y DOM:** parser HTML con todos los modos de inserción y `document.write`, serialización de fragmentos, el DOM con sus algoritmos de mutación, eventos, `MutationObserver`, rangos, recorridos, shadow DOM con slots, y los selectores de nivel 4 para `matches`, `closest` y `querySelector`.
- **Bindings:** generador propio de bindings Web IDL que conecta el DOM con `vjs`, con un wrapper por nodo.
- **Bucle de eventos:** tareas, microtareas, temporizadores, `requestAnimationFrame`, navegación, `iframe`, `window.open`, scripts clásicos y módulos con import maps, `postMessage` y `MessageChannel`, y clonado estructurado.
- **APIs de datos:** `fetch` con CORS, redirecciones y cookies, `XMLHttpRequest`, `Blob`, `File`, `FileReader`, `FormData`, URLs `blob:`, `URL`, `TextEncoder`, `TextDecoder`, `DOMParser` y `XMLSerializer` con un parser XML propio.
- **WebDriver y línea de comandos:** `vidrio --webdriver=PUERTO [--headless] [--ca-cert=RUTA] [URL]` expone el subconjunto WebDriver del contrato. Sin `--webdriver`, carga la URL y sale.

#### Medido

- **test262:** `vjs` pasa 91 956 de 92 012 escenarios (`P` = 0.9994), contra 90 598 de Boa 0.22.0 (`P_boa` = 0.9846). El piso `P ≥ P_boa` se cumple, y ningún escenario que pasaba en K2 falla.
- **web-platform-tests, directorios de documento:** `R(D)` contra Firefox de 0.3015 en `html/syntax/parsing`, 1.0483 en `html/syntax/serializing-html-fragments`, 0.7040 en `dom`, 0.0104 en `encoding`, 0.5163 en `url`, 0.3562 en `html/semantics/scripting-1` y 0.5841 en `html/webappapis`. Ninguno es cero.
- **HTTPS, crashtests y test262 en el navegador:** 11 de 25 pruebas `https` terminan en `OK` por el TLS propio, un crashtest da `PASS`, y vidrio pasa 49 455 subtests de `third_party/test262` de WPT.
- **Memoria:** cada corrida quedó bajo el límite de 10 GiB; el pico más alto fue 4 999 114 752 B, en `encoding`.

#### Corregido

- **vjs:**
  - Cadenas de Temporal: se rechaza el signo menos U+2212 y se ignoran las anotaciones de calendario donde corresponde.
  - `caller` en funciones no estrictas, `byteLength` y `byteOffset` de `DataView` fuera de rango, el límite de `Iterator.prototype.take` y `drop`, y `Promise.try`.
  - Asignaciones dentro de `with` cuyo lado derecho cambia el enlace, y el orden de `super[…]`.
  - Las referencias de los Script Records ya no mantienen vivo el realm de una ventana cerrada.
- **vidrio:**
  - El decodificador conserva la codificación elegida por el BOM al terminar el flujo.
  - `<meta>` expone sus atributos reflejados.
  - Los comandos WebDriver ya no esperan al siguiente temporizador de la página.
  - El módulo de producto del arnés sirve las pruebas en `127.0.0.1`.

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
