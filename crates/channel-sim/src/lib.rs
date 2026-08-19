//! Enlace simulado con pérdida, retardo, jitter, duplicación y corrupción.
//!
//! Existe para que el núcleo del protocolo se pueda probar entero sin cámaras,
//! sin pantallas y sin dos máquinas. Una transferencia de 5 MB con 40 % de
//! pérdida tiene que correr en un test unitario, en milisegundos.
//!
//! Dos propiedades hacen que eso sea posible:
//!
//! - **Tiempo virtual.** El reloj lo mueve quien llama. Un retardo de 200 ms no
//!   cuesta 200 ms de test, cuesta una suma.
//! - **Aleatoriedad semillada.** Mismo `seed`, misma secuencia de pérdidas. Un
//!   fallo se reproduce exactamente, que es la diferencia entre depurar y
//!   adivinar.
//!
//! El reordenamiento no tiene mando propio: emerge del jitter, como en el medio
//! real. Si un marco sale más tarde pero le toca menos jitter, adelanta al
//! anterior. Un mando de "reordenar" separado modelaría algo que físicamente no
//! ocurre por su cuenta.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use optical_protocol::channel::{
    Channel, ChannelCaps, ChannelError, ChannelHealth, ChannelId, Direction,
};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// Cómo se porta el medio.
///
/// Los valores por defecto describen un enlace perfecto, para que un test que
/// solo quiera una tubería fiable no tenga que rellenar seis campos.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkConfig {
    /// Bytes útiles por marco.
    pub mtu: usize,
    /// Probabilidad de que un marco no llegue nunca, en 0..=1.
    pub loss: f64,
    /// Probabilidad de que un marco llegue dos veces.
    pub duplicate: f64,
    /// Probabilidad de que un marco llegue con un bit cambiado.
    pub corrupt: f64,
    /// Retardo base de extremo a extremo.
    pub latency: Duration,
    /// Variación añadida al retardo, uniforme en 0..=jitter. Es lo que produce
    /// el reordenamiento.
    pub jitter: Duration,
}

impl Default for LinkConfig {
    fn default() -> Self {
        Self {
            mtu: 1024,
            loss: 0.0,
            duplicate: 0.0,
            corrupt: 0.0,
            latency: Duration::ZERO,
            jitter: Duration::ZERO,
        }
    }
}

impl LinkConfig {
    /// Enlace perfecto con la MTU dada.
    #[must_use]
    pub fn perfect(mtu: usize) -> Self {
        Self {
            mtu,
            ..Self::default()
        }
    }

    /// Enlace óptico plausible: unos 100 ms de vuelo y jitter apreciable,
    /// porque entre mostrar un QR y decodificarlo hay varios refrescos de
    /// pantalla y varios frames de cámara.
    #[must_use]
    pub fn optical(mtu: usize, loss: f64) -> Self {
        Self {
            mtu,
            loss,
            duplicate: 0.0,
            corrupt: 0.0,
            latency: Duration::from_millis(100),
            jitter: Duration::from_millis(60),
        }
    }

    #[must_use]
    pub fn with_corruption(mut self, corrupt: f64) -> Self {
        self.corrupt = corrupt;
        self
    }

    #[must_use]
    pub fn with_duplication(mut self, duplicate: f64) -> Self {
        self.duplicate = duplicate;
        self
    }
}

/// Un marco esperando su turno de llegada.
#[derive(Debug, Clone)]
struct InFlight {
    due: Duration,
    bytes: Vec<u8>,
    /// Orden de emisión, para poder detectar reordenamiento en los tests.
    emitted: u64,
}

/// Estadísticas de lo que el medio hizo con los marcos. Sirven para comprobar
/// que el simulador simula lo que dice simular.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkStats {
    pub offered: u64,
    pub dropped: u64,
    pub duplicated: u64,
    pub corrupted: u64,
    pub delivered: u64,
}

/// Una tubería en un solo sentido.
#[derive(Debug)]
struct Wire {
    cfg: LinkConfig,
    rng: ChaCha8Rng,
    inflight: Vec<InFlight>,
    ready: VecDeque<Vec<u8>>,
    stats: LinkStats,
    emitted: u64,
    /// Orden de emisión del último marco entregado, para contar adelantamientos.
    last_delivered_order: Option<u64>,
    reorders: u64,
}

impl Wire {
    fn new(cfg: LinkConfig, seed: u64) -> Self {
        Self {
            cfg,
            rng: ChaCha8Rng::seed_from_u64(seed),
            inflight: Vec::new(),
            ready: VecDeque::new(),
            stats: LinkStats::default(),
            emitted: 0,
            last_delivered_order: None,
            reorders: 0,
        }
    }

    fn jittered(&mut self) -> Duration {
        if self.cfg.jitter.is_zero() {
            return self.cfg.latency;
        }
        let extra = self.rng.random_range(0..=self.cfg.jitter.as_nanos() as u64);
        self.cfg.latency + Duration::from_nanos(extra)
    }

    fn enqueue(&mut self, frame: &[u8], now: Duration) {
        self.stats.offered += 1;
        let order = self.emitted;
        self.emitted += 1;

        if self.rng.random::<f64>() < self.cfg.loss {
            self.stats.dropped += 1;
            return;
        }

        let mut copies = 1;
        if self.rng.random::<f64>() < self.cfg.duplicate {
            copies = 2;
            self.stats.duplicated += 1;
        }

        for _ in 0..copies {
            let mut bytes = frame.to_vec();
            if self.rng.random::<f64>() < self.cfg.corrupt && !bytes.is_empty() {
                let byte = self.rng.random_range(0..bytes.len());
                let bit = self.rng.random_range(0..8u8);
                bytes[byte] ^= 1 << bit;
                self.stats.corrupted += 1;
            }
            let due = now + self.jittered();
            self.inflight.push(InFlight {
                due,
                bytes,
                emitted: order,
            });
        }
    }

    /// Mueve a la cola de listos todo lo que ya debería haber llegado.
    fn advance(&mut self, now: Duration) {
        // Estable por `due` para que dos marcos con el mismo vencimiento
        // conserven el orden de emisión; el reordenamiento debe venir del
        // jitter, no de un detalle del contenedor.
        self.inflight
            .sort_by(|a, b| a.due.cmp(&b.due).then(a.emitted.cmp(&b.emitted)));

        let split = self.inflight.partition_point(|f| f.due <= now);
        for f in self.inflight.drain(..split) {
            if let Some(prev) = self.last_delivered_order {
                if f.emitted < prev {
                    self.reorders += 1;
                }
            }
            self.last_delivered_order = Some(f.emitted.max(self.last_delivered_order.unwrap_or(0)));
            self.stats.delivered += 1;
            self.ready.push_back(f.bytes);
        }
    }
}

/// Un extremo del enlace: escribe en una tubería y lee de la otra.
pub struct SimEndpoint {
    tx: Rc<RefCell<Wire>>,
    rx: Rc<RefCell<Wire>>,
    caps: ChannelCaps,
    health: ChannelHealth,
    now: Duration,
}

impl SimEndpoint {
    /// Estadísticas de lo que este extremo ha emitido.
    #[must_use]
    pub fn tx_stats(&self) -> LinkStats {
        self.tx.borrow().stats
    }

    /// Estadísticas de lo que este extremo ha recibido.
    #[must_use]
    pub fn rx_stats(&self) -> LinkStats {
        self.rx.borrow().stats
    }

    /// Cuántas veces un marco llegó por delante de otro emitido antes.
    #[must_use]
    pub fn rx_reorders(&self) -> u64 {
        self.rx.borrow().reorders
    }

    /// Marca un marco recibido como inválido. Lo lleva la capa de arriba,
    /// porque es la única que sabe interpretar los bytes.
    pub fn note_rejected(&mut self) {
        self.health.frames_rejected += 1;
    }

    /// Si queda algo por entregar. Un test que espera a que el enlace se vacíe
    /// necesita saberlo sin hurgar en el interior.
    #[must_use]
    pub fn rx_idle(&self) -> bool {
        let w = self.rx.borrow();
        w.inflight.is_empty() && w.ready.is_empty()
    }
}

impl Channel for SimEndpoint {
    fn caps(&self) -> ChannelCaps {
        self.caps
    }

    fn health(&self) -> ChannelHealth {
        self.health
    }

    fn send_frame(&mut self, frame: &[u8]) -> Result<(), ChannelError> {
        if frame.len() > self.caps.mtu {
            return Err(ChannelError::OverMtu {
                got: frame.len(),
                mtu: self.caps.mtu,
            });
        }
        self.tx.borrow_mut().enqueue(frame, self.now);
        self.health.frames_sent += 1;
        Ok(())
    }

    fn recv_frame(&mut self) -> Option<Vec<u8>> {
        let frame = self.rx.borrow_mut().ready.pop_front();
        if frame.is_some() {
            self.health.frames_received += 1;
            self.health.last_rx = Some(self.now);
        }
        frame
    }

    fn advance(&mut self, now: Duration) {
        self.now = now;
        self.rx.borrow_mut().advance(now);
        // La tubería de salida también avanza: al otro lado hay un extremo que
        // la lee, y su reloj puede ir por detrás del nuestro.
        self.tx.borrow_mut().advance(now);
    }
}

/// Dos extremos conectados por dos tuberías independientes.
///
/// Independientes a propósito: el diseño contempla enlaces asimétricos, donde
/// una dirección funciona y la otra no. Un solo medio compartido no podría
/// expresar eso.
pub struct SimPair {
    pub a: SimEndpoint,
    pub b: SimEndpoint,
}

impl SimPair {
    /// Construye un par con la misma configuración en ambos sentidos.
    #[must_use]
    pub fn new(cfg: LinkConfig, seed: u64) -> Self {
        Self::asymmetric(cfg.clone(), cfg, seed)
    }

    /// Construye un par con configuración distinta por sentido.
    #[must_use]
    pub fn asymmetric(a_to_b: LinkConfig, b_to_a: LinkConfig, seed: u64) -> Self {
        let mtu_ab = a_to_b.mtu;
        let mtu_ba = b_to_a.mtu;

        // Semillas distintas por sentido: con la misma, las dos direcciones
        // perderían los marcos en los mismos instantes y el test estaría
        // midiendo una coincidencia, no el protocolo.
        let ab = Rc::new(RefCell::new(Wire::new(a_to_b, seed)));
        let ba = Rc::new(RefCell::new(Wire::new(
            b_to_a,
            seed ^ 0x9e37_79b9_7f4a_7c15,
        )));

        let caps = |mtu: usize| ChannelCaps {
            id: ChannelId::Simulated,
            mtu,
            direction: Direction::Bidirectional,
            nominal_bps: 0,
            nominal_latency: Duration::ZERO,
        };

        Self {
            a: SimEndpoint {
                tx: Rc::clone(&ab),
                rx: Rc::clone(&ba),
                caps: caps(mtu_ab),
                health: ChannelHealth::default(),
                now: Duration::ZERO,
            },
            b: SimEndpoint {
                tx: ba,
                rx: ab,
                caps: caps(mtu_ba),
                health: ChannelHealth::default(),
                now: Duration::ZERO,
            },
        }
    }

    /// Mueve el reloj de los dos extremos a la vez.
    pub fn advance(&mut self, now: Duration) {
        self.a.advance(now);
        self.b.advance(now);
    }
}
