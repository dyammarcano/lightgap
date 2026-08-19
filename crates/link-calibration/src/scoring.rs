//! Puntuación de un perfil por lo que **entrega**, no por lo que cabe.
//!
//! El error natural es quedarse con el payload más grande que se lee. Pero un
//! marco grande tarda más en mostrarse y en decodificarse, así que puede rendir
//! menos que uno mediano más rápido:
//!
//! ```text
//! 1500 B ×  5 marcos/s × 0,95 =  7 125 B/s
//!  900 B × 12 marcos/s × 0,98 = 10 584 B/s
//! ```
//!
//! Por eso lo que se compara es goodput, y se penaliza lo que el goodput bruto
//! no ve: los reintentos y la latencia.

/// Lo medido al probar un perfil.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measurement {
    /// Bytes útiles por marco, sin cabeceras.
    pub payload_bytes: u32,
    /// Marcos por segundo que el enlace sostiene de verdad.
    pub frames_per_second: f32,
    /// Fracción de marcos que se leen bien, en 0..=1.
    pub success_rate: f32,
    /// Fracción de marcos que hay que repetir, en 0..=1.
    pub retry_rate: f32,
    /// Coste de decodificar un marco, en milisegundos.
    pub decode_ms: f32,
}

impl Measurement {
    /// Bytes útiles por segundo. La medida cruda.
    #[must_use]
    pub fn goodput_bps(&self) -> f64 {
        f64::from(self.payload_bytes)
            * f64::from(self.frames_per_second.max(0.0))
            * f64::from(self.success_rate.clamp(0.0, 1.0))
    }

    /// Puntuación comparable entre perfiles.
    ///
    /// Sobre el goodput se aplican dos penalizaciones:
    ///
    /// - **Reintentos.** Cuestan ancho de banda que el goodput bruto ya
    ///   descuenta, pero además ocupan la ventana y añaden latencia, así que
    ///   pesan más de lo que su fracción sugiere.
    /// - **Latencia de decodificación.** Un perfil que rinde igual pero
    ///   responde el doble de tarde retrasa toda la realimentación y hace que
    ///   la sesión se sienta rota.
    #[must_use]
    pub fn score(&self) -> f64 {
        let base = self.goodput_bps();
        let reintentos = 1.0 - f64::from(self.retry_rate.clamp(0.0, 1.0));
        // Cien milisegundos de decodificación penalizan a la mitad; es el orden
        // de magnitud de un marco óptico, así que pasarse de ahí significa que
        // decodificar cuesta más que transmitir.
        let latencia = 1.0 / (1.0 + f64::from(self.decode_ms.max(0.0)) / 100.0);
        base * reintentos * latencia
    }
}

/// Elige el mejor de varios perfiles medidos.
///
/// Devuelve `None` con la lista vacía o si ninguno entrega nada: un perfil con
/// goodput cero no es «el menos malo», es un enlace que no funciona, y
/// devolverlo haría que la sesión arrancara condenada.
#[must_use]
pub fn best<T: Copy>(candidatos: &[(T, Measurement)]) -> Option<(T, Measurement)> {
    candidatos
        .iter()
        .filter(|(_, m)| m.score() > 0.0)
        .max_by(|a, b| {
            a.1.score()
                .partial_cmp(&b.1.score())
                .unwrap_or(core::cmp::Ordering::Equal)
        })
        .copied()
}
