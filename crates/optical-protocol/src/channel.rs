//! Abstracción del medio físico.
//!
//! Un canal transporta **marcos de bytes**, no PDUs. Un QR entrega exactamente
//! los bytes que se codificaron; interpretarlos es trabajo de [`crate::wire`].
//! Si el canal supiera de PDUs, la abstracción tendría una fuga justo por donde
//! después entra el canal acústico —que empaqueta de otra forma— y por donde
//! entraría un socket TCP, que ni siquiera tiene marcos.
//!
//! Los canales tampoco deciden *qué* se envía. Eso lo hace el multiplexor de la
//! Fase 6, mirando [`ChannelHealth`] en vivo.

use core::fmt;
use core::time::Duration;

/// Qué medio físico es. Se usa para enrutar por clase de prioridad y para que
/// la telemetría distinga de dónde vienen los números.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelId {
    /// Pantalla ⇄ cámara.
    Visual,
    /// Altavoz ⇄ micrófono.
    Acoustic,
    /// Dos instancias en la misma máquina, para probar el protocolo sin
    /// hardware óptico.
    Loopback,
    /// Enlace simulado en memoria, solo en tests.
    Simulated,
}

/// En qué sentidos sirve el canal.
///
/// La asimetría no es hipotética: la calibración puede concluir que el audio
/// funciona de A a B pero no al revés, porque los micrófonos y altavoces de
/// las dos máquinas no tienen por qué parecerse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    TxOnly,
    RxOnly,
    Bidirectional,
}

impl Direction {
    #[must_use]
    pub const fn can_tx(self) -> bool {
        matches!(self, Self::TxOnly | Self::Bidirectional)
    }

    #[must_use]
    pub const fn can_rx(self) -> bool {
        matches!(self, Self::RxOnly | Self::Bidirectional)
    }
}

/// Lo que el canal promete. Se fija al negociar el perfil y cambia solo con una
/// recalibración.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelCaps {
    pub id: ChannelId,
    /// Bytes útiles por marco. Es el techo del tamaño de PDU en este canal.
    pub mtu: usize,
    pub direction: Direction,
    /// Rendimiento nominal del perfil negociado, para que el multiplexor pueda
    /// repartir sin medir de nuevo.
    pub nominal_bps: u64,
    /// Retardo típico de un marco de extremo a extremo.
    pub nominal_latency: Duration,
}

/// Cómo va el canal *ahora*. Es lo que mira el multiplexor para degradar.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ChannelHealth {
    pub frames_sent: u64,
    pub frames_received: u64,
    /// Marcos que llegaron pero no pasaron validación.
    ///
    /// Es un subconjunto de `frames_received`: todo marco recogido cuenta como
    /// recibido, y además como rechazado si resultó inválido. Separarlos de los
    /// perdidos es deliberado: perder marcos indica encuadre o ruido, recibirlos
    /// corruptos indica que el perfil es demasiado agresivo. Piden arreglos
    /// distintos.
    pub frames_rejected: u64,
    /// Cuándo llegó el último marco válido, en tiempo de sesión.
    ///
    /// Tiempo de sesión y no `Instant` porque el núcleo es sans-io: quien manda
    /// el reloj es quien llama, y en tests ese reloj es virtual para que una
    /// transferencia de 5 MB no tarde 5 MB de segundos.
    pub last_rx: Option<Duration>,
}

impl ChannelHealth {
    /// Proporción de marcos recibidos que hubo que descartar, en 0..=1.
    ///
    /// El divisor es `frames_received` a secas, porque los rechazados ya están
    /// contados ahí. Sumarlos aparte los contaría dos veces y un canal 100 %
    /// basura reportaría 0,5 — suficiente para que el multiplexor de la Fase 6
    /// lo siguiera usando.
    ///
    /// Devuelve 0 sin nada recibido: un canal del que aún no se sabe nada no es
    /// un canal malo, y tratarlo como tal haría que el multiplexor lo
    /// descartara antes de darle una oportunidad.
    #[must_use]
    pub fn rejection_rate(&self) -> f32 {
        if self.frames_received == 0 {
            return 0.0;
        }
        self.frames_rejected as f32 / self.frames_received as f32
    }
}

/// Por qué no se pudo entregar un marco al medio.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChannelError {
    #[error("marco de {got} B supera la MTU de {mtu} B del canal")]
    OverMtu { got: usize, mtu: usize },

    #[error("el canal no transmite en este sentido")]
    NotTransmitting,

    #[error("el canal está caído")]
    Down,

    #[error("la cola de salida está llena")]
    Backpressure,
}

/// Un medio por el que viajan marcos.
///
/// Deliberadamente no async: el núcleo es sans-io y no debe elegir runtime. Los
/// drivers reales (cámara, audio) corren en sus propias tasks y alimentan una
/// implementación de este trait por cola.
pub trait Channel {
    fn caps(&self) -> ChannelCaps;

    fn health(&self) -> ChannelHealth;

    /// Encola un marco ya serializado.
    ///
    /// Que devuelva `Ok` significa aceptado para transmisión, no entregado. En
    /// un canal óptico no existe la confirmación a este nivel: eso lo resuelve
    /// la capa de fiabilidad.
    fn send_frame(&mut self, frame: &[u8]) -> Result<(), ChannelError>;

    /// Recoge el siguiente marco recibido, si lo hay. No bloquea.
    fn recv_frame(&mut self) -> Option<Vec<u8>>;

    /// Avisa del paso del tiempo. Los canales con retardo modelado lo necesitan
    /// para decidir qué marcos ya deberían haber llegado.
    fn advance(&mut self, _now: Duration) {}
}

impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Visual => "visual",
            Self::Acoustic => "acústico",
            Self::Loopback => "loopback",
            Self::Simulated => "simulado",
        };
        f.write_str(s)
    }
}
