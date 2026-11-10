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
make check            # formato, tipos, tests y dependencias prohibidas
```

Los comandos del navegador y del arnés se documentan aquí cuando existan.

## Licencia

BSD-2-Clause. Ver [`LICENSE`](LICENSE).
