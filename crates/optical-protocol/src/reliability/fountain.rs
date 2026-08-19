//! Código de fuente (RaptorQ): el emisor no espera a nadie.
//!
//! El emisor genera símbolos codificados indefinidamente. El receptor
//! reconstruye en cuanto reúne suficientes, y da igual *cuáles*: cualquier
//! subconjunto de tamaño suficiente sirve. Eso elimina el round-trip óptico,
//! que en este medio es el coste dominante — mostrar un QR, capturarlo,
//! decodificarlo y contestar con otro QR cuesta cientos de milisegundos.
//!
//! La consecuencia práctica: no hay retransmisiones que pedir, ni huecos que
//! rastrear, ni ventana que gestionar. El único mensaje de vuelta que importa
//! es «ya está, para».
//!
//! El emisor sí necesita esa confirmación: sin ella emitiría para siempre. Es
//! el único punto donde la fuente depende del canal de retorno, y por eso el
//! diseño replica ese mensaje por ambos canales cuando hay dos.

use std::collections::VecDeque;

use raptorq::{Decoder, Encoder, EncodingPacket, ObjectTransmissionInformation};

use super::{Feedback, Progress, Receiver, RecvError, Sender, Symbol};

/// Bytes que RaptorQ antepone a cada símbolo (el `PayloadId`).
///
/// Importa para dimensionar: si el canal admite un payload de P bytes, el
/// tamaño de símbolo utilizable es P − 4.
pub const PACKET_ID_LEN: usize = 4;

/// Cuántos símbolos de reparación se generan por tanda y por bloque.
///
/// Generarlos de uno en uno desperdiciaría la preparación del codificador;
/// generarlos de mil en mil ocuparía memoria que quizá no haga falta si el
/// receptor confirma pronto.
const REPAIR_BATCH: u32 = 64;

/// Tamaño de símbolo utilizable dentro de un payload de `max_payload` bytes.
///
/// No recorta a ningún alineamiento: [`plan`] construye el OTI eligiendo el
/// alineamiento que encaje con el tamaño, así que cualquier valor sirve y no se
/// pierde ni un byte por marco. (`ObjectTransmissionInformation::with_defaults`
/// sí recortaba a múltiplos de 8 — de ahí venía un bug en el que el receptor
/// validaba contra el tamaño pedido y rechazaba **todos** los símbolos.)
///
/// Devuelve `None` si no queda sitio ni para un byte de datos.
#[must_use]
pub fn symbol_size_for(max_payload: usize) -> Option<u16> {
    let usable = u16::try_from(max_payload.checked_sub(PACKET_ID_LEN)?).ok()?;
    (usable > 0).then_some(usable)
}

/// Cuántos símbolos de fuente puede tener como mucho un bloque.
///
/// Este número decide el coste de decodificar, y no es un detalle menor:
/// RaptorQ resuelve cada bloque por eliminación gaussiana sobre GF(256), que
/// crece muy por encima de lineal con K. Medido en este proyecto, dejar que un
/// objeto de 5 MB cayera en un solo bloque de ~6000 símbolos costaba más de
/// nueve minutos de CPU al reconstruir. Troceado en bloques de ~1000 baja a
/// segundos, a cambio de un poco menos de eficiencia de codificación.
///
/// Es un compromiso de experiencia de uso: nadie espera nueve minutos mirando
/// una barra parada después de haber sostenido dos portátiles enfrentados.
pub const MAX_SYMBOLS_PER_BLOCK: u32 = 1024;

/// Parámetros de transmisión de un objeto.
///
/// **Viajan por el cable**, en los metadatos de la transferencia: son 12 bytes
/// una sola vez. Una versión anterior los derivaba en ambos lados para
/// ahorrárselos, pero era mal negocio — ataba el número de bloques a lo que
/// decidiera `with_defaults`, que es justo el parámetro que hay que poder
/// ajustar. Doce bytes por transferencia no se comparan con minutos de espera.
///
/// # Panics
/// Con `total_len` de cero: RaptorQ divide por el número de símbolos y revienta
/// dentro de la librería con un mensaje que no dice de dónde viene. Las rutas
/// normales del emisor y del receptor ni llegan a llamarlo — un objeto vacío se
/// resuelve sin tocar RaptorQ.
#[must_use]
pub fn plan(total_len: u64, symbol_size: u16) -> ObjectTransmissionInformation {
    assert!(
        total_len > 0,
        "RaptorQ no admite objetos vacíos; trátalos antes de llegar aquí"
    );
    assert!(symbol_size > 0, "el tamaño de símbolo no puede ser cero");

    // El alineamiento tiene que dividir al tamaño de símbolo: es una
    // precondición que `ObjectTransmissionInformation::new` comprueba con un
    // assert propio, y con 1 siempre se cumple.
    let alignment: u8 = if symbol_size.is_multiple_of(8) { 8 } else { 1 };

    let total_symbols = total_len.div_ceil(u64::from(symbol_size));
    let blocks = total_symbols.div_ceil(u64::from(MAX_SYMBOLS_PER_BLOCK));
    // `source_blocks` es un u8; por encima de 255 bloques se aceptan bloques
    // más grandes antes que producir un OTI inválido.
    let blocks = blocks.clamp(1, 255) as u8;

    ObjectTransmissionInformation::new(total_len, symbol_size, blocks, 1, alignment)
}

/// Cuántos símbolos de fuente tiene un objeto. Es el mínimo teórico de símbolos
/// que hay que reunir; en la práctica hacen falta unos pocos más.
fn source_symbols(total_len: u64, symbol_size: u16) -> u32 {
    if symbol_size == 0 {
        return 0;
    }
    total_len.div_ceil(u64::from(symbol_size)) as u32
}

/// El lado que tiene el objeto y emite símbolos sin descanso.
pub struct FountainSender {
    /// `None` para un objeto vacío.
    ///
    /// No es una optimización: `raptorq` divide por el número de símbolos al
    /// construirse, y con longitud cero entra en pánico dentro de la librería
    /// (util.rs:45). Un archivo vacío es algo legítimo de transferir, así que el
    /// codificador simplemente no llega a existir.
    encoder: Option<Encoder>,
    symbol_size: u16,
    source_symbols: u32,
    pending: VecDeque<Vec<u8>>,
    /// Identificador del próximo símbolo de reparación a generar.
    next_repair_id: u32,
    source_emitted: bool,
    emitted: u32,
    peer_received: u32,
    complete: bool,
}

impl FountainSender {
    #[must_use]
    pub fn new(object: &[u8], symbol_size: u16) -> Self {
        let base = Self {
            encoder: None,
            symbol_size,
            source_symbols: 0,
            pending: VecDeque::new(),
            next_repair_id: 0,
            source_emitted: false,
            emitted: 0,
            peer_received: 0,
            complete: false,
        };

        // Un objeto vacío no llega a tocar RaptorQ: `with_defaults` divide por
        // el número de símbolos y entra en pánico con longitud cero.
        if object.is_empty() {
            return base;
        }

        let config = plan(object.len() as u64, symbol_size);
        // El tamaño efectivo lo fija el OTI, no el que se pidió: RaptorQ lo
        // alinea a la baja y usar el pedido descuadraría todos los cálculos.
        let effective = config.symbol_size();
        Self {
            encoder: Some(Encoder::new(object, config)),
            symbol_size: effective,
            source_symbols: source_symbols(object.len() as u64, effective),
            ..base
        }
    }

    /// Cuántos símbolos se han emitido en total. Con fuente puede superar
    /// ampliamente el número de símbolos de fuente, y eso es el mecanismo
    /// funcionando, no un síntoma.
    #[must_use]
    pub fn emitted(&self) -> u32 {
        self.emitted
    }

    #[must_use]
    pub fn source_symbols(&self) -> u32 {
        self.source_symbols
    }

    /// Tamaño de símbolo **efectivo**, ya alineado por RaptorQ. Puede ser menor
    /// que el que se pidió al construir.
    #[must_use]
    pub fn symbol_size(&self) -> u16 {
        self.symbol_size
    }

    /// Bytes que ocupa cada símbolo serializado, identificador incluido. Es lo
    /// que tiene que caber en el payload de un PDU.
    #[must_use]
    pub fn wire_len(&self) -> usize {
        usize::from(self.symbol_size) + PACKET_ID_LEN
    }

    /// Parámetros de transmisión para mandar en los metadatos.
    ///
    /// `None` para un objeto vacío: no hay plan que mandar, y el receptor lo
    /// resuelve sabiendo únicamente que la longitud es cero.
    #[must_use]
    pub fn oti_bytes(&self) -> Option<[u8; 12]> {
        self.encoder.as_ref().map(|e| e.get_config().serialize())
    }

    /// Rellena la cola. Primero los símbolos de fuente —son los que decodifican
    /// más barato— y a partir de ahí, reparación sin fin.
    fn refill(&mut self) {
        let Some(encoder) = self.encoder.as_ref() else {
            return;
        };

        if !self.source_emitted {
            self.source_emitted = true;
            for packet in encoder.get_encoded_packets(0) {
                self.pending.push_back(packet.serialize());
            }
            if !self.pending.is_empty() {
                return;
            }
        }

        let start = self.next_repair_id;
        self.next_repair_id = self.next_repair_id.saturating_add(REPAIR_BATCH);
        for block in encoder.get_block_encoders() {
            for packet in block.repair_packets(start, REPAIR_BATCH) {
                self.pending.push_back(packet.serialize());
            }
        }
    }
}

impl Sender for FountainSender {
    fn next_symbol(&mut self, max_payload: usize) -> Option<Symbol> {
        if self.complete {
            return None;
        }
        // Un objeto vacío no tiene nada que emitir; sin esta salida el relleno
        // giraría para siempre produciendo tandas vacías.
        if self.source_symbols == 0 {
            return None;
        }

        if self.pending.is_empty() {
            self.refill();
        }

        let bytes = self.pending.front()?;
        if bytes.len() > max_payload {
            return None;
        }

        let bytes = self.pending.pop_front()?;
        let id = self.emitted;
        self.emitted = self.emitted.saturating_add(1);
        Some(Symbol { id, bytes })
    }

    fn on_feedback(&mut self, feedback: &Feedback) {
        // Una realimentación de ARQ sobre una transferencia de fuente indica un
        // fallo de negociación. Se ignora en vez de tumbar la sesión.
        let Feedback::Fountain { complete, received } = feedback else {
            return;
        };
        self.peer_received = self.peer_received.max(*received);
        if *complete {
            self.complete = true;
        }
    }

    fn is_complete(&self) -> bool {
        self.complete || self.source_symbols == 0
    }

    fn progress(&self) -> Progress {
        // El progreso del emisor es lo que el receptor dice tener, no lo que él
        // ha soltado: con fuente, emitir más no significa avanzar.
        Progress {
            have: u64::from(self.peer_received.min(self.source_symbols)),
            need: u64::from(self.source_symbols),
        }
    }
}

/// El lado que reúne símbolos hasta poder reconstruir.
pub struct FountainReceiver {
    /// `None` para un objeto vacío, por la misma razón que en el emisor:
    /// construir el decodificador con longitud cero hace que `raptorq` divida
    /// por cero.
    decoder: Option<Decoder>,
    symbol_size: u16,
    source_symbols: u32,
    received: u32,
    object: Option<Vec<u8>>,
    /// Separado de `object` a propósito: `take_object` vacía el objeto, y si
    /// `is_complete` dependiera de él, el receptor pasaría a declararse
    /// incompleto justo después de entregar el resultado — y su realimentación
    /// le diría al emisor que siguiera emitiendo.
    complete: bool,
    taken: bool,
}

impl FountainReceiver {
    #[must_use]
    pub fn new(total_len: u64, symbol_size: u16) -> Self {
        // Un objeto vacío ya está reconstruido, y además no se puede construir
        // el decodificador para él: `with_defaults` entra en pánico con
        // longitud cero. Resolverlo aquí evita las dos cosas.
        if total_len == 0 {
            return Self {
                decoder: None,
                symbol_size,
                source_symbols: 0,
                received: 0,
                object: Some(Vec::new()),
                complete: true,
                taken: false,
            };
        }

        Self::from_config(plan(total_len, symbol_size))
    }

    /// Construye a partir de los parámetros que mandó el emisor.
    ///
    /// Es la vía preferente: usar exactamente el plan del emisor elimina la
    /// posibilidad de que los dos lados troceen el objeto de forma distinta.
    #[must_use]
    pub fn from_oti_bytes(oti: &[u8; 12]) -> Self {
        Self::from_config(ObjectTransmissionInformation::deserialize(oti))
    }

    fn from_config(config: ObjectTransmissionInformation) -> Self {
        let total_len = config.transfer_length();
        // El tamaño efectivo lo fija el OTI, no el que se pidió. Contar los
        // símbolos de fuente con el pedido daría una cuenta distinta a la del
        // emisor, y el progreso mentiría en el sentido peligroso: por lo bajo.
        let effective = config.symbol_size();
        Self {
            decoder: Some(Decoder::new(config)),
            symbol_size: effective,
            source_symbols: source_symbols(total_len, effective),
            received: 0,
            object: None,
            complete: false,
            taken: false,
        }
    }

    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// Cuántos símbolos se han incorporado, útiles o no.
    #[must_use]
    pub fn received(&self) -> u32 {
        self.received
    }

    /// Tamaño de símbolo **efectivo**, el que fija el OTI.
    #[must_use]
    pub fn symbol_size(&self) -> u16 {
        self.symbol_size
    }

    /// Cuántos símbolos de fuente tiene el objeto según el plan. Es el mínimo
    /// teórico a reunir; en la práctica hacen falta unos pocos más.
    #[must_use]
    pub fn source_symbols_expected(&self) -> u32 {
        self.source_symbols
    }

    /// Bytes que debe traer cada símbolo serializado, identificador incluido.
    #[must_use]
    pub fn wire_len(&self) -> usize {
        usize::from(self.symbol_size) + PACKET_ID_LEN
    }
}

impl Receiver for FountainReceiver {
    fn on_symbol(&mut self, symbol: &Symbol) -> Result<(), RecvError> {
        // `EncodingPacket::deserialize` indexa los cuatro primeros bytes sin
        // comprobarlos: con un buffer más corto entra en pánico. Un símbolo
        // truncado es algo que este medio produce de verdad, así que se filtra
        // aquí antes de dejarle ver nada.
        let expected = usize::from(self.symbol_size) + PACKET_ID_LEN;
        if symbol.bytes.len() != expected {
            return Err(RecvError::SymbolSize {
                got: symbol.bytes.len(),
                expected,
            });
        }

        self.received = self.received.saturating_add(1);

        if self.complete {
            // Ya está reconstruido; seguir alimentando al decodificador no
            // aporta nada y cuesta.
            return Ok(());
        }

        let Some(decoder) = self.decoder.as_mut() else {
            return Ok(());
        };
        let packet = EncodingPacket::deserialize(&symbol.bytes);
        if let Some(obj) = decoder.decode(packet) {
            self.object = Some(obj);
            self.complete = true;
        }
        Ok(())
    }

    fn feedback(&self) -> Feedback {
        Feedback::Fountain {
            complete: self.complete,
            received: self.received,
        }
    }

    fn take_object(&mut self) -> Option<Vec<u8>> {
        if self.taken {
            return None;
        }
        let obj = self.object.take()?;
        self.taken = true;
        Some(obj)
    }

    fn progress(&self) -> Progress {
        if self.complete {
            return Progress {
                have: u64::from(self.source_symbols),
                need: u64::from(self.source_symbols),
            };
        }
        Progress {
            have: u64::from(self.received.min(self.source_symbols)),
            need: u64::from(self.source_symbols),
        }
    }
}
