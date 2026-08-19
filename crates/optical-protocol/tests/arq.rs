//! Tests de la ventana deslizante, sin canal de por medio.
//!
//! Aquí se prueba la lógica pura: qué emite el emisor, qué acepta el receptor y
//! qué se cuentan entre ellos. La transferencia de extremo a extremo sobre un
//! medio con pérdida vive en `channel-sim`, que es quien tiene el simulador.

use optical_protocol::reliability::arq::{
    ArqReceiver, ArqSender, DEFAULT_WINDOW, MAX_MISSING_REPORTED,
};
use optical_protocol::reliability::{Feedback, Receiver, RecvError, Sender, Symbol};

const CS: usize = 100;

fn objeto(len: usize) -> Vec<u8> {
    // Patrón no repetitivo: si el receptor coloca un chunk en el sitio
    // equivocado, un relleno constante lo ocultaría.
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// Conecta emisor y receptor sin pérdidas, realimentando en cada vuelta.
fn transferir(sender: &mut ArqSender, receiver: &mut ArqReceiver, max_payload: usize) -> usize {
    let mut vueltas = 0;
    while !sender.is_complete() {
        vueltas += 1;
        assert!(vueltas < 100_000, "no converge");

        if let Some(sym) = sender.next_symbol(max_payload) {
            receiver.on_symbol(&sym).expect("símbolo válido");
        }
        sender.on_feedback(&receiver.feedback());
    }
    vueltas
}

#[test]
fn transferencia_limpia_reconstruye_el_objeto_exacto() {
    let original = objeto(1000);
    let mut tx = ArqSender::new(original.clone(), CS);
    let mut rx = ArqReceiver::new(original.len(), CS);

    transferir(&mut tx, &mut rx, CS);

    assert_eq!(rx.take_object().expect("completo"), original);
}

#[test]
fn el_ultimo_chunk_corto_se_maneja() {
    // 1050 = 10 chunks de 100 + uno de 50.
    let original = objeto(1050);
    let mut tx = ArqSender::new(original.clone(), CS);
    let mut rx = ArqReceiver::new(original.len(), CS);

    assert_eq!(tx.total_chunks(), 11);
    transferir(&mut tx, &mut rx, CS);

    assert_eq!(rx.take_object().expect("completo"), original);
}

#[test]
fn objeto_multiplo_exacto_no_genera_chunk_de_sobra() {
    let original = objeto(1000);
    let tx = ArqSender::new(original, CS);
    assert_eq!(tx.total_chunks(), 10, "1000/100 son 10 chunks, no 11");
}

#[test]
fn objeto_vacio_esta_completo_de_entrada() {
    let tx = ArqSender::new(Vec::new(), CS);
    let mut rx = ArqReceiver::new(0, CS);

    assert!(tx.is_complete(), "no hay nada que enviar");
    assert!(rx.is_complete(), "no hay nada que esperar");
    assert_eq!(rx.take_object(), Some(Vec::new()));
    assert_eq!(rx.take_object(), None, "solo se entrega una vez");
}

#[test]
fn la_ventana_limita_los_simbolos_en_vuelo() {
    let original = objeto(100 * 100);
    let mut tx = ArqSender::new(original, CS);

    // Sin realimentación ninguna, el emisor no debería pasar de la ventana.
    let mut emitidos = 0;
    while tx.next_symbol(CS).is_some() {
        emitidos += 1;
        assert!(
            emitidos <= DEFAULT_WINDOW as usize,
            "emitió {emitidos} sin confirmación, con ventana {DEFAULT_WINDOW}"
        );
    }
    assert_eq!(emitidos, DEFAULT_WINDOW as usize);
}

#[test]
fn los_huecos_se_retransmiten_antes_que_lo_nuevo() {
    let original = objeto(1000);
    let mut tx = ArqSender::new(original, CS);

    // Se emiten los primeros cinco y se confirma que faltan el 1 y el 3.
    for _ in 0..5 {
        tx.next_symbol(CS).unwrap();
    }
    tx.on_feedback(&Feedback::Selective {
        cumulative: 1,
        missing: vec![1, 3],
        window: DEFAULT_WINDOW as u16,
    });

    assert_eq!(
        tx.next_symbol(CS).unwrap().id,
        1,
        "primero el hueco más viejo"
    );
    assert_eq!(
        tx.next_symbol(CS).unwrap().id,
        3,
        "luego el siguiente hueco"
    );
    assert_eq!(
        tx.next_symbol(CS).unwrap().id,
        5,
        "y solo entonces datos nuevos"
    );
}

#[test]
fn un_hueco_ya_confirmado_no_se_retransmite() {
    let original = objeto(1000);
    let mut tx = ArqSender::new(original, CS);
    for _ in 0..5 {
        tx.next_symbol(CS).unwrap();
    }

    tx.on_feedback(&Feedback::Selective {
        cumulative: 0,
        missing: vec![2],
        window: DEFAULT_WINDOW as u16,
    });
    // El acumulado avanza por encima del hueco: ya no hace falta.
    tx.on_feedback(&Feedback::Selective {
        cumulative: 5,
        missing: vec![],
        window: DEFAULT_WINDOW as u16,
    });

    assert_eq!(
        tx.next_symbol(CS).unwrap().id,
        5,
        "el hueco 2 quedó cubierto por el acumulado"
    );
}

#[test]
fn el_acumulado_nunca_retrocede() {
    let mut tx = ArqSender::new(objeto(1000), CS);
    tx.on_feedback(&Feedback::Selective {
        cumulative: 7,
        missing: vec![],
        window: 0,
    });
    // Una confirmación vieja que llega tarde no debe deshacer el avance: en un
    // canal con reordenamiento esto pasa de verdad.
    tx.on_feedback(&Feedback::Selective {
        cumulative: 3,
        missing: vec![],
        window: 0,
    });
    assert_eq!(tx.progress().have, 7);
}

#[test]
fn realimentacion_de_otro_modo_se_ignora_sin_romper() {
    let mut tx = ArqSender::new(objeto(1000), CS);
    tx.on_feedback(&Feedback::Fountain {
        complete: true,
        received: 999,
    });
    assert!(
        !tx.is_complete(),
        "una realimentación de fuente no debería completar una transferencia ARQ"
    );
}

#[test]
fn un_duplicado_no_es_un_error() {
    let mut rx = ArqReceiver::new(1000, CS);
    let sym = Symbol {
        id: 0,
        bytes: vec![7; CS],
    };

    rx.on_symbol(&sym).expect("primera vez");
    rx.on_symbol(&sym)
        .expect("el medio duplica solo; no es error");
    assert_eq!(rx.progress().have, 1, "no debería contar dos veces");
}

#[test]
fn un_simbolo_de_tamano_erroneo_se_rechaza() {
    let mut rx = ArqReceiver::new(1000, CS);
    let err = rx
        .on_symbol(&Symbol {
            id: 0,
            bytes: vec![0; CS - 1],
        })
        .unwrap_err();
    assert_eq!(
        err,
        RecvError::SymbolSize {
            got: CS - 1,
            expected: CS
        }
    );
}

#[test]
fn un_identificador_fuera_de_rango_se_rechaza() {
    let mut rx = ArqReceiver::new(1000, CS);
    let err = rx
        .on_symbol(&Symbol {
            id: 10,
            bytes: vec![0; CS],
        })
        .unwrap_err();
    assert_eq!(err, RecvError::OutOfRange { id: 10, chunks: 10 });
}

#[test]
fn la_lista_de_huecos_esta_acotada() {
    // Muchos más huecos que el límite: la realimentación tiene que caber en un
    // marco, así que se recortan.
    let total = (MAX_MISSING_REPORTED + 50) * CS;
    let mut rx = ArqReceiver::new(total, CS);

    // Se recibe solo el último chunk: todo lo anterior queda como hueco.
    let ultimo = (total / CS - 1) as u32;
    rx.on_symbol(&Symbol {
        id: ultimo,
        bytes: vec![0; CS],
    })
    .unwrap();

    let Feedback::Selective { missing, .. } = rx.feedback() else {
        panic!("ARQ debe producir realimentación selectiva");
    };
    assert_eq!(missing.len(), MAX_MISSING_REPORTED);
    assert_eq!(
        missing[0], 0,
        "se reportan los más viejos, que son los que bloquean"
    );
}

#[test]
fn un_simbolo_que_no_cabe_no_se_emite() {
    let mut tx = ArqSender::new(objeto(1000), CS);
    assert!(
        tx.next_symbol(CS - 1).is_none(),
        "con menos espacio que el chunk no debe emitir nada"
    );
    assert_eq!(tx.progress().have, 0, "y el estado no debe haber avanzado");
    assert!(
        tx.next_symbol(CS).is_some(),
        "con espacio suficiente sí emite"
    );
}

#[test]
fn el_objeto_solo_se_entrega_una_vez() {
    let original = objeto(300);
    let mut tx = ArqSender::new(original.clone(), CS);
    let mut rx = ArqReceiver::new(original.len(), CS);
    transferir(&mut tx, &mut rx, CS);

    assert_eq!(rx.take_object(), Some(original));
    assert_eq!(rx.take_object(), None);
}

#[test]
fn sin_todos_los_chunks_no_se_entrega_nada() {
    let mut rx = ArqReceiver::new(300, CS);
    rx.on_symbol(&Symbol {
        id: 0,
        bytes: vec![1; CS],
    })
    .unwrap();
    rx.on_symbol(&Symbol {
        id: 2,
        bytes: vec![3; CS],
    })
    .unwrap();

    assert!(!rx.is_complete());
    assert_eq!(rx.take_object(), None, "falta el chunk 1");
}

#[test]
fn el_orden_de_llegada_no_altera_el_resultado() {
    let original = objeto(1000);

    let reconstruir = |ids: Vec<u32>| {
        let mut rx = ArqReceiver::new(original.len(), CS);
        for id in ids {
            let start = id as usize * CS;
            rx.on_symbol(&Symbol {
                id,
                bytes: original[start..start + CS].to_vec(),
            })
            .unwrap();
        }
        rx.take_object()
    };

    let directo: Vec<u32> = (0..10).collect();
    let inverso: Vec<u32> = (0..10).rev().collect();
    let barajado = vec![3, 7, 0, 9, 1, 5, 8, 2, 6, 4];

    assert_eq!(reconstruir(directo), Some(original.clone()));
    assert_eq!(reconstruir(inverso), Some(original.clone()));
    assert_eq!(reconstruir(barajado), Some(original));
}
