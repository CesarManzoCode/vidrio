# vidrio

vidrio es un motor de navegador escrito desde cero para Arch Linux: parser HTML, DOM, CSS con selectores, cascada y herencia, layout block, inline, flex, grid y tablas, texto, pintado con rasterizado propio, HTTP y TLS propios y un motor de JavaScript propio (`vjs`) con bucle de eventos. No usa V8, QuickJS, SpiderMonkey, JavaScriptCore, Servo, WebKit, Blink ni Gecko. Sus únicas dependencias externas son el kernel, glibc, libxcb y la extensión MIT-SHM de X.

Su calidad se mide solo con suites oficiales, en la misma PC, contra navegadores y motores reales:

- web-platform-tests con su arnés oficial (`./wpt run`) contra Firefox, Chromium y Servo;
- test262 contra Boa, con el mismo corredor para todos.

El contrato técnico está en [`vault/CONTRACT.md`](vault/CONTRACT.md) y las fuentes con sus revisiones fijadas en [`vault/RESEARCH.md`](vault/RESEARCH.md). Lo que el proyecto no demuestra está en [`LIMITATIONS.md`](LIMITATIONS.md).

## Estado

| Fase | Contenido | Estado |
| --- | --- | --- |
| K0 | Fundación: contrato, investigación, plan | Cerrada |
| K1 | Arnés de medición y referencias (Firefox, Chromium, Servo, Boa) | Cerrada |
| K2 | Motor JavaScript `vjs` medido con test262 | Pendiente |
| K3 | Red, TLS, documento, DOM, bucle de eventos y WebDriver | Pendiente |
| K4 | CSS, layout, texto, pintado y ventana | Pendiente |
| K5 | Conformidad y pisos de éxito | Pendiente |

## Resultados

Los pisos de éxito son `R(total) ≥ R_servo(total)` en web-platform-tests, ningún directorio del alcance en cero, y `P ≥ P_boa` en test262. vidrio y `vjs` todavía no se miden: `R` y `P` aún no existen.

Medición de referencia de K1, en esta PC, con web-platform-tests `77787fe5fe23` y test262 `9ae074624e2d` (salida de `bench/ratio.py` en `bench/results/ratio.json`):

- **Piso de web-platform-tests:** `R_servo(total)` = **0.9869** (Servo 0.8.0 contra Firefox 159.0.1).
- **Piso de test262:** `P_boa` = **0.9846**. Boa 0.22.0 pasa 90 598 de 92 012 escenarios, excluidas las 17 features en propuesta.

Unidades PASS por directorio y `R_servo(D)`:

| Directorio | Firefox | Chromium | Servo | `R_servo(D)` |
| --- | ---: | ---: | ---: | ---: |
| `html/syntax/parsing` | 7867 | 8127 | 7850 | 0.9978 |
| `html/syntax/serializing-html-fragments` | 352 | 376 | 363 | 1.0312 |
| `dom` | 65255 | 66186 | 64752 | 0.9923 |
| `encoding` | 1152129 | 1152093 | 1151832 | 0.9997 |
| `url` | 11021 | 11643 | 11281 | 1.0236 |
| `html/semantics/scripting-1` | 3950 | 4434 | 3435 | 0.8696 |
| `html/webappapis` | 3371 | 3586 | 3451 | 1.0237 |
| `css/css-syntax` | 347 | 440 | 335 | 0.9654 |
| `css/css-values` | 6150 | 10115 | 5875 | 0.9553 |
| `css/css-variables` | 686 | 697 | 640 | 0.9329 |
| `css/selectors` | 6188 | 5651 | 4368 | 0.7059 |
| `css/css-cascade` | 1693 | 1802 | 1122 | 0.6627 |
| `css/cssom` | 4377 | 4354 | 3798 | 0.8677 |
| `css/cssom-view` | 2306 | 2501 | 1492 | 0.6470 |
| `css/CSS2` | 8534 | 8741 | 8174 | 0.9578 |
| `css/css-display` | 570 | 657 | 449 | 0.7877 |
| `css/css-box` | 917 | 1012 | 677 | 0.7383 |
| `css/css-sizing` | 5636 | 6794 | 4755 | 0.8437 |
| `css/css-position` | 1626 | 1808 | 1298 | 0.7983 |
| `css/css-flexbox` | 5623 | 5819 | 4168 | 0.7412 |
| `css/css-grid` | 15268 | 17222 | 11180 | 0.7323 |
| `css/css-tables` | 1028 | 1117 | 969 | 0.9426 |
| `css/css-text` | 5366 | 6615 | 4629 | 0.8627 |
| `css/css-fonts` | 8618 | 8763 | 6183 | 0.7175 |
| `css/css-backgrounds` | 6637 | 6711 | 5124 | 0.7720 |
| `css/css-color` | 11906 | 11715 | 11765 | 0.9882 |
| **Total** | **1337421** | **1348979** | **1319965** | **0.9869** |

Cada corrida de directorio quedó bajo el límite de memoria de 10 GiB. Los picos más altos de `memory.peak` fueron 8 883 073 024 B en Chromium (`css/css-grid`), 4 100 489 216 B en Firefox (`encoding`) y 3 447 492 608 B en Servo (`encoding`).

## Uso

```sh
make check                                    # formato, tipos, tests y dependencias prohibidas
python3 bench/fetch.py                        # descarga WPT, test262, Servo, Boa y lo que ./wpt run baja al arrancar (requisitos, MojoJS, OpenH264) y aplica el parche
python3 bench/pins_check.py                   # comprueba que árboles y binarios están en sus revisiones fijadas
python3 bench/scope_check.py                  # comprueba que cada directorio del alcance tiene pruebas en el manifiesto
python3 bench/deps.py ldd BIN…                # comprueba que las bibliotecas enlazadas de BIN están permitidas
python3 bench/deps.py forbidden               # comprueba que no hay dependencias prohibidas
python3 bench/wptrun.py --product P --all     # corre WPT aislado para P (también --dir D…, --path T…, --run N, --force)
python3 bench/wptrun.py --net-probe           # comprueba que el aislamiento deja loopback y bloquea la red externa
python3 bench/test262.py --engine E           # corre test262 con E ∈ {node, boa, vjs} (--control: conjunto de control)
python3 bench/ratio.py                        # calcula R, R_servo, P y P_boa en bench/results/ratio.json
python3 bench/stability.py --product vidrio --dir D  # compara dos corridas de D contra bench/flaky.json
```

Los comandos del navegador se documentan aquí cuando existan.

## Licencia

BSD-2-Clause. Ver [`LICENSE`](LICENSE).
