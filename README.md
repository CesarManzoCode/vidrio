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
| K1 | Arnés de medición y referencias (Firefox, Chromium, Servo, Boa) | Pendiente |
| K2 | Motor JavaScript `vjs` medido con test262 | Pendiente |
| K3 | Red, TLS, documento, DOM, bucle de eventos y WebDriver | Pendiente |
| K4 | CSS, layout, texto, pintado y ventana | Pendiente |
| K5 | Conformidad y pisos de éxito | Pendiente |

## Resultados

Todavía no hay mediciones. Los pisos de éxito son `R(total) ≥ R_servo(total)` en web-platform-tests, ningún directorio del alcance en cero, y `P ≥ P_boa` en test262; sus valores salen de la primera medición de referencia.

## Uso

```sh
make check                                    # formato, tipos, tests y dependencias prohibidas
python3 bench/fetch.py                        # descarga WPT, test262, Servo y Boa fijados a bench/cache/ y aplica el parche
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
