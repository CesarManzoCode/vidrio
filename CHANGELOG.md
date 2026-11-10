# Changelog

Cambios y decisiones de vidrio por versión, en términos del producto.

## [Sin publicar]

### Decidido

- Rust sin crates de terceros para el navegador y el motor JS; el runtime de GCC se enlaza estático para que el binario solo dependa de glibc y libxcb.
- Un solo proceso con DOM en arena, motor JS de bytecode con recolector mark-sweep compartido con el DOM, y lista de pintado propia rasterizada por software.
- Integración con web-platform-tests mediante un servidor WebDriver propio y los ejecutores WebDriver genéricos de wptrunner.
- Revisiones fijadas: web-platform-tests `77787fe5fe23`, test262 `9ae074624e2d`, Firefox 159.0.1, Chromium 156.0.8078.79, Servo 0.8.0, Boa 0.22.0.
- Medición sin red externa y sin sudo, con un espacio de red propio por corrida.
