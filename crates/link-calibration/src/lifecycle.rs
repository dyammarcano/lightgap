//! Ciclo de vida de un canal, independiente de la sesión.
//!
//! Esta separación es lo que permite que añadir el canal acústico no obligue a
//! tocar la máquina de estados de la sesión. Cada medio nace caído, se sondea,
//! se pone en marcha con un perfil, se degrada y puede volver a caer, sin que
//! la sesión sepa nada de ello.

use core::time::Duration;

/// Estado de un canal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    /// No hay enlace por este medio.
    Down,
    /// Buscando perfil.
    Probing,
    /// Operativo.
    Up,
    /// Operativo pero sufriendo; se sigue usando mientras entregue algo.
    Degraded,
}

/// Tiempo sin marcos válidos tras el cual el canal se da por caído.
pub const SILENCE_TO_DOWN: Duration = Duration::from_secs(4);

/// Cuánto tiene que sostenerse la degradación antes de declararla.
///
/// Un pico de mala suerte no es una degradación. Declararla a la primera
/// produciría un vaivén de perfiles que cuesta más que la degradación misma.
pub const DEGRADE_DEBOUNCE: Duration = Duration::from_millis(1500);

/// Qué le pasó al canal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// Empezó a sondear.
    ProbingStarted,
    /// Quedó operativo.
    CameUp,
    /// Empezó a sufrir de forma sostenida.
    Degraded,
    /// Volvió a ir bien.
    Recovered,
    /// Se cayó.
    WentDown,
}

/// Máquina de estados de un canal.
#[derive(Debug, Clone)]
pub struct Lifecycle {
    state: LinkState,
    now: Duration,
    last_good: Option<Duration>,
    degraded_since: Option<Duration>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl Lifecycle {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: LinkState::Down,
            now: Duration::ZERO,
            last_good: None,
            degraded_since: None,
        }
    }

    #[must_use]
    pub fn state(&self) -> LinkState {
        self.state
    }

    /// Arranca la búsqueda de perfil.
    pub fn start_probing(&mut self) -> Option<Transition> {
        if self.state == LinkState::Probing {
            return None;
        }
        self.state = LinkState::Probing;
        Some(Transition::ProbingStarted)
    }

    /// Declara el canal operativo con el perfil ya elegido.
    pub fn bring_up(&mut self) -> Option<Transition> {
        if self.state == LinkState::Up {
            return None;
        }
        self.state = LinkState::Up;
        self.degraded_since = None;
        self.last_good = Some(self.now);
        Some(Transition::CameUp)
    }

    /// Incorpora una observación de calidad.
    pub fn observe(&mut self, now: Duration, success_rate: f32) -> Option<Transition> {
        self.now = now;
        if matches!(self.state, LinkState::Down | LinkState::Probing) {
            return None;
        }

        if success_rate >= crate::adaptive::ACCEPTABLE {
            self.last_good = Some(now);
            self.degraded_since = None;
            if self.state == LinkState::Degraded {
                self.state = LinkState::Up;
                return Some(Transition::Recovered);
            }
            return None;
        }

        let desde = *self.degraded_since.get_or_insert(now);
        if self.state == LinkState::Up && now.saturating_sub(desde) >= DEGRADE_DEBOUNCE {
            self.state = LinkState::Degraded;
            return Some(Transition::Degraded);
        }
        None
    }

    /// Mueve el reloj sin observación nueva.
    pub fn tick(&mut self, now: Duration) -> Option<Transition> {
        self.now = now;
        if matches!(self.state, LinkState::Down | LinkState::Probing) {
            return None;
        }
        let last = self.last_good?;
        if now.saturating_sub(last) >= SILENCE_TO_DOWN {
            self.state = LinkState::Down;
            self.degraded_since = None;
            self.last_good = None;
            return Some(Transition::WentDown);
        }
        None
    }

    /// Fuerza la caída, por ejemplo al desaparecer el dispositivo de captura.
    pub fn force_down(&mut self) -> Option<Transition> {
        if self.state == LinkState::Down {
            return None;
        }
        self.state = LinkState::Down;
        self.degraded_since = None;
        self.last_good = None;
        Some(Transition::WentDown)
    }

    /// Si el canal sirve para transportar algo ahora mismo.
    #[must_use]
    pub fn usable(&self) -> bool {
        matches!(self.state, LinkState::Up | LinkState::Degraded)
    }
}
