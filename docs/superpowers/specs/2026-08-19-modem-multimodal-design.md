# Módem multimodal air-gapped — diseño

## Contexto

Dos máquinas necesitan intercambiar archivos sin red, sin Bluetooth y sin cable: solo
la pantalla y la cámara que ya tienen, y opcionalmente el altavoz y el micrófono. El
caso de uso real es air-gap — pasar secretos, configuraciones o claves entre equipos
aislados, donde conectar un USB o levantar una red no es aceptable.

La idea no es "un QR con un enlace". Es una **capa de transporte completa sobre un
medio óptico**: la pantalla emite paquetes como QR animados, la cámara del otro lado
los captura, y encima corre handshake, secuencias, confirmaciones, retransmisión y
control de flujo. El canal acústico (FSK en banda casi inaudible) se suma como segundo
medio físico cuando el hardware demuestra que es viable, principalmente para señalización
y ACK, porque una confirmación por audio evita el round-trip óptico completo.

El punto de diseño central: **el protocolo no debe saber por qué medio viaja**. Añadir
audio, LEDs o un socket TCP más adelante debe ser implementar un trait, no editar la
máquina de estados.

Estado actual del repo: scaffold limpio de Tauri 2 + Leptos (CSR, Trunk), sin commits.
Frontend en Rust/WASM, backend en Rust. Toolchain verificado (Rust 1.96, Tauri CLI
2.11.4, Trunk 0.21.14, target `wasm32-unknown-unknown` presente).

## Decisiones tomadas

| Decisión | Elección |
|---|---|
| Alcance | Los 8 subsistemas, entregados por fases con cortes funcionales |
| Fiabilidad | RaptorQ **y** ARQ, seleccionables por perfil detrás de un trait |
| Cámara | Híbrido: preview con `getUserMedia` en el WebView, decode en el backend |
| Estructura | Cargo workspace con crates separados |

Decisiones adicionales que introduce este diseño (no estaban en el boceto original):

- **Núcleo sans-io.** El protocolo es una máquina de estados pura: `handle_incoming`,
  `poll_transmit`, `handle_timeout`. No abre sockets, cámaras ni audio. Es el patrón de
  `quinn`/`rustls`, y es lo que permite testear una transferencia completa con 40% de
  pérdida sin encender una sola cámara.
- **FSM en capas.** El boceto original metía `AudioNoiseMeasurement`, `AudioFrequencySweep`
  etc. como estados de *sesión*. Eso acopla la sesión al audio: añadir un tercer canal
  obligaría a editar la FSM de sesión. Aquí la sesión tiene una FSM pequeña
  (`Discovering → Peered → Negotiating → Active → Closing`) y **cada canal tiene su
  propio ciclo de vida independiente** (`Down → Probing → Up{profile} → Degraded → Down`).
  La calibración es asunto del canal, no de la sesión.
- **Elección de líder.** Dos apps simétricas tienen un problema de desempate: ¿quién
  inicia la calibración, quién emite el barrido primero? Se resuelve comparando los
  `peer_id` (16 bytes aleatorios) lexicográficamente. El menor es líder, secuencia la
  calibración y fija el `session_id`.
- **FDM en audio, sin cancelación de eco.** Cada micrófono oye su propio altavoz. En vez
  de AEC (difícil) o TDMA (lento), la calibración ya descubre bandas viables por dirección:
  se asignan **bandas disjuntas** (líder abajo, seguidor arriba). Full duplex acústico
  sin AEC.
- **IPC binario crudo.** Los frames van por `tauri::ipc::Request` con cuerpo binario, no
  como arrays JSON. Un frame en escala de grises pasado como array JSON son ~4 MB de
  texto por frame; como bytes crudos son 900 KB. Esta es la decisión que hace viable la
  ruta híbrida que elegiste.
- **Cifrado con nonce derivado.** El nonce de ChaCha20-Poly1305 se deriva de
  `(session_id, direction, seq)` en ambos lados. No se transmite. En un canal donde cada
  byte cuesta, ahorrar 12 bytes por PDU importa.

## Arquitectura

```
┌──────────────────────────────────────────────┐
│ Transferencia de archivos                    │
├──────────────────────────────────────────────┤
│ Sesión  (FSM pequeña, elección de líder)     │
├──────────────────────────────────────────────┤
│ Fiabilidad  (trait: RaptorQ | ARQ)           │
├──────────────────────────────────────────────┤
│ Multiplexor  (clase de prioridad → canal)    │
├───────────────────────┬──────────────────────┤
│ Canal visual          │ Canal acústico       │
│ ciclo de vida propio  │ ciclo de vida propio │
├───────────────────────┼──────────────────────┤
│ pantalla ⇄ cámara     │ altavoz ⇄ micrófono  │
└───────────────────────┴──────────────────────┘
```

### Layout de crates

Requiere mover la raíz del workspace al raíz del repo. Hoy `tauri-app/Cargo.toml` es a la
vez paquete UI y raíz del workspace (convención de la plantilla); con varios crates eso
estorba.

```
qr_comm/
├── Cargo.toml                 # [workspace] members = ["tauri-app", "tauri-app/src-tauri", "crates/*"]
├── crates/
│   ├── optical-protocol/      # sans-io: PDU, FSM sesión, fiabilidad, traits. Sin I/O, sin tauri.
│   ├── optical-codec/         # QR encode/decode + geometría. Compartido nativo + wasm.
│   ├── acoustic-codec/        # 2-FSK mod/demod, preámbulo, framing. Sin I/O de audio.
│   ├── link-calibration/      # escaleras de sondas, scoring, perfiles. Lógica pura.
│   └── channel-sim/           # dev-dep: canal con pérdidas + distorsión sintética de cámara
├── tauri-app/
│   ├── Cargo.toml             # tauri-app-ui (leptos/wasm) — se le quita [workspace]
│   ├── src/                   # UI: QrDisplay, CameraPreview, AlignmentOverlay, Progress
│   └── src-tauri/             # tauri-app: drivers de cámara/audio, comandos, filesystem
```

Ajustes que arrastra el movimiento: `.gitignore` pasa a la raíz (`/target/`, `/dist/`);
**`Cargo.lock` se commitea** (la plantilla lo ignora, pero esto es una app, no una lib);
`Trunk.toml` y `tauri.conf.json` se quedan donde están y siguen funcionando porque
`trunk serve` corre desde `tauri-app/`.

### Abstracciones centrales

Formato de PDU — a mano, no `bincode` (bincode no es un formato de wire estable):

```rust
version: u8, session_id: u64, kind: u8, flags: u16,
seq: u32, ack: u32, payload_len: u16, payload: [u8], crc32: u32
```

Cabecera de ~24 B. Sobre un payload de 900 B son 2.7% de overhead.

```rust
pub trait Channel {
    fn caps(&self) -> ChannelCaps;      // mtu, dirección, bps estimados, latencia
    fn health(&self) -> ChannelHealth;  // per, calidad, último rx
    fn send(&mut self, pdu: &Pdu) -> Result<(), ChannelError>;
    fn poll(&mut self) -> Option<Pdu>;
}

pub trait Reliability {
    fn on_data(&mut self, pdu: &Pdu) -> Vec<AppEvent>;
    fn next_transmit(&mut self) -> Option<Payload>;
    fn feedback(&mut self, ack: AckInfo);
    fn is_complete(&self) -> bool;
}
```

Los drivers (cámara, audio) corren en sus propias tasks y hablan con el núcleo por mpsc.
El núcleo nunca bloquea.

## Fases

Cada fase deja la app en un estado usable. Las fases 4-7 pueden re-planificarse sin tocar
el núcleo.

### Fase 0 — Spike: throughput de IPC  *(código desechable)*

Medir la ruta híbrida antes de construir encima. Frame en escala de grises 1280×720 desde
`getUserMedia` → IPC binario crudo → backend → decode con `rqrr` → evento de vuelta.

- Medir: frames decodificados/s sostenidos y CPU.
- Medir también la ruta de array JSON, para cuantificar la diferencia.
- **Objetivo: ≥10 decodificaciones/s con <30% de un core.**

Si no llega, plan de repliegue en orden: (a) recortar al ROI del QR tras el primer lock
—típicamente 40-60% menos píxeles—, (b) mover el decode a WASM, (c) `nokhwa` en el backend.

Salida: un número y un go/no-go. Nada de este código sobrevive.

### Fase 1 — Núcleo de protocolo  *(sin hardware)*

`optical-protocol` + `channel-sim`.

- PDU encode/decode, CRC32.
- FSM de sesión + elección de líder por `peer_id`.
- Trait `Reliability` con las dos implementaciones: `Raptor` (crate `raptorq`) y `Arq`
  (ventana deslizante, retransmisión selectiva).
- Trait `Channel` + `ChannelCaps` / `ChannelHealth`.
- `channel-sim`: pérdida, reordenamiento, duplicación, corrupción y latencia configurables,
  con RNG semillado y determinista.

**Verificación:** `cargo test -p optical-protocol` transfiere un archivo de 5 MB con 40%
de pérdida usando RaptorQ y con 15% usando ARQ. Property test de roundtrip del formato de
wire con `proptest`. Cero hardware.

### Fase 2 — Canal visual + UI

`optical-codec` + frontend + drivers.

- Encode con el crate `qrcode`; decode con `rqrr`, devolviendo payload **y** `QrGeometry`
  (bbox, rotación, error de perspectiva, cobertura del frame).
- **Banco de distorsión sintética** en `channel-sim`: renderiza el QR, le aplica warp de
  perspectiva + desenfoque gaussiano + ruido de sensor, y lo decodifica. Esto convierte
  "poner dos laptops enfrentadas" en un test que corre en CI.
- Frontend: preview con `getUserMedia` en canvas, IPC binario a cadencia de decode
  (desacoplada de la cadencia de preview), componente de QR, overlay de alineamiento
  alimentado por eventos de `QrGeometry`.
- Perfil fijo conservador: ~800 B, ECC Q, 8 QR/s, hold 125 ms.
- Modo loopback: dos instancias en una máquina por localhost, para probar el protocolo
  extremo a extremo sin cámaras.

**Verificación:** archivo transferido entre dos laptops enfrentadas. La suite sintética
pasa en CI sin cámara.

### Fase 3 — Calibración visual

`link-calibration`.

- Escalera de sondas: duplicar hasta fallar, búsqueda binaria, margen del 15%.
- Perfiles independientes por dirección (una webcam puede ser mejor que la otra).
- Scoring por goodput real, no por capacidad máxima:
  `payload × qr/s × tasa_de_éxito`, penalizado por reintentos y latencia.
- Bucle de control adaptativo: subida aditiva (+64 B), bajada multiplicativa (×0.7).
- Ciclo de vida del canal conectado: `Probing → Up{profile} → Degraded`.

**Verificación:** el perfil negociado supera en goodput medido al perfil fijo de la Fase 2,
y el enlace se recupera solo al desenfocar y reenfocar la cámara.

### Fase 4 — Canal acústico

`acoustic-codec` + drivers `cpal`.

- 2-FSK, correlación de preámbulo, detección por Goertzel.
- Captura/reproducción con `cpal`, ring buffers.
- FDM: bandas disjuntas por dirección asignadas por el líder.

**Verificación:** mod→demod a través de AWGN sintético + filtro paso banda + recorte, a
SNR variable; pasa a SNR ≥ 10 dB sin hardware. Luego, PDUs de control reales entre dos
máquinas.

### Fase 5 — Calibración acústica

- Enumeración de dispositivos, piso de ruido por banda.
- Barrido **estrictamente alternado** (nunca simultáneo, para no captar el propio altavoz),
  coordinado por el canal visual que ya está arriba.
- Prueba de modulación real (BER/PER, no solo detección de tono), scoring, enum de
  viabilidad (`FullDuplex | HalfDuplex | ControlOnly | Unavailable`).
- Handshake de commit/verify antes de habilitar, con retorno automático a visual-only.
- Supervisor en runtime con política de degradación por PER sostenido.

**Verificación:** en hardware real distingue correctamente `Unavailable` de `ControlOnly`,
y solo desactiva el audio ante degradación sostenida, no ante un pico.

### Fase 6 — Multiplexor

- Clases de prioridad: `Control > Metadata > Data`.
- Scheduler que mapea clase → canal según `ChannelHealth` en vivo.
- Duplicación de mensajes críticos por ambos canales, con deduplicación por `(session, seq)`.

**Verificación:** los ACK migran a audio cuando es viable y vuelven a visual al degradarse,
sin interrumpir la transferencia en curso.

### Fase 7 — Emparejamiento cifrado

- X25519 con la clave pública viajando por el canal visual — el óptico es línea de vista,
  lo que hace el MITM físicamente incómodo.
- `HKDF(shared_secret, qr_nonce || audio_nonce)`.
- ChaCha20-Poly1305 por PDU, nonce derivado de `(session_id, direction, seq)`.
- **SAS** (short authentication string) mostrado en ambas pantallas para comparación visual
  — es la defensa estándar contra MITM y cierra el hueco que el fingerprint dual-canal
  deja abierto.

**Verificación:** transferencia cifrada extremo a extremo, el SAS coincide en ambas
pantallas, y un PDU manipulado se rechaza.

## Verificación global

La estrategia es que **casi todo se pueda probar sin dos laptops**:

1. `cargo test --workspace` — núcleo con canal simulado, roundtrip de wire, distorsión
   sintética de QR, FSK sobre AWGN.
2. Modo loopback — dos instancias en una máquina, protocolo extremo a extremo, sin cámara.
3. `cargo tauri dev` en una máquina — UI, preview de cámara, alineamiento, decode de un QR
   mostrado en otra ventana.
4. Prueba de dos máquinas enfrentadas — la puerta final de cada fase, manual.

## Riesgos

| Riesgo | Mitigación |
|---|---|
| El IPC de frames no da el throughput | Fase 0 lo mide antes de construir; tres repliegues definidos |
| El ultrasonido no sobrevive al hardware real (AEC y supresión de ruido del SO filtran >16 kHz) | La calibración reporta `Unavailable` honestamente; el audio siempre es opcional |
| Autofocus cazando, moiré de pantalla, brillo | ECC + margen del 15% en la calibración + política de degradación |
| Bloqueo entre pares simétricos | Elección de líder determinista por `peer_id` |
| Eco: cada micro oye su propio altavoz | FDM con bandas disjuntas por dirección |
| 8 subsistemas, el spec envejece | Cortes funcionales por fase; 4-7 re-planificables sin tocar el núcleo |

## Primeros pasos de ejecución

1. Commit inicial del scaffold actual (el repo no tiene ninguno todavía).
2. Guardar este diseño en `docs/superpowers/specs/2026-08-19-modem-multimodal-design.md`
   y commitearlo.
3. Reestructurar a workspace en la raíz.
4. Fase 0.

## Nota fuera de alcance

Los targets de Android están instalados en este toolchain y Tauri 2 soporta móvil. Un
teléfono es un segundo peer mejor que una laptop —cámara, altavoz y micrófono superiores—
y encaja sin cambiar el protocolo. No es alcance ahora, pero el diseño no lo impide.


---

## Anexo: hallazgos medidos durante la implementación

Estos números no estaban en el diseño original. Salieron de medir, y algunos
contradicen supuestos que el diseño daba por buenos.

### Píxeles por módulo: 6, no 3

Barrido sobre la cámara sintética (`cargo run -p optical-codec --example umbral`):

| px/módulo | tasa de lectura |
|---|---|
| 2,0–3,0 | 24–40 % |
| 3,0–6,0 | 60–87 % |
| ≥ 6,0   | ~100 %  |

El estándar da 2 como mínimo absoluto, pero supone una rejilla alineada al
píxel. Una cámara escala de forma fraccionaria, los bordes de módulo caen a
mitad de píxel y el detector se despista. **El doble del mínimo teórico es lo
que cuesta la realidad.**

### Capacidad real por marco a 720p

Payload que decodifica **de forma fiable** (todas las repeticiones), con el
código ocupando el 75 % del alto:

| corrección | bytes fiables por marco |
|---|---|
| L | 500 |
| M | 400 |
| Q | 200 |
| H | 200 |

El diseño asumía ~900 B. La diferencia importa: a 10 QR/s son 2–5 KB/s, y **un
archivo de 5 MB tardaría entre 17 y 40 minutos**. Conviene decírselo al usuario
antes de empezar, no a mitad.

Cuidado con la distinción entre «decodifica una vez» y «decodifica siempre»: a
3,3 px/módulo entran 1200 B *a veces*. Negociar sobre ese número daría un perfil
que falla uno de cada cuatro marcos.

### La resolución de cámara es la palanca dominante

El payload por marco crece con el **cuadrado** de la resolución lineal, mientras
que subir FPS solo escala linealmente y bajar la corrección apenas da un factor
2,5. Pasar de 720p a 1080p multiplica por ~2,25 el payload por marco. La
calibración debe priorizar negociar la resolución de captura más alta que la
cámara sostenga.

### RaptorQ: acotar el bloque de fuente no es opcional

Dejar que un objeto de 5 MB caiga en un solo bloque de ~6000 símbolos costaba
**más de nueve minutos de CPU** al reconstruir. Acotando a 1024 símbolos por
bloque baja a segundos. El coste crece muy por encima de lineal con K porque
cada bloque se resuelve por eliminación gaussiana sobre GF(256).

Esto obligó a corregir una decisión del diseño: el OTI **sí viaja** por el cable.
Son 12 bytes una vez por transferencia, y derivarlo en ambos lados ataba el
troceado a lo que decidiera `with_defaults`.

### Fuente frente a ARQ, medido

Transferencia de 5 MB sobre el simulador:

| | símbolos enviados | mínimo teórico | exceso |
|---|---|---|---|
| Fuente, 40 % pérdida | 10 210 | 10 047 | **+1,6 %** |
| ARQ, 15 % pérdida | 10 095 | 7 091 | +42 % |

Fuente opera casi en el óptimo teórico con casi el triple de pérdida. Confirma
que debe ser el modo por defecto para volumen, y ARQ quedar para control.

### Limitación conocida de la medida de nitidez

La varianza del laplaciano sube con el ruido, no solo con el enfoque: una imagen
ruidosa y borrosa puede puntuar más que una limpia y algo borrosa. Para usarla
como criterio de enfoque en la Fase 3 habrá que filtrar el ruido antes, o
combinarla con otra medida.
