# Changelog

Cambios y decisiones de vidrio por versión, en términos del producto.

## [Sin publicar]

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
