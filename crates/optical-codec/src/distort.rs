//! Cámara sintética: convierte una matriz de módulos en el frame borroso,
//! torcido y ruidoso que de verdad captura una webcam apuntando a una pantalla.
//!
//! Sin esto, probar el canal visual exige dos portátiles, una habitación y un
//! par de manos. Con esto cabe en un test que corre en CI, y además se puede
//! barrer el espacio de condiciones —enfoque, ángulo, distancia, luz— de forma
//! sistemática, que a mano no se hace nunca.
//!
//! Lo que se modela, y por qué cada cosa:
//!
//! - **Perspectiva.** Dos pantallas nunca quedan perfectamente enfrentadas.
//! - **Desenfoque.** El autofoco de una webcam caza, y a poca distancia falla.
//! - **Ruido.** Con poca luz el sensor sube ganancia y ensucia.
//! - **Contraste.** El brillo de la pantalla contra la exposición de la cámara;
//!   un negro que llega a gris medio arruina el umbralizado.
//! - **Moiré.** La rejilla de píxeles de la pantalla contra la del sensor. Es el
//!   artefacto propio de este medio y no aparece fotografiando papel.

use crate::encode::Modules;

/// Módulos de zona de silencio alrededor del código. Cuatro es el mínimo del
/// estándar; con menos, el detector no distingue dónde empieza el código.
const QUIET_MODULES: usize = 4;

/// Condiciones de captura.
#[derive(Debug, Clone, PartialEq)]
pub struct Conditions {
    /// Tamaño del frame de cámara.
    pub frame_w: usize,
    pub frame_h: usize,
    /// Fracción del lado menor del frame que ocupa el código, en 0..=1.
    pub fill: f32,
    /// Desplazamiento del centro, en fracción de la mitad del frame.
    pub offset_x: f32,
    pub offset_y: f32,
    /// Giro en grados.
    pub rotation_deg: f32,
    /// Inclinación horizontal y vertical, en 0..=1. Cero es de frente.
    pub tilt_x: f32,
    pub tilt_y: f32,
    /// Radio del desenfoque gaussiano, en píxeles. Cero es enfoque perfecto.
    pub blur: f32,
    /// Desviación típica del ruido, en niveles de gris.
    pub noise: f32,
    /// Contraste en 0..=1: 1 conserva negro y blanco puros, valores menores los
    /// acercan al gris.
    pub contrast: f32,
    /// Brillo añadido, en niveles de gris, positivo o negativo.
    pub brightness: f32,
    /// Intensidad del moiré, en 0..=1.
    pub moire: f32,
    /// Semilla del ruido, para que un fallo se reproduzca exacto.
    pub seed: u64,
}

impl Default for Conditions {
    fn default() -> Self {
        Self {
            frame_w: 1280,
            frame_h: 720,
            fill: 0.7,
            offset_x: 0.0,
            offset_y: 0.0,
            rotation_deg: 0.0,
            tilt_x: 0.0,
            tilt_y: 0.0,
            blur: 0.0,
            noise: 0.0,
            contrast: 1.0,
            brightness: 0.0,
            moire: 0.0,
            seed: 1,
        }
    }
}

impl Conditions {
    /// Captura ideal: de frente, enfocada, sin ruido. El caso de control.
    #[must_use]
    pub fn ideal() -> Self {
        Self::default()
    }

    /// Una webcam decente sobre una mesa: algo de inclinación, enfoque
    /// imperfecto y ruido leve, con el código bien encuadrado.
    ///
    /// El desenfoque va en relación con el tamaño de módulo, no en absoluto: un
    /// radio de 1,2 px es inofensivo sobre módulos de 8 px y devastador sobre
    /// módulos de 3. Estos valores suponen un encuadre que respeta
    /// [`crate::geometry::MIN_PIXELS_PER_MODULE`].
    #[must_use]
    pub fn typical() -> Self {
        Self {
            fill: 0.75,
            tilt_x: 0.05,
            tilt_y: 0.03,
            rotation_deg: 2.0,
            blur: 0.8,
            noise: 4.0,
            contrast: 0.9,
            moire: 0.08,
            ..Self::default()
        }
    }

    /// Condiciones malas pero todavía plausibles: mano temblorosa, poca luz,
    /// pantallas mal enfrentadas.
    #[must_use]
    pub fn harsh() -> Self {
        Self {
            fill: 0.65,
            offset_x: 0.12,
            tilt_x: 0.14,
            tilt_y: 0.10,
            rotation_deg: 6.0,
            blur: 1.6,
            noise: 10.0,
            contrast: 0.75,
            brightness: 10.0,
            moire: 0.18,
            ..Self::default()
        }
    }
}

/// Generador de ruido reproducible. No hace falta calidad criptográfica, sí
/// que la misma semilla dé siempre la misma imagen.
struct Noise(u64);

impl Noise {
    fn next_u32(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 32) as u32
    }

    /// Aproximación gaussiana sumando uniformes: por el teorema central del
    /// límite, doce uniformes menos seis dan media 0 y varianza 1. Es el truco
    /// clásico y sobra para ensuciar una imagen.
    fn gaussian(&mut self) -> f32 {
        let mut acc = 0.0f32;
        for _ in 0..12 {
            acc += self.next_u32() as f32 / u32::MAX as f32;
        }
        acc - 6.0
    }
}

/// Matriz de 3×3 en orden por filas.
type Mat3 = [f32; 9];

/// Homografía del cuadrado unidad al cuadrilátero dado.
///
/// Es la construcción clásica de Heckbert. El caso afín se trata aparte porque
/// el general divide por un determinante que allí se anula.
fn unit_square_to_quad(q: [(f32, f32); 4]) -> Mat3 {
    let (x0, y0) = q[0];
    let (x1, y1) = q[1];
    let (x2, y2) = q[2];
    let (x3, y3) = q[3];

    let sx = x0 - x1 + x2 - x3;
    let sy = y0 - y1 + y2 - y3;

    if sx.abs() < 1e-6 && sy.abs() < 1e-6 {
        return [x1 - x0, x2 - x1, x0, y1 - y0, y2 - y1, y0, 0.0, 0.0, 1.0];
    }

    let dx1 = x1 - x2;
    let dx2 = x3 - x2;
    let dy1 = y1 - y2;
    let dy2 = y3 - y2;
    let den = dx1 * dy2 - dx2 * dy1;
    if den.abs() < 1e-9 {
        // Cuadrilátero degenerado: se cae al afín antes que producir infinitos.
        return [x1 - x0, x2 - x1, x0, y1 - y0, y2 - y1, y0, 0.0, 0.0, 1.0];
    }

    let g = (sx * dy2 - dx2 * sy) / den;
    let h = (dx1 * sy - sx * dy1) / den;

    [
        x1 - x0 + g * x1,
        x3 - x0 + h * x3,
        x0,
        y1 - y0 + g * y1,
        y3 - y0 + h * y3,
        y0,
        g,
        h,
        1.0,
    ]
}

fn invert3(m: &Mat3) -> Option<Mat3> {
    let [a, b, c, d, e, f, g, h, i] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        (e * i - f * h) * inv,
        (c * h - b * i) * inv,
        (b * f - c * e) * inv,
        (f * g - d * i) * inv,
        (a * i - c * g) * inv,
        (c * d - a * f) * inv,
        (d * h - e * g) * inv,
        (b * g - a * h) * inv,
        (a * e - b * d) * inv,
    ])
}

fn apply(m: &Mat3, x: f32, y: f32) -> (f32, f32) {
    let w = m[6] * x + m[7] * y + m[8];
    if w.abs() < 1e-9 {
        return (f32::NAN, f32::NAN);
    }
    (
        (m[0] * x + m[1] * y + m[2]) / w,
        (m[3] * x + m[4] * y + m[5]) / w,
    )
}

/// Muestreo bilineal, para que el remuestreo no añada escalones que el detector
/// confundiría con módulos.
fn sample(src: &[u8], w: usize, h: usize, x: f32, y: f32) -> f32 {
    if x < 0.0 || y < 0.0 || x > (w - 1) as f32 || y > (h - 1) as f32 {
        return 255.0;
    }
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;

    let p00 = f32::from(src[y0 * w + x0]);
    let p10 = f32::from(src[y0 * w + x1]);
    let p01 = f32::from(src[y1 * w + x0]);
    let p11 = f32::from(src[y1 * w + x1]);

    let top = p00 + (p10 - p00) * fx;
    let bot = p01 + (p11 - p01) * fx;
    top + (bot - top) * fy
}

/// Desenfoque gaussiano separable. Dos pasadas de una dimensión en vez de una
/// de dos: mismo resultado, coste lineal en el radio en vez de cuadrático.
fn blur(buf: &mut [f32], w: usize, h: usize, sigma: f32) {
    if sigma <= 0.0 {
        return;
    }
    let radius = (sigma * 3.0).ceil() as isize;
    let mut kernel: Vec<f32> = (-radius..=radius)
        .map(|i| {
            let x = i as f32;
            (-(x * x) / (2.0 * sigma * sigma)).exp()
        })
        .collect();
    let suma: f32 = kernel.iter().sum();
    for k in &mut kernel {
        *k /= suma;
    }

    let mut tmp = vec![0.0f32; buf.len()];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (ki, k) in kernel.iter().enumerate() {
                let sx = (x as isize + ki as isize - radius).clamp(0, w as isize - 1) as usize;
                acc += buf[y * w + sx] * k;
            }
            tmp[y * w + x] = acc;
        }
    }
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (ki, k) in kernel.iter().enumerate() {
                let sy = (y as isize + ki as isize - radius).clamp(0, h as isize - 1) as usize;
                acc += tmp[sy * w + x] * k;
            }
            buf[y * w + x] = acc;
        }
    }
}

/// Captura sintética: matriz de módulos → frame de cámara en escala de grises.
///
/// Devuelve `(ancho, alto, píxeles)`.
#[must_use]
pub fn capture(modules: &Modules, cond: &Conditions) -> (usize, usize, Vec<u8>) {
    let (fw, fh) = (cond.frame_w, cond.frame_h);
    let lado = (fw.min(fh) as f32) * cond.fill.clamp(0.05, 1.0);

    // La resolución de rasterizado se ajusta al tamaño que va a ocupar en el
    // frame, en vez de fijarla alta y reducir mucho después.
    //
    // Reducir mucho es justo lo que produce aliasing, y el aliasing tiene una
    // firma engañosa: el código falla con enfoque perfecto y se lee al
    // desenfocar, porque el desenfoque actúa de filtro antialias. Eso llevaría
    // a concluir que el enlace mejora al desenfocar — al revés de la realidad.
    // Manteniendo la fuente cerca del destino, el supermuestreo de abajo basta.
    let modulos_totales = (modules.size() + QUIET_MODULES * 2) as f32;
    let escala = ((2.5 * lado / modulos_totales).ceil() as usize).clamp(2, 12);
    let (sw, sh, src) = modules.render_greyscale(escala, QUIET_MODULES);
    let cx = fw as f32 / 2.0 + cond.offset_x * fw as f32 / 2.0;
    let cy = fh as f32 / 2.0 + cond.offset_y * fh as f32 / 2.0;

    // Cuadrado centrado, girado y luego inclinado. La inclinación se aplica
    // acercando dos esquinas: es lo que hace una pantalla vista en escorzo.
    let r = lado / 2.0;
    let rot = cond.rotation_deg.to_radians();
    let (sin, cos) = rot.sin_cos();
    let girar = |dx: f32, dy: f32| (cx + dx * cos - dy * sin, cy + dx * sin + dy * cos);

    let tx = cond.tilt_x.clamp(0.0, 0.9);
    let ty = cond.tilt_y.clamp(0.0, 0.9);
    let quad = [
        girar(-r, -r),
        girar(r * (1.0 - tx), -r * (1.0 - ty)),
        girar(r, r),
        girar(-r * (1.0 - tx), r * (1.0 - ty)),
    ];

    let m = unit_square_to_quad(quad);
    let Some(inv) = invert3(&m) else {
        return (fw, fh, vec![255u8; fw * fh]);
    };

    // Fondo claro: una pantalla encendida en una habitación normal no está
    // rodeada de negro.
    let mut buf = vec![235.0f32; fw * fh];

    // Supermuestreo: cada píxel de destino promedia SS×SS muestras repartidas
    // por su área.
    //
    // No es un lujo. Un sensor real INTEGRA sobre el área del píxel; muestrear
    // por puntos produce aliasing al reducir, y el aliasing tiene una firma
    // muy engañosa: el código falla sin desenfoque y se lee con él, porque el
    // desenfoque hace de filtro antialias. Eso llevaría a concluir que el
    // enlace mejora al desenfocar, que es exactamente al revés de la realidad.
    const SS: usize = 3;

    // Solo se recorre la caja que ocupa el cuadrilátero: el resto del frame es
    // fondo y supermuestrearlo es trabajo tirado. En un encuadre típico eso es
    // un tercio de los píxeles.
    let xs = quad.map(|p| p.0);
    let ys = quad.map(|p| p.1);
    let bx0 = xs
        .iter()
        .cloned()
        .fold(f32::INFINITY, f32::min)
        .floor()
        .max(0.0) as usize;
    let by0 = ys
        .iter()
        .cloned()
        .fold(f32::INFINITY, f32::min)
        .floor()
        .max(0.0) as usize;
    let bx1 = (xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max).ceil() as usize + 1).min(fw);
    let by1 = (ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max).ceil() as usize + 1).min(fh);

    for py in by0..by1 {
        for px in bx0..bx1 {
            let mut acc = 0.0f32;
            let mut dentro = 0u32;
            for sy in 0..SS {
                for sx in 0..SS {
                    // Centros de SS×SS subceldas que cubren el píxel ENTERO.
                    // Con `1/(SS+1)` las muestras se apiñan en la mitad central
                    // y dejan sin cubrir la huella real del píxel, que es lo que
                    // el sensor integra.
                    let fx = px as f32 + (sx as f32 + 0.5) / SS as f32;
                    let fy = py as f32 + (sy as f32 + 0.5) / SS as f32;
                    let (u, v) = apply(&inv, fx, fy);
                    if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                        continue;
                    }
                    acc += sample(&src, sw, sh, u * (sw - 1) as f32, v * (sh - 1) as f32);
                    dentro += 1;
                }
            }
            if dentro == 0 {
                continue;
            }
            // Las muestras que caen fuera del código aportan fondo, para que el
            // borde quede suavizado en vez de escalonado.
            let total = (SS * SS) as f32;
            let fuera = total - dentro as f32;
            buf[py * fw + px] = (acc + fuera * 235.0) / total;
        }
    }

    // El moiré nace del batido entre la rejilla de la pantalla y la del sensor,
    // así que se modela como una modulación de frecuencia cercana al paso de
    // píxel, no como ruido suelto.
    if cond.moire > 0.0 {
        let amp = cond.moire.clamp(0.0, 1.0) * 40.0;
        for py in 0..fh {
            for px in 0..fw {
                let onda = ((px as f32 * 0.83).sin() * (py as f32 * 0.79).sin()) * amp;
                buf[py * fw + px] += onda;
            }
        }
    }

    blur(&mut buf, fw, fh, cond.blur);

    let mut rng = Noise(cond.seed | 1);
    let contraste = cond.contrast.clamp(0.05, 2.0);
    let mut out = vec![0u8; fw * fh];
    for (i, v) in buf.iter().enumerate() {
        // El contraste se aplica alrededor del gris medio, que es donde queda
        // el umbral de decisión del detector.
        let mut p = (v - 128.0) * contraste + 128.0 + cond.brightness;
        if cond.noise > 0.0 {
            p += rng.gaussian() * cond.noise;
        }
        out[i] = p.clamp(0.0, 255.0) as u8;
    }

    (fw, fh, out)
}
