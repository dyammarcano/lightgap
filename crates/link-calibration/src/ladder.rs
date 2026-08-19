//! Búsqueda del mayor parámetro que el enlace sostiene.
//!
//! Duplicar hasta fallar, luego bisecar, y quedarse por debajo con margen. Es
//! agnóstica del medio a propósito: sirve igual para negociar bytes por QR que
//! símbolos por segundo por audio. Lo único que necesita saber es «con este
//! valor, ¿qué tasa de acierto sale?».
//!
//! El margen final no es prudencia decorativa. Un enlace óptico se degrada solo
//! —alguien mueve el portátil, cambia la luz, el autofoco caza— y operar en el
//! límite exacto significa caerse a los pocos segundos de haber negociado.

/// En qué punto de la búsqueda estamos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Duplicando mientras funcione.
    Doubling,
    /// Bisecando entre el último bueno y el primer malo.
    Bisecting,
    /// Hay resultado.
    Settled,
}

/// Escalera de sondas sobre un parámetro entero.
#[derive(Debug, Clone)]
pub struct Ladder {
    min: u32,
    max: u32,
    current: u32,
    best_ok: Option<u32>,
    first_bad: Option<u32>,
    phase: Phase,
    margin_pct: u8,
    /// Tasa de acierto a partir de la cual un valor se da por bueno.
    threshold: f32,
    probes: u32,
}

/// Margen por defecto que se descuenta del mayor valor que funcionó.
pub const DEFAULT_MARGIN_PCT: u8 = 15;

/// Tasa de acierto a partir de la cual un valor se considera sostenible.
///
/// Alta a propósito. Un perfil que acierta el 90 % obliga a reenviar uno de cada
/// diez marcos, y en un medio donde cada marco cuesta cien milisegundos eso se
/// nota más que haber elegido un payload algo menor.
pub const DEFAULT_THRESHOLD: f32 = 0.97;

impl Ladder {
    /// # Panics
    /// Si el rango es vacío o el valor inicial cae fuera.
    #[must_use]
    pub fn new(min: u32, max: u32, start: u32) -> Self {
        assert!(min > 0 && min <= max, "rango inválido: {min}..={max}");
        assert!(
            (min..=max).contains(&start),
            "el arranque {start} cae fuera de {min}..={max}"
        );
        Self {
            min,
            max,
            current: start,
            best_ok: None,
            first_bad: None,
            phase: Phase::Doubling,
            margin_pct: DEFAULT_MARGIN_PCT,
            threshold: DEFAULT_THRESHOLD,
            probes: 0,
        }
    }

    #[must_use]
    pub fn with_margin(mut self, pct: u8) -> Self {
        self.margin_pct = pct.min(90);
        self
    }

    #[must_use]
    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.threshold = threshold.clamp(0.0, 1.0);
        self
    }

    /// Valor que hay que probar ahora.
    #[must_use]
    pub fn current(&self) -> u32 {
        self.current
    }

    #[must_use]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Cuántas sondas se han lanzado. Sirve para acotar la duración de la
    /// calibración: nadie sostiene dos portátiles enfrentados indefinidamente.
    #[must_use]
    pub fn probes(&self) -> u32 {
        self.probes
    }

    /// Incorpora el resultado de probar [`Ladder::current`].
    pub fn record(&mut self, success_rate: f32) {
        if self.phase == Phase::Settled {
            return;
        }
        self.probes += 1;
        let ok = success_rate >= self.threshold;

        if ok {
            self.best_ok = Some(self.best_ok.map_or(self.current, |b| b.max(self.current)));
        } else {
            self.first_bad = Some(self.first_bad.map_or(self.current, |b| b.min(self.current)));
        }

        match self.phase {
            Phase::Doubling => {
                if !ok {
                    if self.best_ok.is_none() {
                        // Falló el arranque sin que nada haya funcionado aún.
                        // Antes de rendirse hay que probar el suelo: rendirse
                        // aquí descartaría un enlace que sí da para el mínimo,
                        // solo porque se empezó a probar demasiado arriba.
                        if self.current > self.min {
                            self.current = self.min;
                        } else {
                            self.phase = Phase::Settled;
                        }
                        return;
                    }
                    self.phase = Phase::Bisecting;
                    self.step_bisect();
                    return;
                }

                if self.first_bad.is_some() {
                    // Ya se conoce un fallo por arriba: seguir duplicando lo
                    // rebasaría, así que toca bisecar.
                    self.phase = Phase::Bisecting;
                    self.step_bisect();
                    return;
                }

                if self.current >= self.max {
                    // Llegó al techo funcionando: no hay más que buscar.
                    self.phase = Phase::Settled;
                } else {
                    self.current = self.current.saturating_mul(2).min(self.max);
                }
            }
            Phase::Bisecting => self.step_bisect(),
            Phase::Settled => {}
        }
    }

    fn step_bisect(&mut self) {
        let (lo, hi) = (
            self.best_ok.unwrap_or(self.min),
            self.first_bad.unwrap_or(self.max),
        );
        // Con los extremos pegados no queda nada entre medias que probar.
        if hi <= lo + 1 {
            self.phase = Phase::Settled;
            return;
        }
        let mid = lo + (hi - lo) / 2;
        if mid == self.current {
            self.phase = Phase::Settled;
            return;
        }
        self.current = mid;
    }

    /// Corta la búsqueda y se queda con lo mejor conocido.
    ///
    /// Hace falta porque la calibración tiene presupuesto de tiempo: es
    /// preferible un perfil conservador ya que uno óptimo dentro de un minuto.
    pub fn give_up(&mut self) {
        self.phase = Phase::Settled;
    }

    /// Valor recomendado, con el margen ya descontado.
    ///
    /// `None` si no se encontró ningún valor que funcionara: en ese caso el
    /// enlace no da ni para el mínimo y hay que arreglar el encuadre, no
    /// negociar.
    #[must_use]
    pub fn settled(&self) -> Option<u32> {
        if self.phase != Phase::Settled {
            return None;
        }
        let best = self.best_ok?;
        let con_margen =
            (u64::from(best) * u64::from(100 - u16::from(self.margin_pct)) / 100) as u32;
        Some(con_margen.max(self.min))
    }
}
