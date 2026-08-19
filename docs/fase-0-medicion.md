# Fase 0 — procedimiento de medición

Documento de trabajo. Se borra junto con el spike cuando la decisión esté tomada.

## Qué se decide aquí

El diseño eligió la ruta híbrida de cámara: preview con `getUserMedia` en el WebView,
decode de QR en el backend Rust. Esa elección paga un coste de IPC por cada frame. La
Fase 0 mide ese coste antes de construir encima.

**Criterio de aprobación:** ≥ 10 decodificaciones/s sostenidas con < 30 % de un core.

Si no pasa, el tramo dominante dice qué repliegue aplicar — por eso el lazo se cronometra
en tres partes y no en una:

| Tramo dominante | Qué significa | Repliegue |
|---|---|---|
| **Captura** | manda el readback GPU→CPU de `getImageData` | bajar resolución u `OffscreenCanvas`. Mover el decode a WASM **no ayuda** |
| **IPC** | cruzar el puente cuesta más que decodificar | **(b)** decode en WASM: elimina el cruce entero |
| **Decode** | `rqrr` es el cuello | **(a)** recorte a ROI tras el primer lock. WASM **tampoco** ayuda |

## Montaje

La cámara tiene que ver una pantalla que muestre el QR. La webcam integrada de un portátil
apunta al usuario y **no puede ver su propia pantalla**, así que hace falta una de estas:

- un segundo monitor con la ventana del spike, y el portátil mirándolo;
- una webcam USB apuntada a la pantalla;
- la segunda máquina, si ya está delante.

Sin ningún QR delante los tramos de captura e IPC siguen siendo válidos —son los que
deciden el go/no-go— pero el de decode queda algo optimista: `rqrr` corre la detección
completa igual, pero se salta la decodificación al no encontrar rejilla.

## Ejecución

```bash
cd tauri-app
cargo tauri dev
```

Concede el permiso de cámara cuando el WebView lo pida. Encuadra el QR, pulsa **Iniciar**
y deja correr ~30 s para que las medias rodantes se asienten (ventana de 30 muestras).

Para el criterio de CPU, con el spike ya midiendo, en otra terminal:

```bash
pwsh -File scripts/spike-cpu.ps1 -Seconds 30
```

Cuenta también los procesos `msedgewebview2`: `getImageData` y el canvas viven ahí, no en
el proceso Rust. Contar solo el padre subestimaría la ruta híbrida.

## Matriz a rellenar

Cada fila es una corrida de ~30 s. Las dos rutas de IPC a 1280×720 son la comparación que
justifica (o no) la decisión de diseño; las resoluciones menores dicen cuánto margen hay.

| Ruta | Resolución | Decod./s | Captura ms | IPC ms | Decode ms | Bytes/frame | Frames con QR | % de 1 core |
|---|---|---|---|---|---|---|---|---|
| Binaria | 1280×720 | | | | | | | |
| Binaria | 960×540 | | | | | | | |
| Binaria | 640×480 | | | | | | | |
| JSON (control) | 1280×720 | | | | | | | |

Referencia de tamaño: 1280×720 en gris son 921 608 B por frame (8 B de cabecera + w·h).
Por la ruta JSON esos mismos bytes viajan como números en texto — del orden de 4 MB.

## Resultado

- **Veredicto:** _(pendiente)_
- **Tramo dominante:** _(pendiente)_
- **Decisión:** _(pendiente — seguir con la ruta híbrida, o repliegue (a)/(b)/(c))_
- **Punto de operación elegido para la Fase 2:** _(pendiente)_

Una vez anotado: borrar `tauri-app/src/spike.rs`, `tauri-app/src-tauri/src/spike.rs`,
`scripts/`, los estilos `.spike` de `styles.css`, las deps marcadas como Fase 0 en ambos
manifiestos, y devolver `main.rs` a montar `<App/>`. Después, Fase 1.
