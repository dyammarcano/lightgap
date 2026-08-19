//! Máquina de estados de la sesión.
//!
//! Es deliberadamente pequeña. El boceto original de este proyecto metía
//! `AudioNoiseMeasurement`, `AudioFrequencySweep` y compañía como estados de
//! *sesión*, lo que ata la sesión al audio: añadir un tercer medio obligaría a
//! editar esta máquina. Aquí la calibración es asunto de cada canal, que lleva
//! su propio ciclo de vida ([`crate::channel`]), y la sesión solo sabe si hay
//! par, si está negociando y si está transfiriendo.
//!
//! No hace entrada/salida: se le entregan PDUs, se le pregunta qué transmitir y
//! se le avisa del paso del tiempo. Quien manda el reloj es quien llama.

use core::fmt;
use core::time::Duration;

use crate::wire::{Flags, Pdu, PduKind};

/// Identificador de par. Se sortea al arrancar la aplicación.
///
/// Dieciséis bytes para que dos instancias no coincidan por accidente: con
/// menos, una colisión dejaría la elección de líder sin desempate y las dos
/// máquinas se quedarían esperándose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PeerId(pub [u8; 16]);

impl PeerId {
    #[must_use]
    pub const fn from_bytes(b: [u8; 16]) -> Self {
        Self(b)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Los cuatro primeros bytes bastan para distinguir en un registro.
        for b in &self.0[..4] {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

/// Quién dirige la sesión.
///
/// Dos aplicaciones idénticas mirándose necesitan un desempate: si ninguna
/// arranca la calibración, se quedan esperando; si las dos emiten el barrido
/// acústico a la vez, cada micrófono capta su propio altavoz y la medida no
/// vale nada. El criterio es el `PeerId` menor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Secuencia la calibración y fija el identificador de sesión.
    Leader,
    /// Sigue el ritmo que marca el líder.
    Follower,
}

/// En qué punto está la sesión.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Emitiendo `Hello` y buscando a alguien enfrente.
    Discovering,
    /// Los dos se han visto; ya hay papeles repartidos.
    Peered,
    /// Acordando perfiles de canal.
    Negotiating,
    /// Moviendo datos.
    Active,
    /// Cerrando de mutuo acuerdo.
    Closing,
    /// Terminada.
    Closed,
}

/// Lo que le pasa a quien usa la sesión.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Se ha visto al otro lado y se han repartido papeles.
    PeerDiscovered { peer: PeerId, role: Role },
    /// Se pasó a negociar perfiles.
    NegotiationStarted,
    /// Todo listo para transferir.
    Ready,
    /// El par lleva demasiado tiempo callado.
    PeerLost,
    /// Sesión terminada, con o sin acuerdo.
    Closed,
}

/// Cada cuánto se repite el `Hello` mientras se busca par.
///
/// Lento a propósito: durante el descubrimiento nadie está encuadrado todavía,
/// y un QR que cambia deprisa es más difícil de enganchar que uno que se queda
/// quieto medio segundo.
pub const HELLO_INTERVAL: Duration = Duration::from_millis(500);

/// Sin noticias durante este tiempo, se da al par por perdido.
///
/// Generoso frente al ritmo de `Hello`: un enlace óptico pierde marcos por
/// rachas —una mano que pasa, un reflejo— y cortar a la primera racha haría
/// que la sesión se cayera constantemente.
pub const PEER_TIMEOUT: Duration = Duration::from_secs(5);

/// La sesión.
#[derive(Debug)]
pub struct Session {
    local: PeerId,
    remote: Option<PeerId>,
    session_id: u64,
    state: State,
    role: Option<Role>,
    now: Duration,
    last_rx: Option<Duration>,
    next_hello: Duration,
    pending: Option<Pdu>,
}

impl Session {
    #[must_use]
    pub fn new(local: PeerId) -> Self {
        Self {
            local,
            remote: None,
            session_id: 0,
            state: State::Discovering,
            role: None,
            now: Duration::ZERO,
            last_rx: None,
            next_hello: Duration::ZERO,
            pending: None,
        }
    }

    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }

    #[must_use]
    pub fn role(&self) -> Option<Role> {
        self.role
    }

    #[must_use]
    pub fn peer(&self) -> Option<PeerId> {
        self.remote
    }

    /// Identificador de sesión acordado. Cero mientras no haya par.
    #[must_use]
    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    #[must_use]
    pub fn local(&self) -> PeerId {
        self.local
    }

    /// Deriva el identificador de sesión de los dos identificadores de par.
    ///
    /// Determinista y simétrico: los dos lados llegan al mismo número sin
    /// negociarlo, así que no hace falta un intercambio extra ni que el líder
    /// lo imponga. Se mezcla con el mismo entero que usa `SimPair` para
    /// separar semillas, elegido por tener buena dispersión de bits.
    fn derive_session_id(a: &PeerId, b: &PeerId) -> u64 {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let mut acc: u64 = 0x9e37_79b9_7f4a_7c15;
        for byte in lo.0.iter().chain(hi.0.iter()) {
            acc ^= u64::from(*byte);
            acc = acc.wrapping_mul(0x0100_0000_01b3);
        }
        // Cero queda reservado para «todavía no hay sesión».
        acc | 1
    }

    fn hello(&self) -> Pdu {
        Pdu {
            session_id: self.session_id,
            kind: PduKind::Hello,
            flags: Flags::SYN,
            seq: 0,
            ack: 0,
            payload: self.local.0.to_vec(),
        }
    }

    /// Incorpora un PDU recibido.
    pub fn handle_incoming(&mut self, pdu: &Pdu) -> Vec<Event> {
        if self.state == State::Closed {
            return Vec::new();
        }
        self.last_rx = Some(self.now);
        let mut events = Vec::new();

        match pdu.kind {
            PduKind::Hello => {
                let Ok(bytes) = <[u8; 16]>::try_from(pdu.payload.as_slice()) else {
                    // Un `Hello` con identificador de otro tamaño es de otra
                    // versión del protocolo. Se ignora: no hay forma de repartir
                    // papeles con alguien cuyo identificador no se entiende.
                    return events;
                };
                let remote = PeerId::from_bytes(bytes);
                if remote == self.local {
                    // Verse a uno mismo (un espejo, o la propia pantalla en el
                    // encuadre) no es descubrir un par.
                    return events;
                }

                if self.remote != Some(remote) {
                    self.remote = Some(remote);
                    self.session_id = Self::derive_session_id(&self.local, &remote);
                    let role = if self.local < remote {
                        Role::Leader
                    } else {
                        Role::Follower
                    };
                    self.role = Some(role);
                    self.state = State::Peered;
                    events.push(Event::PeerDiscovered { peer: remote, role });
                }
            }

            PduKind::Capabilities if self.state == State::Peered => {
                self.state = State::Negotiating;
                events.push(Event::NegotiationStarted);
            }

            PduKind::Cancel => {
                self.state = State::Closed;
                events.push(Event::Closed);
            }

            _ => {}
        }

        events
    }

    /// Qué transmitir ahora, si toca algo.
    pub fn poll_transmit(&mut self) -> Option<Pdu> {
        if let Some(pdu) = self.pending.take() {
            return Some(pdu);
        }
        // El `Hello` se sigue repitiendo tras encontrar par: el otro lado puede
        // no haber visto el nuestro todavía, y el descubrimiento no es
        // simétrico en el tiempo.
        if matches!(self.state, State::Discovering | State::Peered) && self.now >= self.next_hello {
            self.next_hello = self.now + HELLO_INTERVAL;
            return Some(self.hello());
        }
        None
    }

    /// Mueve el reloj. Devuelve lo que el paso del tiempo haya provocado.
    pub fn handle_timeout(&mut self, now: Duration) -> Vec<Event> {
        self.now = now;
        let mut events = Vec::new();

        if matches!(self.state, State::Closed | State::Discovering) {
            return events;
        }

        if let Some(last) = self.last_rx {
            if now.saturating_sub(last) >= PEER_TIMEOUT {
                self.remote = None;
                self.role = None;
                self.session_id = 0;
                self.state = State::Discovering;
                self.last_rx = None;
                // Se reanuda el `Hello` de inmediato: quien acaba de perder al
                // par es quien más prisa tiene por volver a anunciarse.
                self.next_hello = now;
                events.push(Event::PeerLost);
            }
        }

        events
    }

    /// Declara acordados los perfiles y pasa a transferir.
    ///
    /// Lo decide la capa de calibración, no la sesión: la sesión no sabe qué es
    /// un perfil visual ni uno acústico, y meterle ese conocimiento sería
    /// reintroducir justo el acoplamiento que este diseño evita.
    pub fn mark_ready(&mut self) -> Vec<Event> {
        if matches!(self.state, State::Peered | State::Negotiating) {
            self.state = State::Active;
            return vec![Event::Ready];
        }
        Vec::new()
    }

    /// Cierra la sesión y deja preparado el aviso al par.
    pub fn close(&mut self) -> Vec<Event> {
        if self.state == State::Closed {
            return Vec::new();
        }
        self.pending = Some(Pdu {
            session_id: self.session_id,
            kind: PduKind::Cancel,
            flags: Flags::FIN,
            seq: 0,
            ack: 0,
            payload: Vec::new(),
        });
        self.state = State::Closed;
        vec![Event::Closed]
    }
}
