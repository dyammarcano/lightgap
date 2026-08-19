//! Cómo está encuadrado el código dentro del frame.
//!
//! Es lo que alimenta la ayuda de alineamiento de la interfaz. La medida que
//! más manda es `pixels_per_module`: por debajo de unos 6 píxeles por módulo
//! —valor medido, ver [`MIN_PIXELS_PER_MODULE`]— el detector empieza a fallar
//! por muy bien centrado que esté el código, y ningún otro ajuste lo compensa.
//! Por eso «acércate» es casi siempre el consejo correcto cuando algo va mal.

/// Un punto en coordenadas del frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    fn dist(self, other: Self) -> f32 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }
}

/// Dónde y cómo se ve el código.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QrGeometry {
    /// Esquinas en orden: superior izquierda, superior derecha, inferior
    /// derecha, inferior izquierda.
    pub corners: [Point; 4],
    /// Lado medio en píxeles.
    pub side_px: f32,
    /// Giro respecto a la horizontal, en grados, en −180..=180.
    pub rotation_deg: f32,
    /// Cuánto se aparta de un cuadrado, en 0..=1.
    ///
    /// Cero es un cuadrado perfecto; crece cuando el código se ve en escorzo
    /// porque las pantallas no están enfrentadas.
    pub perspective_error: f32,
    /// Fracción del área del frame que ocupa el código, en 0..=1.
    pub frame_coverage: f32,
    /// Desplazamiento del centro respecto al centro del frame, en 0..=1, donde
    /// 1 es una esquina.
    pub offset: f32,
    /// Módulos por lado del código detectado.
    pub modules: u32,
    /// Píxeles de imagen por módulo. La medida que decide si se puede leer.
    pub pixels_per_module: f32,
}

impl QrGeometry {
    /// Calcula la geometría a partir de las cuatro esquinas y el tamaño del
    /// frame.
    #[must_use]
    pub fn from_corners(corners: [Point; 4], modules: u32, frame_w: u32, frame_h: u32) -> Self {
        let [tl, tr, br, bl] = corners;

        let arriba = tl.dist(tr);
        let derecha = tr.dist(br);
        let abajo = br.dist(bl);
        let izquierda = bl.dist(tl);
        let side_px = (arriba + derecha + abajo + izquierda) / 4.0;

        // El escorzo se estima comparando lados opuestos: en un cuadrado visto
        // de frente son iguales, y la diferencia relativa crece con el ángulo.
        let perspective_error = if side_px > 0.0 {
            let h = (arriba - abajo).abs() / side_px;
            let v = (izquierda - derecha).abs() / side_px;
            (h.max(v)).min(1.0)
        } else {
            1.0
        };

        let rotation_deg = (tr.y - tl.y).atan2(tr.x - tl.x).to_degrees();

        let frame_area = (frame_w as f32) * (frame_h as f32);
        let frame_coverage = if frame_area > 0.0 {
            (side_px * side_px / frame_area).min(1.0)
        } else {
            0.0
        };

        let cx = (tl.x + tr.x + br.x + bl.x) / 4.0;
        let cy = (tl.y + tr.y + br.y + bl.y) / 4.0;
        let offset = if frame_w > 0 && frame_h > 0 {
            let dx = (cx - frame_w as f32 / 2.0) / (frame_w as f32 / 2.0);
            let dy = (cy - frame_h as f32 / 2.0) / (frame_h as f32 / 2.0);
            (dx * dx + dy * dy).sqrt().min(1.0)
        } else {
            0.0
        };

        let pixels_per_module = if modules > 0 {
            side_px / modules as f32
        } else {
            0.0
        };

        Self {
            corners,
            side_px,
            rotation_deg,
            perspective_error,
            frame_coverage,
            offset,
            modules,
            pixels_per_module,
        }
    }
}

/// Píxeles por módulo a partir de los cuales la lectura es fiable.
///
/// **Medido, no supuesto.** Barriendo escalas fraccionarias sobre la cámara
/// sintética (`cargo run -p optical-codec --example umbral`):
///
/// | px/módulo | tasa de lectura |
/// |---|---|
/// | 2,0–3,0 | 24–40 % |
/// | 3,0–6,0 | 60–87 % |
/// | ≥ 6,0   | ~100 %  |
///
/// El estándar habla de 2 como mínimo absoluto, pero ese número supone una
/// rejilla alineada al píxel. Una cámara escala de forma fraccionaria, los
/// bordes de módulo caen a mitad de píxel y el detector —que muestrea el centro
/// del módulo— se despista. El doble del mínimo teórico es lo que cuesta la
/// realidad.
///
/// Consecuencia práctica: a 720p con el código ocupando el 70 % del alto caben
/// unos 84 módulos, es decir alrededor de 450 B por marco con corrección Q.
pub const MIN_PIXELS_PER_MODULE: f32 = 6.0;

/// Por debajo de esto la lectura falla más de la mitad de las veces.
///
/// Entre este valor y [`MIN_PIXELS_PER_MODULE`] el enlace funciona a ratos: sirve
/// para no cortar la sesión a la primera, no para operar.
pub const MARGINAL_PIXELS_PER_MODULE: f32 = 3.0;

/// Por encima de esta cobertura el código roza los bordes del frame y se corta
/// al menor movimiento.
pub const MAX_COVERAGE: f32 = 0.75;

/// Escorzo por encima del cual conviene enderezar las pantallas.
pub const MAX_PERSPECTIVE_ERROR: f32 = 0.20;

/// Desplazamiento del centro a partir del cual conviene recentrar.
pub const MAX_OFFSET: f32 = 0.35;

/// Varianza del laplaciano por debajo de la cual la imagen está desenfocada.
pub const MIN_SHARPNESS: f32 = 50.0;

/// Qué decirle a quien sostiene los equipos.
///
/// Un solo consejo cada vez, y el que más manda: una lista de cinco cosas a
/// corregir a la vez no se sigue, y algunas se arreglan solas al corregir otra
/// —acercarse suele mejorar el enfoque y la cobertura de paso.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advice {
    /// El enlace está en condiciones.
    Ok,
    /// Muy pocos píxeles por módulo.
    MoveCloser,
    /// El código llena el frame y se cortará al menor movimiento.
    MoveAway,
    /// Descentrado.
    Center,
    /// Demasiado escorzo: las pantallas no se miran de frente.
    Straighten,
    /// La imagen está borrosa.
    Focus,
}

impl Advice {
    /// Mensaje corto para la interfaz.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Ok => "Enlace óptico estable",
            Self::MoveCloser => "Acerque los equipos",
            Self::MoveAway => "Separe los equipos",
            Self::Center => "Centre el código en la cámara",
            Self::Straighten => "Enfrente las pantallas",
            Self::Focus => "Enfoque insuficiente",
        }
    }
}

/// Elige el consejo más urgente.
///
/// El orden importa y no es arbitrario: sin píxeles suficientes por módulo nada
/// más se puede arreglar, así que va primero. El enfoque va antes que el
/// centrado porque una imagen borrosa hace que las esquinas —y con ellas todas
/// las demás medidas— sean poco de fiar.
#[must_use]
pub fn advise(geom: &QrGeometry, sharpness: f32) -> Advice {
    if geom.pixels_per_module < MIN_PIXELS_PER_MODULE {
        return Advice::MoveCloser;
    }
    if geom.frame_coverage > MAX_COVERAGE {
        return Advice::MoveAway;
    }
    if sharpness < MIN_SHARPNESS {
        return Advice::Focus;
    }
    if geom.perspective_error > MAX_PERSPECTIVE_ERROR {
        return Advice::Straighten;
    }
    if geom.offset > MAX_OFFSET {
        return Advice::Center;
    }
    Advice::Ok
}

/// Varianza del laplaciano sobre una región: la medida de nitidez habitual.
///
/// Una imagen enfocada tiene bordes marcados, y el laplaciano —que responde a
/// los cambios bruscos— se dispara en ellos. Al desenfocar, los bordes se
/// suavizan y la varianza cae. Un QR es casi todo bordes, así que el indicador
/// es especialmente claro sobre esta clase de imagen.
#[must_use]
pub fn sharpness(
    width: usize,
    height: usize,
    pixels: &[u8],
    region: Option<(usize, usize, usize, usize)>,
) -> f32 {
    if width < 3 || height < 3 || pixels.len() < width * height {
        return 0.0;
    }
    let (x0, y0, x1, y1) = region.unwrap_or((0, 0, width, height));
    let x0 = x0.max(1);
    let y0 = y0.max(1);
    let x1 = x1.min(width.saturating_sub(1));
    let y1 = y1.min(height.saturating_sub(1));
    if x1 <= x0 || y1 <= y0 {
        return 0.0;
    }

    let mut suma = 0.0f64;
    let mut suma_cuadrados = 0.0f64;
    let mut n = 0u64;

    for y in y0..y1 {
        for x in x0..x1 {
            let i = y * width + x;
            // Núcleo laplaciano de 4 vecinos.
            let lap = 4.0 * f64::from(pixels[i])
                - f64::from(pixels[i - 1])
                - f64::from(pixels[i + 1])
                - f64::from(pixels[i - width])
                - f64::from(pixels[i + width]);
            suma += lap;
            suma_cuadrados += lap * lap;
            n += 1;
        }
    }

    if n == 0 {
        return 0.0;
    }
    let media = suma / n as f64;
    ((suma_cuadrados / n as f64) - media * media).max(0.0) as f32
}
