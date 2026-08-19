//! Transferencia completa de extremo a extremo sobre el medio simulado.
//!
//! Este es el criterio de salida de la Fase 1: mover un objeto real, entero y
//! byte a byte idéntico, a través de un canal que pierde, retrasa, reordena y
//! duplica — sin encender una cámara.
//!
//! El driver modela cómo funciona de verdad un enlace óptico: **la
//! realimentación se emite periódicamente, no como respuesta a cada dato**. La
//! pantalla del receptor siempre está mostrando algún QR, así que su estado se
//! está radiando de forma continua. Con ACK reactivo, perder uno dejaría al
//! emisor bloqueado esperando algo que ya nadie va a repetir.

use std::time::Duration;

use channel_sim::{LinkConfig, SimPair};
use optical_protocol::channel::Channel;
use optical_protocol::reliability::arq::{ArqReceiver, ArqSender};
use optical_protocol::reliability::fountain::{symbol_size_for, FountainReceiver, FountainSender};
use optical_protocol::reliability::{Feedback, Receiver, Sender, Symbol};
use optical_protocol::wire::{Flags, Pdu, PduKind, OVERHEAD};

/// MTU del canal, en bytes por marco. Un QR de densidad media y buena lectura.
const MTU: usize = 900;
/// Payload útil una vez descontada la cabecera y el CRC de la PDU.
const PAYLOAD: usize = MTU - OVERHEAD;
/// Cada cuántos ticks el receptor refresca su realimentación.
const FEEDBACK_EVERY: u64 = 4;
/// Duración virtual de un tick: aproximadamente un marco óptico.
const TICK_MS: u64 = 80;

const SESSION: u64 = 0xfeed_face_dead_beef;

struct Resultado {
    objeto: Vec<u8>,
    ticks: u64,
    simbolos_enviados: u64,
    realimentaciones: u64,
}

fn datos(len: usize) -> Vec<u8> {
    // Pseudoaleatorio determinista y sin periodo corto: un patrón repetitivo
    // podría ocultar que un trozo acabó en el sitio equivocado.
    let mut out = Vec::with_capacity(len);
    let mut x: u64 = 0x2545_f491_4f6c_dd1d;
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.truncate(len);
    out
}

fn pdu_datos(sym: &Symbol, flags: Flags) -> Vec<u8> {
    Pdu {
        session_id: SESSION,
        kind: PduKind::Data,
        flags,
        seq: sym.id,
        ack: 0,
        payload: sym.bytes.clone(),
    }
    .to_vec()
    .expect("símbolo dentro del límite del formato")
}

fn pdu_ack(fb: &Feedback) -> Vec<u8> {
    Pdu {
        session_id: SESSION,
        kind: PduKind::Ack,
        flags: Flags::ACK_VALID,
        seq: 0,
        ack: 0,
        payload: fb.encode(),
    }
    .to_vec()
    .expect("realimentación acotada")
}

/// Empuja una transferencia hasta que el receptor reconstruye o se agota el
/// presupuesto de ticks.
fn correr(
    tx: &mut dyn Sender,
    rx: &mut dyn Receiver,
    link: &mut SimPair,
    max_ticks: u64,
    flags_datos: Flags,
) -> Resultado {
    let mut ahora = Duration::ZERO;
    let mut ticks = 0;
    let mut enviados = 0;
    let mut realimentaciones = 0;

    loop {
        ticks += 1;
        assert!(
            ticks <= max_ticks,
            "no convergió en {max_ticks} ticks (progreso rx {:?})",
            rx.progress()
        );
        ahora += Duration::from_millis(TICK_MS);
        link.advance(ahora);

        // --- A emite un símbolo por tick, como un QR por marco ---------------
        if !tx.is_complete() {
            if let Some(sym) = tx.next_symbol(PAYLOAD) {
                link.a
                    .send_frame(&pdu_datos(&sym, flags_datos))
                    .expect("cabe en la MTU");
                enviados += 1;
            }
        }

        // --- B recoge lo que haya llegado ------------------------------------
        while let Some(marco) = link.b.recv_frame() {
            match Pdu::decode(&marco) {
                Ok(pdu) if pdu.kind == PduKind::Data => {
                    let sym = Symbol {
                        id: pdu.seq,
                        bytes: pdu.payload,
                    };
                    // Un símbolo mal formado se descarta: en este medio pasa.
                    let _ = rx.on_symbol(&sym);
                }
                Ok(_) => {}
                Err(_) => link.b.note_rejected(),
            }
        }

        // --- B radia su estado cada pocos ticks ------------------------------
        if ticks % FEEDBACK_EVERY == 0 {
            link.b
                .send_frame(&pdu_ack(&rx.feedback()))
                .expect("cabe en la MTU");
            realimentaciones += 1;
        }

        // --- A recoge la realimentación --------------------------------------
        while let Some(marco) = link.a.recv_frame() {
            match Pdu::decode(&marco) {
                Ok(pdu) if pdu.kind == PduKind::Ack => {
                    if let Some(fb) = Feedback::decode(&pdu.payload) {
                        tx.on_feedback(&fb);
                    }
                }
                Ok(_) => {}
                Err(_) => link.a.note_rejected(),
            }
        }

        if let Some(objeto) = rx.take_object() {
            return Resultado {
                objeto,
                ticks,
                simbolos_enviados: enviados,
                realimentaciones,
            };
        }
    }
}

/// Criterio de la Fase 1 para fuente: 40 % de pérdida en ambos sentidos.
#[test]
fn fuente_transfiere_con_40_por_ciento_de_perdida() {
    let original = datos(5 * 1024 * 1024);
    let ss = symbol_size_for(PAYLOAD).expect("cabe un símbolo");

    let mut tx = FountainSender::new(&original, ss);
    let mut rx = FountainReceiver::new(original.len() as u64, ss);
    let mut link = SimPair::new(LinkConfig::optical(MTU, 0.40), 20_260_819);

    let r = correr(&mut tx, &mut rx, &mut link, 200_000, Flags::FOUNTAIN);

    assert_eq!(
        r.objeto, original,
        "el objeto debe llegar byte a byte igual"
    );
    println!(
        "fuente 40%: {} ticks, {} símbolos, {} realimentaciones",
        r.ticks, r.simbolos_enviados, r.realimentaciones
    );
}

/// Criterio de la Fase 1 para ARQ: 15 % de pérdida en ambos sentidos.
#[test]
fn arq_transfiere_con_15_por_ciento_de_perdida() {
    let original = datos(5 * 1024 * 1024);

    let mut tx = ArqSender::new(original.clone(), PAYLOAD);
    let mut rx = ArqReceiver::new(original.len(), PAYLOAD);
    let mut link = SimPair::new(LinkConfig::optical(MTU, 0.15), 20_260_819);

    let r = correr(&mut tx, &mut rx, &mut link, 200_000, Flags::NONE);

    assert_eq!(
        r.objeto, original,
        "el objeto debe llegar byte a byte igual"
    );
    println!(
        "arq 15%: {} ticks, {} símbolos, {} realimentaciones",
        r.ticks, r.simbolos_enviados, r.realimentaciones
    );
}

/// Con el canal limpio, la fuente no debería necesitar mucho más que los
/// símbolos de fuente. Si necesitara muchísimos más, algo va mal en la
/// generación de reparación.
#[test]
fn fuente_sin_perdida_no_desperdicia_mucho() {
    let original = datos(128 * 1024);
    let ss = symbol_size_for(PAYLOAD).unwrap();

    let mut tx = FountainSender::new(&original, ss);
    let k = tx.source_symbols() as u64;
    let mut rx = FountainReceiver::new(original.len() as u64, ss);
    let mut link = SimPair::new(LinkConfig::perfect(MTU), 7);

    let r = correr(&mut tx, &mut rx, &mut link, 100_000, Flags::FOUNTAIN);

    assert_eq!(r.objeto, original);
    assert!(
        r.simbolos_enviados < k * 2,
        "envió {} símbolos para K={k}: demasiado desperdicio en un canal limpio",
        r.simbolos_enviados
    );
}

/// La corrupción se detecta por CRC y el marco se descarta; la transferencia
/// tiene que sobrevivir igualmente. Es el caso real de un QR borroso que aun
/// así decodifica a bytes equivocados.
#[test]
fn fuente_sobrevive_a_corrupcion_ademas_de_perdida() {
    let original = datos(128 * 1024);
    let ss = symbol_size_for(PAYLOAD).unwrap();

    let cfg = LinkConfig::optical(MTU, 0.10).with_corruption(0.10);
    let mut tx = FountainSender::new(&original, ss);
    let mut rx = FountainReceiver::new(original.len() as u64, ss);
    let mut link = SimPair::new(cfg, 99);

    let r = correr(&mut tx, &mut rx, &mut link, 200_000, Flags::FOUNTAIN);

    assert_eq!(r.objeto, original);
    assert!(
        link.b.health().frames_rejected > 0,
        "el test debería haber ejercitado el rechazo por CRC"
    );
}

/// Enlace asimétrico: el camino de vuelta es mucho peor que el de ida. Es el
/// escenario que el diseño contempla cuando una webcam es peor que la otra.
#[test]
fn fuente_tolera_un_camino_de_vuelta_mucho_peor() {
    let original = datos(128 * 1024);
    let ss = symbol_size_for(PAYLOAD).unwrap();

    let ida = LinkConfig::optical(MTU, 0.05);
    let vuelta = LinkConfig::optical(MTU, 0.80);
    let mut tx = FountainSender::new(&original, ss);
    let mut rx = FountainReceiver::new(original.len() as u64, ss);
    let mut link = SimPair::asymmetric(ida, vuelta, 5);

    let r = correr(&mut tx, &mut rx, &mut link, 200_000, Flags::FOUNTAIN);
    assert_eq!(r.objeto, original);
}
