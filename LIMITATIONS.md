# Límites de vidrio

Fuente única de lo que el proyecto no demuestra. Ningún otro archivo los repite.

## Alcance

- Solo se mide el núcleo del motor: 26 directorios de web-platform-tests (ver `vault/CONTRACT.md` §5.2) y `test/language`, `test/built-ins` y `test/annexB` de test262. Medios, WebRTC, WebGL, WebGPU, Web Audio, WebAssembly, service workers, almacenamiento, `css-transforms`, `css-animations`, `css-transitions`, `css-writing-modes` y `test/intl402` quedan fuera, así que la cifra no dice nada de ellos.
- Las propuestas de test262 en etapa 3 (sección `Proposed language features` de `features.txt`) se excluyen para todos los motores por igual.
- Solo corre en Arch Linux x86_64 con X11. No hay soporte de Wayland nativo, de otras distribuciones ni de otras arquitecturas.

## Medición

- Las comparaciones son de conformidad en la misma PC y revisión; no prueban nada de velocidad, consumo ni seguridad.
- El piso WPT se mide contra Servo y el de test262 contra Boa en versiones fijadas; con otras versiones las cifras cambian.
- Servir para navegar páginas reales fuera de las suites no se mide; solo se registra una revisión humana.
- D-07 dice «nunca `/etc/hosts`», pero el `./wpt run` de la revisión fijada se niega a arrancar `chromium` si `/etc/hosts` no tiene las entradas de `./wpt make-hosts-file` (su `check_environ` exceptúa `chrome`, `firefox` y `servo`, no `chromium`). Por eso la corrida de Chromium monta, en un espacio de montaje propio y sin sudo, una copia de `/etc/hosts` con esas entradas (`bench/wptrun.py`). El `/etc/hosts` real no cambia y Chromium sigue resolviendo los hosts de prueba con `--host-resolver-rules`, así que ningún resultado cambia.
- El paquete `firefox` de Arch no incluye `chrome://remote/content/marionette/reftest-chrome/reftest.xhtml`, que usa el corredor interno de reftests de Marionette (sin él, cada reftest termina en `CRASH`). Firefox corre sus reftests con `--reftest-external`, que compara capturas de WebDriver como en Chromium y Servo; el R de Firefox en directorios con reftests puede diferir algo del que daría el corredor interno de las compilaciones de Mozilla.
- `R(total)` es un cociente de sumas y `encoding` pesa casi todo. En la medición de K1, `encoding` aporta 1 152 129 de las 1 337 421 unidades PASS de Firefox (86.1 %) y 1 151 832 de las 1 319 965 de Servo (`bench/results/ratio.json`). Por eso `R(total)` dice poco de los otros 25 directorios; para ellos cuentan los `R(D)` por directorio.
- Las cifras de referencia (Firefox, Chromium, Servo y Boa) salen de una sola corrida por directorio. No se midió cuánto varían entre corridas, así que `R_servo` y `P_boa` no tienen margen de variación medido.

## Producto

- No hay aislamiento por procesos ni sandbox: un solo proceso ejecuta todo.
- La criptografía de TLS es propia y no se afirma que sea de tiempo constante ni resistente a canales laterales; sirve para cargar las pruebas `https` del servidor de WPT, no para navegar con seguridad.
- Sin JIT: el motor JS es un intérprete.
- El recolector de vjs solo corre en los puntos seguros del bucle de intérprete más externo (el que abre la evaluación de un script) y entre trabajos. El código JS que corre dentro de una llamada nativa (callbacks de built-ins, trabajos de promesas, llamadas del host a una función) no recolecta hasta volver a ese bucle.
- vjs no implementa llamadas de cola (feature `tail-call-optimization` de test262): cada llamada ocupa un marco, y pasados 10 000 marcos se lanza `RangeError`.
- El texto que recibe `eval`, y los parámetros y el cuerpo que reciben `Function`, `GeneratorFunction`, `AsyncFunction` y `AsyncGeneratorFunction`, se pasan al parser como UTF-8: un sustituto UTF-16 suelto dentro de ese texto se convierte en U+FFFD antes de analizarlo (y así queda en el texto que devuelve `Function.prototype.toString`).
- En vjs, la expresión `extends` de una clase ve los nombres privados de la propia clase; la especificación la evalúa con el entorno privado exterior.
- En vjs, una referencia a un enlace declarativo que solo se resuelve por nombre (con `with` o `eval` directo no estricto de por medio) se vuelve a resolver al asignar, en vez de fijarse antes de evaluar el lado derecho; solo se nota si el lado derecho borra un `var` creado por `eval`.
- En vjs, los Script Records y Module Records viven mientras viva el agente: el estado de módulos los enraíza todos, como un mapa de módulos que nunca se vacía.
- `$262.agent.broadcast` del binario `vjs` espera solo a los agentes que siguen vivos; un agente que termina su script sin llamar a `receiveBroadcast` sale y no recibe mensajes.
- Los BigInt de vjs tienen un tope de 2^30 bits (`MAX_BIGINT_BITS`): una operación cuyo resultado lo pasaría (`**`, `<<`, `BigInt.asUintN` de un negativo con muchos bits) lanza `RangeError`. La multiplicación y la conversión a cadena son cuadráticas, así que operar cerca del tope es lento.
- `Math.random` de vjs usa xorshift64* sembrado con el reloj; no es criptográfico.
- vjs no tiene datos de locale: `toLocaleLowerCase` y `toLocaleUpperCase` ignoran el locale (sin las reglas de turco, azerí y lituano de `SpecialCasing.txt`), y `localeCompare` compara por unidades de código las formas NFC de las dos cadenas, no con el orden de cotejo de Unicode (UCA).
