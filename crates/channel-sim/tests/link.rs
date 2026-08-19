//! Tests del propio simulador.
//!
//! Si el simulador miente, todo test construido encima no vale nada: el núcleo
//! del protocolo pasaría con 40 % de pérdida porque el simulador en realidad no
//! estaría perdiendo nada. Así que aquí se comprueba que hace exactamente lo
//! que promete antes de confiar en él.

use std::time::Duration;

use channel_sim::{LinkConfig, SimPair};
use optical_protocol::channel::{Channel, ChannelError};
use optical_protocol::wire::{Flags, Pdu, PduKind};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn pdu(seq: u32) -> Pdu {
    Pdu {
        session_id: 7,
        kind: PduKind::Data,
        flags: Flags::NONE,
        seq,
        ack: 0,
        payload: vec![0xab; 64],
    }
}

#[test]
fn enlace_perfecto_entrega_todo_en_orden_e_intacto() {
    let mut link = SimPair::new(LinkConfig::perfect(1024), 1);

    for seq in 0..50u32 {
        link.a.send_frame(&pdu(seq).to_vec().unwrap()).unwrap();
    }
    link.advance(ms(1));

    for seq in 0..50u32 {
        let frame = link.b.recv_frame().expect("marco {seq} debería llegar");
        let recibido = Pdu::decode(&frame).expect("debería decodificar");
        assert_eq!(recibido, pdu(seq), "el marco {seq} llegó alterado");
    }
    assert!(link.b.recv_frame().is_none(), "no debería sobrar nada");
}

#[test]
fn la_misma_semilla_produce_la_misma_secuencia() {
    let corrida = |seed: u64| {
        let mut link = SimPair::new(LinkConfig::optical(1024, 0.4), seed);
        let mut llegados = Vec::new();
        for seq in 0..200u32 {
            link.advance(ms(seq as u64));
            link.a.send_frame(&pdu(seq).to_vec().unwrap()).unwrap();
        }
        link.advance(ms(10_000));
        while let Some(f) = link.b.recv_frame() {
            llegados.push(Pdu::decode(&f).unwrap().seq);
        }
        llegados
    };

    assert_eq!(corrida(42), corrida(42), "la misma semilla debe repetirse");
    assert_ne!(
        corrida(42),
        corrida(43),
        "semillas distintas no deberían coincidir"
    );
}

#[test]
fn la_tasa_de_perdida_es_la_declarada() {
    const N: u32 = 10_000;
    let mut link = SimPair::new(LinkConfig::optical(1024, 0.4), 7);

    for seq in 0..N {
        link.a.send_frame(&pdu(seq).to_vec().unwrap()).unwrap();
    }

    let stats = link.a.tx_stats();
    assert_eq!(stats.offered, u64::from(N));

    let observada = stats.dropped as f64 / f64::from(N);
    assert!(
        (0.375..0.425).contains(&observada),
        "pérdida observada {observada:.4}, se esperaba ~0.40 (±5σ ≈ ±0.025)"
    );
}

#[test]
fn el_retardo_se_respeta_al_milisegundo() {
    let cfg = LinkConfig {
        mtu: 1024,
        latency: ms(100),
        ..LinkConfig::default()
    };
    let mut link = SimPair::new(cfg, 1);

    link.a.send_frame(&pdu(0).to_vec().unwrap()).unwrap();

    link.advance(ms(99));
    assert!(
        link.b.recv_frame().is_none(),
        "a los 99 ms el marco no debería haber llegado"
    );

    link.advance(ms(100));
    assert!(
        link.b.recv_frame().is_some(),
        "a los 100 ms el marco debería estar disponible"
    );
}

#[test]
fn el_jitter_produce_reordenamiento() {
    let cfg = LinkConfig {
        mtu: 1024,
        latency: ms(100),
        jitter: ms(60),
        ..LinkConfig::default()
    };
    let mut link = SimPair::new(cfg, 5);

    // Marcos separados 1 ms, con hasta 60 ms de jitter: adelantarse es lo
    // normal, no la excepción.
    for seq in 0..200u32 {
        link.advance(ms(seq as u64));
        link.a.send_frame(&pdu(seq).to_vec().unwrap()).unwrap();
    }
    link.advance(ms(10_000));
    while link.b.recv_frame().is_some() {}

    assert!(
        link.b.rx_reorders() > 10,
        "con 60 ms de jitter y marcos cada 1 ms debería haber reordenamiento, hubo {}",
        link.b.rx_reorders()
    );
}

#[test]
fn sin_jitter_no_hay_reordenamiento() {
    let mut link = SimPair::new(LinkConfig::perfect(1024), 5);
    for seq in 0..200u32 {
        link.advance(ms(seq as u64));
        link.a.send_frame(&pdu(seq).to_vec().unwrap()).unwrap();
    }
    link.advance(ms(10_000));
    while link.b.recv_frame().is_some() {}

    assert_eq!(
        link.b.rx_reorders(),
        0,
        "sin jitter el orden debe conservarse"
    );
}

#[test]
fn la_duplicacion_entrega_el_marco_dos_veces() {
    let cfg = LinkConfig::perfect(1024).with_duplication(1.0);
    let mut link = SimPair::new(cfg, 3);

    link.a.send_frame(&pdu(0).to_vec().unwrap()).unwrap();
    link.advance(ms(1));

    let primero = link.b.recv_frame().expect("primera copia");
    let segundo = link.b.recv_frame().expect("segunda copia");
    assert_eq!(primero, segundo, "las dos copias deberían ser idénticas");
    assert!(link.b.recv_frame().is_none(), "solo debería haber dos");
}

/// El test que une simulador y formato de wire: la corrupción del medio tiene
/// que ser detectada por el CRC, no colarse hasta la capa de aplicación.
#[test]
fn la_corrupcion_del_medio_la_caza_el_crc() {
    let cfg = LinkConfig::perfect(1024).with_corruption(1.0);
    let mut link = SimPair::new(cfg, 11);

    const N: u32 = 500;
    for seq in 0..N {
        link.a.send_frame(&pdu(seq).to_vec().unwrap()).unwrap();
    }
    link.advance(ms(1));

    let mut recibidos = 0;
    let mut rechazados = 0;
    while let Some(frame) = link.b.recv_frame() {
        recibidos += 1;
        if Pdu::decode(&frame).is_err() {
            rechazados += 1;
            link.b.note_rejected();
        }
    }

    assert_eq!(recibidos, N as usize, "todos los marcos deberían llegar");
    assert_eq!(
        rechazados, N as usize,
        "con corrupción al 100 % ningún marco debería pasar la validación"
    );
    assert_eq!(link.b.health().frames_rejected, u64::from(N));
    assert_eq!(
        link.b.health().rejection_rate(),
        1.0,
        "con corrupción al 100 % la tasa de rechazo es 1, no 0,5: los rechazados ya están contados dentro de los recibidos"
    );
}

#[test]
fn un_marco_mayor_que_la_mtu_se_rechaza_al_enviar() {
    let mut link = SimPair::new(LinkConfig::perfect(64), 1);
    let grande = vec![0u8; 65];

    assert_eq!(
        link.a.send_frame(&grande),
        Err(ChannelError::OverMtu { got: 65, mtu: 64 })
    );
    assert_eq!(
        link.a.health().frames_sent,
        0,
        "un marco rechazado no debería contar como enviado"
    );
}

/// El diseño contempla enlaces asimétricos —el audio puede funcionar de A a B
/// y no al revés—, así que el simulador tiene que poder expresarlo.
#[test]
fn los_dos_sentidos_son_independientes() {
    let vivo = LinkConfig::perfect(1024);
    let muerto = LinkConfig {
        loss: 1.0,
        ..LinkConfig::perfect(1024)
    };
    let mut link = SimPair::asymmetric(vivo, muerto, 1);

    link.a.send_frame(&pdu(1).to_vec().unwrap()).unwrap();
    link.b.send_frame(&pdu(2).to_vec().unwrap()).unwrap();
    link.advance(ms(1));

    assert!(
        link.b.recv_frame().is_some(),
        "el sentido A→B debería funcionar"
    );
    assert!(
        link.a.recv_frame().is_none(),
        "el sentido B→A debería estar muerto"
    );
}

#[test]
fn rx_idle_distingue_vacio_de_en_vuelo() {
    let cfg = LinkConfig {
        mtu: 1024,
        latency: ms(100),
        ..LinkConfig::default()
    };
    let mut link = SimPair::new(cfg, 1);

    assert!(link.b.rx_idle(), "recién creado no hay nada en vuelo");

    link.a.send_frame(&pdu(0).to_vec().unwrap()).unwrap();
    assert!(!link.b.rx_idle(), "hay un marco en vuelo");

    link.advance(ms(100));
    assert!(!link.b.rx_idle(), "llegó pero nadie lo ha recogido");

    link.b.recv_frame().unwrap();
    assert!(link.b.rx_idle(), "ya está todo recogido");
}
