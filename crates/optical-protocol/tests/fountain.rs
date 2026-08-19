//! Tests del código de fuente, sin canal de por medio.

use optical_protocol::reliability::fountain::{
    symbol_size_for, FountainReceiver, FountainSender, PACKET_ID_LEN,
};
use optical_protocol::reliability::{Feedback, Receiver, RecvError, Sender, Symbol};

const SS: u16 = 200;
/// Payload de canal que deja sitio al símbolo y a su identificador.
const MAX: usize = SS as usize + PACKET_ID_LEN;

fn objeto(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// Emite `n` símbolos sin realimentar. Devuelve lo emitido.
fn emitir(tx: &mut FountainSender, n: usize) -> Vec<Symbol> {
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        match tx.next_symbol(MAX) {
            Some(s) => out.push(s),
            None => break,
        }
    }
    out
}

#[test]
fn reconstruye_con_todos_los_simbolos_de_fuente() {
    let original = objeto(10_000);
    let mut tx = FountainSender::new(&original, SS);
    let mut rx = FountainReceiver::new(original.len() as u64, SS);

    let k = tx.source_symbols() as usize;
    for sym in emitir(&mut tx, k * 2) {
        rx.on_symbol(&sym).unwrap();
        if rx.is_complete() {
            break;
        }
    }

    assert!(rx.is_complete());
    assert_eq!(rx.take_object().unwrap(), original);
}

/// La propiedad que justifica elegir fuente: da igual *cuáles* símbolos se
/// pierdan mientras lleguen suficientes. Sin esto, todo el argumento de diseño
/// a favor de la fuente se cae.
#[test]
fn reconstruye_perdiendo_el_40_por_ciento_de_los_simbolos() {
    let original = objeto(10_000);
    let mut tx = FountainSender::new(&original, SS);
    let mut rx = FountainReceiver::new(original.len() as u64, SS);

    // Se generan de sobra y se tira el 40 % con un patrón determinista.
    let k = tx.source_symbols() as usize;
    let todos = emitir(&mut tx, k * 3);
    let mut descartados = 0;
    for (i, sym) in todos.iter().enumerate() {
        if i % 5 < 2 {
            descartados += 1;
            continue;
        }
        rx.on_symbol(sym).unwrap();
        if rx.is_complete() {
            break;
        }
    }

    assert!(descartados > 0, "el test debe descartar algo");
    assert!(
        rx.is_complete(),
        "con el 60 % de un exceso de 3× debería sobrar para reconstruir"
    );
    assert_eq!(rx.take_object().unwrap(), original);
}

#[test]
fn el_orden_de_llegada_es_irrelevante() {
    let original = objeto(5_000);
    let mut tx = FountainSender::new(&original, SS);
    let k = tx.source_symbols() as usize;
    let mut todos = emitir(&mut tx, k * 2);

    // Barajado determinista: se invierte y se entrelaza.
    todos.reverse();
    let (a, b) = todos.split_at(todos.len() / 2);
    let entrelazado: Vec<_> = a.iter().zip(b.iter()).flat_map(|(x, y)| [x, y]).collect();

    let mut rx = FountainReceiver::new(original.len() as u64, SS);
    for sym in entrelazado {
        rx.on_symbol(sym).unwrap();
        if rx.is_complete() {
            break;
        }
    }

    assert_eq!(rx.take_object().unwrap(), original);
}

#[test]
fn un_objeto_vacio_esta_completo_de_entrada() {
    let mut tx = FountainSender::new(&[], SS);
    let mut rx = FountainReceiver::new(0, SS);

    assert!(tx.is_complete(), "no hay nada que emitir");
    assert!(rx.is_complete(), "no hay nada que esperar");
    assert!(
        tx.next_symbol(MAX).is_none(),
        "un objeto vacío no debe producir símbolos ni girar en el relleno"
    );
    assert_eq!(rx.take_object(), Some(Vec::new()));
}

/// `EncodingPacket::deserialize` indexa los cuatro primeros bytes sin
/// comprobarlos. Un símbolo truncado tiene que morir en la validación, no en un
/// pánico dentro de la librería.
#[test]
fn un_simbolo_truncado_no_provoca_panico() {
    let mut rx = FountainReceiver::new(10_000, SS);

    for len in [0usize, 1, 2, 3, 4, 5, MAX - 1, MAX + 1] {
        let err = rx
            .on_symbol(&Symbol {
                id: 0,
                bytes: vec![0; len],
            })
            .unwrap_err();
        assert_eq!(
            err,
            RecvError::SymbolSize {
                got: len,
                expected: MAX
            },
            "un símbolo de {len} B debería rechazarse limpiamente"
        );
    }
}

/// El bug que tuvo esta implementación: `take_object` vaciaba el `Option` y el
/// receptor pasaba a declararse incompleto justo después de entregar, lo que
/// habría hecho que su realimentación pidiera al emisor seguir emitiendo para
/// siempre.
#[test]
fn seguir_completo_despues_de_entregar_el_objeto() {
    let original = objeto(3_000);
    let mut tx = FountainSender::new(&original, SS);
    let mut rx = FountainReceiver::new(original.len() as u64, SS);

    let k = tx.source_symbols() as usize;
    for sym in emitir(&mut tx, k * 3) {
        rx.on_symbol(&sym).unwrap();
        if rx.is_complete() {
            break;
        }
    }

    assert_eq!(rx.take_object().unwrap(), original);
    assert!(rx.is_complete(), "sigue completo tras entregar");
    assert_eq!(
        rx.feedback(),
        Feedback::Fountain {
            complete: true,
            received: rx.received()
        },
        "la realimentación debe seguir diciendo que pare"
    );
    assert_eq!(rx.take_object(), None, "solo se entrega una vez");
}

#[test]
fn el_emisor_para_cuando_se_lo_dicen() {
    let original = objeto(10_000);
    let mut tx = FountainSender::new(&original, SS);

    assert!(tx.next_symbol(MAX).is_some());
    tx.on_feedback(&Feedback::Fountain {
        complete: true,
        received: 999,
    });

    assert!(tx.is_complete());
    assert!(
        tx.next_symbol(MAX).is_none(),
        "confirmado el final, no debe emitir más"
    );
}

#[test]
fn realimentacion_de_arq_se_ignora_sin_romper() {
    let original = objeto(3_000);
    let mut tx = FountainSender::new(&original, SS);
    tx.on_feedback(&Feedback::Selective {
        cumulative: 9_999,
        missing: vec![],
        window: 16,
    });
    assert!(
        !tx.is_complete(),
        "una realimentación de ARQ no debe completar una transferencia de fuente"
    );
}

#[test]
fn un_simbolo_que_no_cabe_no_se_emite() {
    let original = objeto(10_000);
    let mut tx = FountainSender::new(&original, SS);
    assert!(
        tx.next_symbol(MAX - 1).is_none(),
        "sin sitio para símbolo + identificador no debe emitir"
    );
    assert!(tx.next_symbol(MAX).is_some());
}

#[test]
fn el_emisor_genera_mas_simbolos_que_los_de_fuente() {
    let original = objeto(5_000);
    let mut tx = FountainSender::new(&original, SS);
    let k = tx.source_symbols() as usize;

    // Que pueda pasar de K no es un defecto: es el mecanismo. Sin reparación
    // ilimitada, una pérdida al final dejaría la transferencia colgada.
    let emitidos = emitir(&mut tx, k * 3).len();
    assert!(
        emitidos > k,
        "emitió {emitidos} con K={k}; la reparación debe ser ilimitada"
    );
}

#[test]
fn el_progreso_del_emisor_refleja_al_receptor_no_lo_emitido() {
    let original = objeto(10_000);
    let mut tx = FountainSender::new(&original, SS);
    emitir(&mut tx, 50);

    assert_eq!(
        tx.progress().have,
        0,
        "haber emitido 50 símbolos no es progreso mientras el receptor calle"
    );

    tx.on_feedback(&Feedback::Fountain {
        complete: false,
        received: 30,
    });
    assert_eq!(tx.progress().have, 30);
}

/// Regresión del bug que costó cuatro tests de integración.
///
/// El emisor y el receptor tienen que derivar **el mismo** tamaño de símbolo
/// efectivo. Cuando no coincidían, el receptor rechazaba todos los símbolos y
/// el síntoma era «recibidos: 0» — que parece un fallo de transporte y no de
/// parámetros. Los tests unitarios no lo vieron porque usan 200, que ya está
/// alineado a 8; hizo falta un tamaño realista para que saliera.
///
/// Lo que se afirma es la coincidencia, no un número concreto: el número
/// depende de cómo se construya el OTI, la coincidencia es el invariante.
#[test]
fn un_tamano_de_simbolo_no_alineado_sigue_funcionando() {
    const PEDIDO: u16 = 870;
    let original = objeto(20_000);

    let mut tx = FountainSender::new(&original, PEDIDO);
    let mut rx = FountainReceiver::new(original.len() as u64, PEDIDO);

    assert_eq!(
        tx.symbol_size(),
        rx.symbol_size(),
        "los dos lados deben derivar el mismo tamaño efectivo"
    );
    assert_eq!(tx.wire_len(), rx.wire_len());

    let ancho = tx.wire_len();
    let k = tx.source_symbols() as usize;
    for _ in 0..k * 3 {
        let Some(sym) = tx.next_symbol(ancho) else {
            break;
        };
        rx.on_symbol(&sym).expect("el receptor debe aceptarlo");
        if rx.is_complete() {
            break;
        }
    }

    assert!(rx.is_complete(), "debería haber reconstruido");
    assert_eq!(rx.take_object().unwrap(), original);
}

/// Construir el receptor con el plan que mandó el emisor es la vía preferente:
/// elimina de raíz la posibilidad de que los dos lados troceen distinto.
#[test]
fn el_receptor_puede_construirse_del_plan_del_emisor() {
    const PEDIDO: u16 = 870;
    let original = objeto(20_000);

    let mut tx = FountainSender::new(&original, PEDIDO);
    let oti = tx.oti_bytes().expect("objeto no vacío");
    let mut rx = FountainReceiver::from_oti_bytes(&oti);

    assert_eq!(tx.symbol_size(), rx.symbol_size());
    assert_eq!(tx.source_symbols(), rx.source_symbols_expected());

    let ancho = tx.wire_len();
    let k = tx.source_symbols() as usize;
    for _ in 0..k * 3 {
        let Some(sym) = tx.next_symbol(ancho) else {
            break;
        };
        rx.on_symbol(&sym).unwrap();
        if rx.is_complete() {
            break;
        }
    }
    assert_eq!(rx.take_object().unwrap(), original);
}

#[test]
fn un_objeto_vacio_no_tiene_plan_que_mandar() {
    let tx = FountainSender::new(&[], SS);
    assert_eq!(
        tx.oti_bytes(),
        None,
        "sin objeto no hay parámetros; el receptor lo resuelve con la longitud"
    );
}

#[test]
fn symbol_size_for_aprovecha_todo_el_payload() {
    assert_eq!(symbol_size_for(874), Some(870), "no se recorta nada");
    assert_eq!(symbol_size_for(904), Some(900));
    assert_eq!(symbol_size_for(5), Some(1));
    assert_eq!(symbol_size_for(4), None, "sin sitio para datos");
    assert_eq!(symbol_size_for(0), None);
}

/// Los bloques de fuente se acotan para que decodificar no se dispare. Sin este
/// tope, un objeto de 5 MB caía en un solo bloque de ~6000 símbolos y
/// reconstruirlo costaba más de nueve minutos de CPU.
#[test]
fn los_bloques_de_fuente_estan_acotados() {
    use optical_protocol::reliability::fountain::{plan, MAX_SYMBOLS_PER_BLOCK};

    let simbolo = 870u16;
    for mb in [1u64, 5, 20] {
        let len = mb * 1024 * 1024;
        let cfg = plan(len, simbolo);
        let total = len.div_ceil(u64::from(simbolo));
        let por_bloque = total.div_ceil(u64::from(cfg.source_blocks()));
        assert!(
            por_bloque <= u64::from(MAX_SYMBOLS_PER_BLOCK),
            "{mb} MB: {por_bloque} símbolos por bloque supera el tope de {MAX_SYMBOLS_PER_BLOCK}"
        );
    }
}
