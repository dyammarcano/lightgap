//! Tests del formato de wire.
//!
//! El canal óptico entrega marcos corruptos con regularidad —desenfoque,
//! reflejos, movimiento— así que lo que se prueba aquí no es solo que el
//! roundtrip funcione, sino que la corrupción **se detecte siempre**. Un byte
//! malo que pase por bueno se convierte en un archivo corrupto al otro lado.

use optical_protocol::wire::{
    Flags, Pdu, PduKind, WireError, HEADER_LEN, MAX_PAYLOAD, OVERHEAD, PROTOCOL_VERSION,
};
use proptest::prelude::*;

fn sample() -> Pdu {
    Pdu {
        session_id: 0x0123_4567_89ab_cdef,
        kind: PduKind::Data,
        flags: Flags::ACK_VALID | Flags::FOUNTAIN,
        seq: 42,
        ack: 41,
        payload: b"carga de prueba".to_vec(),
    }
}

#[test]
fn roundtrip_conserva_todos_los_campos() {
    let pdu = sample();
    let bytes = pdu.to_vec().expect("codifica");
    assert_eq!(Pdu::decode(&bytes).expect("decodifica"), pdu);
}

#[test]
fn encoded_len_coincide_con_lo_producido() {
    let pdu = sample();
    assert_eq!(pdu.to_vec().unwrap().len(), pdu.encoded_len());
    assert_eq!(pdu.encoded_len(), OVERHEAD + pdu.payload.len());
}

#[test]
fn payload_vacio_es_valido() {
    let pdu = Pdu {
        payload: Vec::new(),
        ..sample()
    };
    let bytes = pdu.to_vec().unwrap();
    assert_eq!(bytes.len(), OVERHEAD);
    assert_eq!(Pdu::decode(&bytes).unwrap(), pdu);
}

#[test]
fn buffer_corto_se_rechaza_sin_leer_campos() {
    for n in 0..OVERHEAD {
        let buf = vec![0u8; n];
        assert_eq!(
            Pdu::decode(&buf),
            Err(WireError::TooShort {
                got: n,
                need: OVERHEAD
            }),
            "un buffer de {n} B debería rechazarse por corto"
        );
    }
}

#[test]
fn version_distinta_se_rechaza() {
    let mut bytes = sample().to_vec().unwrap();
    bytes[0] = PROTOCOL_VERSION.wrapping_add(1);
    assert_eq!(
        Pdu::decode(&bytes),
        Err(WireError::Version {
            got: PROTOCOL_VERSION.wrapping_add(1),
            expected: PROTOCOL_VERSION,
        })
    );
}

#[test]
fn tipo_desconocido_se_rechaza() {
    let mut bytes = sample().to_vec().unwrap();
    bytes[9] = 0xff;
    assert_eq!(Pdu::decode(&bytes), Err(WireError::UnknownKind(0xff)));
}

#[test]
fn longitud_declarada_mayor_que_el_buffer_se_rechaza() {
    let pdu = sample();
    let mut bytes = pdu.to_vec().unwrap();
    let inflada = (pdu.payload.len() as u16) + 10;
    bytes[20..22].copy_from_slice(&inflada.to_le_bytes());
    assert_eq!(
        Pdu::decode(&bytes),
        Err(WireError::PayloadLen {
            declared: inflada as usize,
            available: bytes.len() - OVERHEAD,
        })
    );
}

#[test]
fn bytes_sobrantes_se_rechazan() {
    let mut bytes = sample().to_vec().unwrap();
    bytes.extend_from_slice(&[0, 0, 0]);
    assert_eq!(Pdu::decode(&bytes), Err(WireError::TrailingBytes(3)));
}

#[test]
fn payload_mayor_que_el_campo_de_longitud_no_se_codifica() {
    let pdu = Pdu {
        payload: vec![0u8; MAX_PAYLOAD + 1],
        ..sample()
    };
    assert_eq!(
        pdu.to_vec(),
        Err(WireError::PayloadTooLarge {
            got: MAX_PAYLOAD + 1,
            max: MAX_PAYLOAD,
        })
    );
}

/// La propiedad que de verdad importa en este dominio.
///
/// CRC32 detecta todo error de un solo bit, pero la PDU tiene campos
/// estructurales (versión, tipo, longitud) que se leen ANTES de comprobar el
/// CRC. Este test recorre exhaustivamente cada bit de un marco codificado y
/// exige que voltearlo produzca un error — sea del CRC o estructural. Es la
/// garantía de que ningún marco corrupto se acepta como bueno.
#[test]
fn ningun_bit_volteado_pasa_por_bueno() {
    let pdu = sample();
    let limpio = pdu.to_vec().unwrap();

    for byte_idx in 0..limpio.len() {
        for bit in 0..8 {
            let mut corrupto = limpio.clone();
            corrupto[byte_idx] ^= 1 << bit;

            match Pdu::decode(&corrupto) {
                Err(_) => {}
                Ok(recuperado) => panic!(
                    "el bit {bit} del byte {byte_idx} se volteó y decode() lo aceptó: {recuperado}"
                ),
            }
        }
    }
}

/// Voltear dos bits tampoco debería colar. CRC32 no lo garantiza en general
/// para cualquier distancia, pero sí para errores dentro de su alcance; este
/// test acota empíricamente el riesgo sobre un marco representativo.
#[test]
fn ningun_par_de_bits_volteados_pasa_por_bueno() {
    let pdu = Pdu {
        payload: b"abcd".to_vec(),
        ..sample()
    };
    let limpio = pdu.to_vec().unwrap();
    let total_bits = limpio.len() * 8;

    for a in 0..total_bits {
        for b in (a + 1)..total_bits {
            let mut corrupto = limpio.clone();
            corrupto[a / 8] ^= 1 << (a % 8);
            corrupto[b / 8] ^= 1 << (b % 8);

            assert!(
                Pdu::decode(&corrupto).is_err(),
                "los bits {a} y {b} volteados juntos pasaron por buenos"
            );
        }
    }
}

fn kind_arb() -> impl Strategy<Value = PduKind> {
    prop_oneof![
        Just(PduKind::Hello),
        Just(PduKind::Capabilities),
        Just(PduKind::Data),
        Just(PduKind::Ack),
        Just(PduKind::Probe),
        Just(PduKind::ProbeResult),
        Just(PduKind::Complete),
        Just(PduKind::Cancel),
    ]
}

prop_compose! {
    fn pdu_arb()(
        session_id in any::<u64>(),
        kind in kind_arb(),
        flags in any::<u16>(),
        seq in any::<u32>(),
        ack in any::<u32>(),
        payload in prop::collection::vec(any::<u8>(), 0..3000),
    ) -> Pdu {
        Pdu { session_id, kind, flags: Flags(flags), seq, ack, payload }
    }
}

proptest! {
    /// Cualquier PDU representable sobrevive al viaje de ida y vuelta.
    #[test]
    fn roundtrip_arbitrario(pdu in pdu_arb()) {
        let bytes = pdu.to_vec().unwrap();
        prop_assert_eq!(bytes.len(), pdu.encoded_len());
        prop_assert_eq!(Pdu::decode(&bytes).unwrap(), pdu);
    }

    /// Truncar por cualquier sitio se detecta. Un marco óptico parcialmente
    /// leído es un caso real, no hipotético.
    #[test]
    fn truncar_siempre_se_detecta(pdu in pdu_arb(), corte in 0usize..4096) {
        let bytes = pdu.to_vec().unwrap();
        let corte = corte % bytes.len().max(1);
        prop_assert!(Pdu::decode(&bytes[..corte]).is_err());
    }

    /// El payload empieza exactamente donde dice la cabecera. Protege contra
    /// que un cambio de layout desplace el payload sin que nadie se entere.
    #[test]
    fn el_payload_esta_donde_dice_la_cabecera(pdu in pdu_arb()) {
        let bytes = pdu.to_vec().unwrap();
        let fin = HEADER_LEN + pdu.payload.len();
        prop_assert_eq!(&bytes[HEADER_LEN..fin], &pdu.payload[..]);
    }
}
