# Investigación de vidrio: fuentes primarias y revisiones fijadas

Consultas del 2026-11-09. Cada entrada dice qué se tomó de la fuente y cómo se verificó. "Ejecutado" = se corrió la herramienta en la PC del proyecto (Arch Linux, kernel 7.2.14, 12 núcleos, MemTotal 15,635,076 kB).

## 1. Suites y arneses

| # | Fuente | Revisión fijada | Qué se usa | Verificación |
| --- | --- | --- | --- | --- |
| 1 | web-platform-tests, https://github.com/web-platform-tests/wpt | `77787fe5fe2339a45c740638c5a65345911e80bf` (master, 2026-11-09T23:19:36Z) | Pruebas del alcance, `./wpt run`, `wpt manifest`, wptrunner | `git ls-remote`; archivos leídos en esa revisión con la API de GitHub |
| 2 | wptrunner, `tools/wptrunner/wptrunner/browsers/ladybird.py` | ídem | Modelo del módulo de producto: `WebDriverBrowser`, ejecutores WebDriver genéricos para testharness, reftest, crashtest y test262; arranque `[webdriver_binary, "--port", port]` | Leído |
| 3 | wptrunner, `browsers/servo.py` | ídem | `HOST_FILE` con archivo temporal de hosts (línea 93), `--webdriver=<puerto>`, `--certificate-path`, `--headless` | Leído |
| 4 | wptrunner, `browsers/firefox.py` | ídem | `network.dns.localDomains` (línea 829); `prefs_root` | Leído |
| 5 | wptrunner, `browsers/chrome.py` y `chromium.py` | ídem | `--host-resolver-rules="MAP *.test 127.0.0.1, …"` (líneas 132–137); `--headless=new`; chromium reutiliza chrome | Leído |
| 6 | wptrunner, `products.py` | ídem | Productos integrados en `BUILTIN_PRODUCTS`; carga `wptrunner.browsers.<nombre>` | Leído |
| 7 | `tools/wpt/run.py` y `tools/wpt/browser.py` | ídem | Clase `BrowserSetup` por producto y tabla de productos; Firefox llama `install_prefs` (descarga) si falta `--prefs-root` y usa caché si existe `profiles.json` | Leído |
| 8 | wptrunner, `executors/executorwebdriver.py` | ídem | Comandos WebDriver usados: url, ventana actual, handles, ventana nueva, cerrar ventana, execute sync/async, timeouts, actions release, window rect, screenshot | Leído |
| 8b | Captura de la ventana raíz con libxcb (`xcb_get_image`, ZPixmap) | libxcb 1.17.0 | Ejecutado desde Python con ctypes: conexión, setup y petición funcionan; en XWayland sin raíz la raíz responde BadMatch (código 8), por eso la prueba de primer cuadro usa Xvfb |
| 9 | wptrunner, `wptcommandline.py` | ídem | `--processes`, `--binary`, `--webdriver-binary`, `--headless`, `--no-manifest-update`, `--prefs-root`, `--include-file`, `--run-by-dir`; formateadores `wptreport` y `wptscreenshot` registrados como `--log-wptreport` | Leído |
| 10 | Documentación de WPT, https://web-platform-tests.org/running-tests/from-local-system.html | vigente | `./wpt run`, `--log-wptreport`, `--processes`, hosts con `./wpt make-hosts-file` | Consultada (investigación adjunta) |
| 11 | WPT, `tools/certs/cacert.pem` | ídem | CA de prueba: RSA 2048, `sha256WithRSAEncryption` → vidrio necesita RSA (PKCS#1 v1.5 y PSS) en TLS | Leído con `openssl x509 -text` |
| 12 | WPT, `fonts/Ahem.ttf` | ídem | Fuente de los reftests (TrueType) | Listado |
| 13 | WPT, `html/syntax/` | ídem | Existen `parsing`, `serializing-html-fragments`, `serializing-xml-fragments`; **no** existe `serializing` | Listado |
| 14 | test262, https://github.com/tc39/test262 | `9ae074624e2de0ac79e5e29945d2523c69f8582f` (main, 2026-11-10T00:05:51Z UTC) | `test/language`, `test/built-ins`, `test/annexB`, `harness/` | `git ls-remote` |
| 15 | test262, `INTERPRETING.md` | ídem | Ejecución, `$262`, modos estricto/no estricto, módulos, `raw`, `async`, `CanBlock`, negativos por fase | Leído completo |
| 16 | test262, `features.txt` | ídem | Sección `## Proposed language features` (líneas 1–79) = lista de exclusión; `Temporal` está en la sección estándar (línea 249) | Leído |
| 17 | test262, `test/built-ins/RegExp/property-escapes/generated/` | ídem | Generados para Unicode v17.0.0 | Leído |

## 2. Navegadores y motores de referencia

| # | Fuente | Versión fijada | Verificación |
| --- | --- | --- | --- |
| 18 | Firefox, paquete Arch `extra/firefox` | 159.0.1-1; binario `/usr/bin/firefox` | `pacman -Si`; lista de archivos en https://archlinux.org/packages/extra/x86_64/firefox/files/ (no instalado; requiere sudo) |
| 19 | geckodriver, paquete Arch `extra/geckodriver`; release https://github.com/mozilla/geckodriver/releases | 0.37.1-1 (último release v0.37.1); binario `/usr/bin/geckodriver` | `pacman -Si`; API de releases; lista de archivos en archlinux.org |
| 20 | Chromium, paquete Arch `extra/chromium` (incluye `chromedriver`) | 156.0.8078.79-1; binarios `/usr/bin/chromium` y `/usr/bin/chromedriver` | `pacman -Si`; lista de archivos en archlinux.org |
| 21 | Servo, https://github.com/servo/servo/releases/tag/v0.8.0 | 0.8.0; `servo-x86_64-linux-gnu.tar.gz` sha256 `02a78892746e69bc39d471da2bb012fd8ce228e464cda5a017334d0e88c61de1` | Ejecutado: `servoshell --version` → `Version: Servo 0.8.0-4e136d8ef`; `--help` muestra `--webdriver`, `--headless`, `--certificate-path`, `--ignore-certificate-errors`; `ldd` sin faltantes |
| 22 | Boa, https://github.com/boa-dev/boa/releases/tag/v0.22 | 0.22.0; `boa-x86_64-unknown-linux-gnu` sha256 `d3484a63c9ee5f025de11379d055285a0bc015cf6d1d45b895e0d0f93566d671` | Ejecutado: `boa --version` → `boa 0.22.0`; `--help` muestra `--test262-object`, `--strict`, `-m/--module`, `--no-can-block`; `$262.createRealm` y `$262.agent` presentes |
| 23 | Node, paquete Arch `nodejs` | 26.13.0-1 | Ejecutado: `node --version`; `--experimental-vm-modules` habilita `vm.SourceTextModule` |
| 24 | Xvfb, paquete Arch `extra/xorg-server-xvfb` | 21.1.25-2; binario `/usr/bin/Xvfb` | `pacman -Si`; lista de archivos en archlinux.org. Ejecutado (2026-11-11, instalado): `Xvfb -version` responde `Unrecognized option: -version` y la ayuda (`-help`) no tiene opción de versión; la versión sale del *vendor release* del setup X11 (`12101025` = 1.21.1.25), que lee `bench/pins_check.py` |

## 3. Toolchain y sistema

| # | Fuente | Versión | Verificación |
| --- | --- | --- | --- |
| 25 | Rust (rustup) | rustc 1.98.1 (2026-09-01), cargo 1.98.1, rustfmt 1.9.0 | Ejecutado |
| 26 | Enlace sin `libgcc_s` | — | Ejecutado: binario Rust normal pide `libgcc_s.so.1`; con `build.rs` que copia `libgcc_eh.a` como `libgcc_s.a` en la ruta de enlace, `readelf -d` solo muestra `libc.so.6` y `ld-linux-x86-64.so.2`, y `catch_unwind` sigue funcionando |
| 27 | libxcb, paquete Arch `libxcb` | 1.17.0-1; `/usr/lib/libxcb.so.1`, `/usr/lib/libxcb-shm.so.0`; dependencias transitivas `libXau.so.6`, `libXdmcp.so.6` | `pacman -Q`; `ldd` |
| 28 | glibc | 2.44 | `pacman -Q` |
| 29 | Python | 3.14.9 | Ejecutado |
| 30 | util-linux `unshare`, iproute2 `ip` | iproute2 7.3.0 | Ejecutado: `unshare -rn sh -c 'ip link set lo up && exec unshare --map-user=1000 --map-group=1000 …'` → loopback funciona, conexión externa falla con errno 101 |
| 31 | systemd user scopes | — | Ejecutado: `systemd-run --user --scope -p MemoryMax=1G true` funciona |
| 32 | Espacios de nombres de usuario | `max_user_namespaces` = 60863 | Leído de `/proc/sys/user/max_user_namespaces` |
| 33 | Fuentes del sistema | Liberation (Serif, Sans, Mono), Noto, JetBrains Mono Nerd Font en `/usr/share/fonts` | Listado |
| 34 | Ubuntu 24.04 para CI: `libxcb1-dev`, `libxcb-shm0-dev` | 1.15-1ubuntu2 | https://packages.ubuntu.com/noble/ |
| 35 | GitHub Actions `actions/checkout` | v7 (v7.0.1, 2026-07-20) | API de releases |

## 4. Especificaciones y datos

| # | Fuente | Revisión | Uso |
| --- | --- | --- | --- |
| 36 | ECMA-262, https://tc39.es/ecma262/ | la que prueba test262 fijado | Motor JS |
| 37 | Temporal (en ECMA-262 estándar según `features.txt` fijado) | ídem | Built-in Temporal |
| 38 | WHATWG HTML, https://html.spec.whatwg.org/ | vigente | Parsing (§13), bucle de eventos (§8), scripts |
| 39 | WHATWG DOM, https://dom.spec.whatwg.org/ | vigente | DOM |
| 40 | WHATWG Encoding, https://github.com/whatwg/encoding | `be17ac5c49881336538acfc7b40dba3c8009144b` (2026-10-19) | Índices `index-*.txt` e `indexes.json` |
| 41 | WHATWG URL, https://github.com/whatwg/url | `845bafca22fb0c5a42c4bf384a9bdea525a2f10d` (2026-10-19) | Parser de URL |
| 42 | Unicode Character Database 17.0.0, https://www.unicode.org/Public/17.0.0/ucd/ | 17.0.0 | Propiedades, mayúsculas, normalización, `LineBreak.txt`, `auxiliary/GraphemeBreakProperty.txt` |
| 43 | UTS #46 IDNA 17.0.0, https://www.unicode.org/Public/17.0.0/idna/IdnaMappingTable.txt | 17.0.0 | URL con dominios internacionales |
| 44 | W3C WebDriver, https://www.w3.org/TR/webdriver2/ | vigente | Subconjunto del servidor |
| 45 | Web IDL, https://webidl.spec.whatwg.org/ ; archivos `interfaces/*.idl` de WPT fijado | WPT `77787fe5fe23` | Bindings generados |
| 46 | CSS Syntax 3, Values 4, Variables, Selectors 4, Cascade 5, CSSOM, CSSOM View, CSS 2.2, Display 3, Box 3, Sizing 3/4, Position 3, Flexbox 1, Grid 2, Tables 3, Text 3, Fonts 4, Backgrounds 3, Color 4/5 (https://drafts.csswg.org/) | vigente | Estilo, layout y pintado |
| 47 | RFC 8446 (TLS 1.3), RFC 5280 (X.509), RFC 7748 (X25519), RFC 8017 (RSA), FIPS 186-5 (ECDSA), RFC 5869 (HKDF), RFC 8439 (ChaCha20-Poly1305), NIST SP 800-38D (GCM), RFC 9112 (HTTP/1.1), RFC 1951/1950/1952 (deflate, zlib, gzip), RFC 7932 (Brotli) | — | Red |
| 48 | OpenType 1.9.1, https://learn.microsoft.com/typography/opentype/spec/ ; WOFF 1.0 y WOFF2 (W3C) | — | Fuentes |
| 49 | PNG (W3C, 3.ª ed.), GIF89a, JPEG (ITU T.81) | — | Imágenes |

## 5. Antecedentes (de la investigación adjunta)

Ningún intento de una persona o de agentes (FastRender, one-agent-one-browser, cwbrowser, we) publica WPT con el arnés oficial comparado contra Firefox o Chromium en la misma máquina; Ladybird y Servo publican WPT oficial pero dependen de bibliotecas que este proyecto prohíbe. Boa publica 96.1% de test262 (2026-07-25). Se usan solo como contexto, no como piso: los pisos se miden.

- FastRender: https://github.com/wilsonzlin/fastrender
- one-agent-one-browser: https://github.com/embedding-shapes/one-agent-one-browser
- Ladybird, boletines: https://ladybird.org/newsletter/2026-06-30
- Servo, informe de WPT (datos 2026-02-13): https://tangled.org/burrito.space/servo-readiness-report
- we: https://tangled.org/pierrelf.com/we
- test262.fyi: https://github.com/CanadaHonk/test262.fyi
