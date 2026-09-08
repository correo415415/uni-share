# Marca uni-share

- `logo.svg` — icono de aplicación (fuente vectorial). Cuadrado redondeado grafito `#1b1e21` (radio 22 %) con el glifo de intercambio en azul `#4c8dff` (tokens del sistema de diseño Graphite, `ui/theme.slint`).
- `logo-mark.svg` — glifo monocromo (`currentColor`) para barras de herramientas y favicon.
- `logo-<n>.png` — rasterizados 16…1024 px (`rsvg-convert`); `logo.ico` — Windows (16/32/48/256).
- `ui/icon.png` (256 px) es la copia que se embebe en el binario (icono de ventana Slint, iconos XDG de `uni-share associate`).

Regenerar: `for s in 16 32 48 64 128 256 512 1024; do rsvg-convert -w $s -h $s assets/logo.svg -o assets/logo-$s.png; done; convert assets/logo-{16,32,48,256}.png assets/logo.ico; cp assets/logo-256.png ui/icon.png`
