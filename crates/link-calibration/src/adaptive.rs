//! Ajuste continuo durante la transferencia.
//!
//! El perfil negociado al principio caduca: alguien mueve un portátil, cambia
//! la luz de la habitación, el autofoco vuelve a cazar. Sin ajuste continuo, la
//! calibración inicial solo describe el primer minuto.
//!
//! La progresión es **aditiva al subir y multiplicativa al bajar**, como el
//! control de congestión de TCP y por la misma razón: pasarse al subir cuesta
//! poco si se sube despacio, mientras que bajar despacio cuando el enlace ya se
//! rompió alarga el corte. Ante la duda, retroceder rápido.

/// Qué hacer con el perfil.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adaptation {
    /// Subir el parámetro: el enlace va sobrado.
    Increase,
    /// Dejarlo como está.
    Hold,
    /// Bajarlo: el enlace está sufriendo.
    Reduce,
    /// Bajarlo mucho y avisar: el enlace está roto.
    Recover,
}

/// Tasa de acierto a partir de la cual el enlace va sobrado.
pub const EXCELLENT: f32 = 0.99;
/// Por encima de esto no hace falta tocar nada.
pub const ACCEPTABLE: f32 = 0.95;
/// Por debajo de esto el enlace no está degradado, está roto.
pub const STRUGGLING: f32 = 0.85;

/// Cuántas observaciones buenas seguidas hacen falta para atreverse a subir.
///
/// Varias, no una: un pico de suerte no es una mejora del enlace, y subir a la
/// primera produce una oscilación entre dos perfiles que cuesta más que
/// quedarse en el bajo.
pub const GOOD_STREAK_TO_INCREASE: u32 = 3;

/// Controlador aditivo-multiplicativo sobre un parámetro entero.
#[derive(Debug, Clone)]
pub struct Aimd {
    current: u32,
    min: u32,
    max: u32,
    increment: u32,
    decrease_factor: f32,
    good_streak: u32,
}

impl Aimd {
    /// # Panics
    /// Si el rango es vacío.
    #[must_use]
    pub fn new(current: u32, min: u32, max: u32, increment: u32) -> Self {
        assert!(min > 0 && min <= max, "rango inválido: {min}..={max}");
        Self {
            current: current.clamp(min, max),
            min,
            max,
            increment: increment.max(1),
            decrease_factor: 0.7,
            good_streak: 0,
        }
    }

    #[must_use]
    pub fn current(&self) -> u32 {
        self.current
    }

    #[must_use]
    pub fn good_streak(&self) -> u32 {
        self.good_streak
    }

    /// Incorpora una observación de tasa de acierto y ajusta el parámetro.
    pub fn observe(&mut self, success_rate: f32) -> Adaptation {
        let rate = success_rate.clamp(0.0, 1.0);

        if rate >= EXCELLENT {
            self.good_streak += 1;
            if self.good_streak >= GOOD_STREAK_TO_INCREASE && self.current < self.max {
                self.good_streak = 0;
                self.current = self.current.saturating_add(self.increment).min(self.max);
                return Adaptation::Increase;
            }
            return Adaptation::Hold;
        }

        self.good_streak = 0;

        if rate >= ACCEPTABLE {
            return Adaptation::Hold;
        }

        let factor = if rate >= STRUGGLING {
            self.decrease_factor
        } else {
            // Por debajo del umbral de agonía se retrocede el doble: el enlace
            // no está degradado, está roto, y bajar un escalón solo alarga el
            // corte.
            self.decrease_factor * self.decrease_factor
        };

        // Redondeo, no truncamiento: 1000 × 0,7 da 699,999… en coma flotante,
        // y truncar convierte un factor limpio en un off-by-one que luego
        // aparece como constante mágica en los tests.
        let nuevo = ((f64::from(self.current) * f64::from(factor)).round() as u32).max(self.min);
        let cambio = nuevo != self.current;
        self.current = nuevo;

        if rate < STRUGGLING {
            Adaptation::Recover
        } else if cambio {
            Adaptation::Reduce
        } else {
            // Ya en el mínimo: no hay nada que recortar, y decir «Reduce» cuando
            // no se puede reducir engañaría a quien llama.
            Adaptation::Hold
        }
    }
}
