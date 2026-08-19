//! Ventana deslizante con retransmisión selectiva.
//!
//! El objeto se parte en chunks de tamaño fijo indexados densamente. El emisor
//! mantiene una ventana de chunks en vuelo; el receptor confirma de forma
//! acumulativa y además lista los huecos que ve por delante del acumulado.
//!
//! El coste de esta estrategia en un canal óptico es real: cada confirmación
//! necesita que el receptor muestre un QR y que el emisor lo capture y lo
//! decodifique. Por eso la ventana importa tanto — con ventana 1 esto degenera
//! en stop-and-wait, que es lo que hace inutilizable el enlace.

use super::{Feedback, Progress, Receiver, RecvError, Sender, Symbol};

/// Cuántos huecos caben en una realimentación.
///
/// Acotado porque un `Feedback` viaja en el payload de un PDU, y ese payload
/// tiene que caber en un marco del canal. Un ACK que no entra en un QR no es un
/// ACK. Si hay más huecos que este límite se mandan los más antiguos: son los
/// que bloquean el avance del acumulado.
pub const MAX_MISSING_REPORTED: usize = 32;

/// Ventana inicial, en chunks. La negociación de la Fase 3 la ajusta.
pub const DEFAULT_WINDOW: u32 = 16;

/// El lado que tiene el objeto.
#[derive(Debug)]
pub struct ArqSender {
    object: Vec<u8>,
    chunk_size: usize,
    total_chunks: u32,
    /// Primer chunk sin confirmar.
    base: u32,
    /// Primer chunk que no se ha enviado nunca.
    next: u32,
    window: u32,
    /// Huecos que el receptor ha pedido, en orden de llegada.
    retransmit: Vec<u32>,
}

impl ArqSender {
    /// # Panics
    /// Si `chunk_size` es cero.
    #[must_use]
    pub fn new(object: Vec<u8>, chunk_size: usize) -> Self {
        assert!(chunk_size > 0, "el tamaño de chunk no puede ser cero");
        let total_chunks = object.len().div_ceil(chunk_size) as u32;
        Self {
            object,
            chunk_size,
            total_chunks,
            base: 0,
            next: 0,
            window: DEFAULT_WINDOW,
            retransmit: Vec::new(),
        }
    }

    #[must_use]
    pub fn total_chunks(&self) -> u32 {
        self.total_chunks
    }

    fn chunk(&self, id: u32) -> Option<Vec<u8>> {
        if id >= self.total_chunks {
            return None;
        }
        let start = id as usize * self.chunk_size;
        let end = (start + self.chunk_size).min(self.object.len());
        Some(self.object[start..end].to_vec())
    }
}

impl Sender for ArqSender {
    fn next_symbol(&mut self, max_payload: usize) -> Option<Symbol> {
        // Los huecos primero: son los que bloquean el acumulado, y hasta que se
        // llenen el receptor no puede avanzar por mucho que reciba lo demás.
        if let Some(&id) = self.retransmit.first() {
            let bytes = self.chunk(id)?;
            // Un chunk no se puede trocear: el índice es la identidad del dato.
            // Si no cabe, el perfil está mal negociado, y no mandar nada es
            // mejor que mandar algo que el receptor no sabrá recomponer.
            if bytes.len() > max_payload {
                return None;
            }
            self.retransmit.remove(0);
            return Some(Symbol { id, bytes });
        }

        if self.next < self.total_chunks && self.next < self.base.saturating_add(self.window) {
            let id = self.next;
            let bytes = self.chunk(id)?;
            if bytes.len() > max_payload {
                return None;
            }
            // El estado solo avanza cuando el símbolo sale de verdad.
            self.next += 1;
            return Some(Symbol { id, bytes });
        }

        None
    }

    fn on_feedback(&mut self, feedback: &Feedback) {
        let Feedback::Selective {
            cumulative,
            missing,
            window,
        } = feedback
        else {
            // Realimentación de otro modo: se ignora en vez de entrar en pánico.
            // Un par que hable otro dialecto es un fallo de negociación, no una
            // razón para tumbar la sesión.
            return;
        };

        self.base = self.base.max(*cumulative);
        if *window > 0 {
            self.window = u32::from(*window);
        }

        // Solo interesan los huecos que siguen por delante del acumulado; los
        // anteriores ya están dados por buenos.
        self.retransmit.retain(|id| *id >= self.base);
        for id in missing {
            if *id >= self.base && *id < self.total_chunks && !self.retransmit.contains(id) {
                self.retransmit.push(*id);
            }
        }
    }

    fn is_complete(&self) -> bool {
        self.base >= self.total_chunks
    }

    fn progress(&self) -> Progress {
        Progress {
            have: u64::from(self.base),
            need: u64::from(self.total_chunks),
        }
    }
}

/// El lado que reconstruye.
#[derive(Debug)]
pub struct ArqReceiver {
    buffer: Vec<u8>,
    received: Vec<bool>,
    chunk_size: usize,
    total_len: usize,
    total_chunks: u32,
    /// Primer chunk que falta. Se mantiene incremental para no recorrer el
    /// mapa entero en cada símbolo.
    cumulative: u32,
    count: u32,
    taken: bool,
}

impl ArqReceiver {
    /// # Panics
    /// Si `chunk_size` es cero.
    #[must_use]
    pub fn new(total_len: usize, chunk_size: usize) -> Self {
        assert!(chunk_size > 0, "el tamaño de chunk no puede ser cero");
        let total_chunks = total_len.div_ceil(chunk_size) as u32;
        Self {
            buffer: vec![0; total_len],
            received: vec![false; total_chunks as usize],
            chunk_size,
            total_len,
            total_chunks,
            cumulative: 0,
            count: 0,
            taken: false,
        }
    }

    /// Longitud que debe tener el chunk `id`. El último es más corto salvo que
    /// el objeto sea múltiplo exacto del tamaño de chunk.
    fn expected_len(&self, id: u32) -> usize {
        let start = id as usize * self.chunk_size;
        (start + self.chunk_size).min(self.total_len) - start
    }

    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.count == self.total_chunks
    }
}

impl Receiver for ArqReceiver {
    fn on_symbol(&mut self, symbol: &Symbol) -> Result<(), RecvError> {
        if symbol.id >= self.total_chunks {
            return Err(RecvError::OutOfRange {
                id: symbol.id,
                chunks: self.total_chunks,
            });
        }

        let expected = self.expected_len(symbol.id);
        if symbol.bytes.len() != expected {
            return Err(RecvError::SymbolSize {
                got: symbol.bytes.len(),
                expected,
            });
        }

        let idx = symbol.id as usize;
        if self.received[idx] {
            // Duplicado. No es un error: el medio los produce solo, y en un
            // canal donde cada QR se muestra varios refrescos es lo esperable.
            return Ok(());
        }

        let start = idx * self.chunk_size;
        self.buffer[start..start + expected].copy_from_slice(&symbol.bytes);
        self.received[idx] = true;
        self.count += 1;

        while (self.cumulative as usize) < self.received.len()
            && self.received[self.cumulative as usize]
        {
            self.cumulative += 1;
        }

        Ok(())
    }

    fn feedback(&self) -> Feedback {
        let mut missing = Vec::new();
        for (idx, got) in self
            .received
            .iter()
            .enumerate()
            .skip(self.cumulative as usize)
        {
            if !*got {
                missing.push(idx as u32);
                if missing.len() == MAX_MISSING_REPORTED {
                    break;
                }
            }
        }

        Feedback::Selective {
            cumulative: self.cumulative,
            missing,
            window: DEFAULT_WINDOW as u16,
        }
    }

    fn take_object(&mut self) -> Option<Vec<u8>> {
        if !self.is_complete() || self.taken {
            return None;
        }
        self.taken = true;
        Some(core::mem::take(&mut self.buffer))
    }

    fn progress(&self) -> Progress {
        Progress {
            have: u64::from(self.count),
            need: u64::from(self.total_chunks),
        }
    }
}
