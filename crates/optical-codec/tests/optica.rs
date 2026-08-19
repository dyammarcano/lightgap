//! El lazo óptico completo, sin cámara: bytes → QR → cámara sintética → bytes.
//!
//! Es lo que convierte «poner dos portátiles enfrentados» en algo que corre en
//! CI. Además permite barrer condiciones de forma sistemática —enfoque, ángulo,
//! distancia, luz— que a mano no se hace nunca.

use optical_codec::decode::scan_pdus;
use optical_codec::distort::{capture, Conditions};
use optical_codec::encode::{encode, max_payload, Ecc};
use optical_codec::geometry::{advise, Advice, MIN_PIXELS_PER_MODULE};
use optical_codec::scan_greyscale;
use optical_protocol::wire::{Flags, Pdu, PduKind};

fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

fn pdu(n: usize) -> Pdu {
    Pdu {
        session_id: 0x0123_4567_89ab_cdef,
        kind: PduKind::Data,
        flags: Flags::FOUNTAIN,
        seq: 4242,
        ack: 0,
        payload: payload(n),
    }
}

/// Va y vuelve por el lazo óptico. Devuelve el payload leído, si se leyó.
fn ida_y_vuelta(datos: &[u8], ecc: Ecc, cond: &Conditions) -> Option<Vec<u8>> {
    let modules = encode(datos, ecc).ok()?;
    let (w, h, px) = capture(&modules, cond);
    let scan = scan_greyscale(w, h, &px);
    scan.detections.first().map(|d| d.payload.clone())
}

#[test]
fn el_lazo_se_cierra_en_condiciones_ideales() {
    // 200 B con corrección Q son 65 módulos; a fill 0,7 sobre 720p salen ~7
    // px/módulo. Con 400 B serían 89 módulos y 5,2 px/módulo, por debajo del
    // umbral medido — y fallaría con razón, no por un defecto del códec.
    let datos = payload(200);
    assert_eq!(
        ida_y_vuelta(&datos, Ecc::Q, &Conditions::ideal()).as_deref(),
        Some(&datos[..]),
        "de frente, enfocado y sin ruido debe leerse exacto"
    );
}

#[test]
fn el_lazo_se_cierra_en_condiciones_tipicas() {
    // 200 B con corrección Q son 65 módulos, que a fill 0,75 sobre 720p dan
    // ~7,4 px/módulo: por encima del umbral medido.
    let datos = payload(200);
    assert_eq!(
        ida_y_vuelta(&datos, Ecc::Q, &Conditions::typical()).as_deref(),
        Some(&datos[..]),
        "una webcam decente sobre una mesa debe leer sin problema"
    );
}

/// El caso que de verdad importa: condiciones malas pero plausibles.
///
/// No se afirma un payload concreto sino que **queda capacidad utilizable**.
/// Cuánta exactamente depende del umbral de píxeles por módulo, y fijar aquí un
/// número lo convertiría en una constante mágica que se rompe al tocar el
/// códec. Lo que no puede pasar es que en condiciones duras no entre nada.
#[test]
fn el_lazo_aguanta_condiciones_duras() {
    let mut maximo = 0usize;
    for tam in [50usize, 100, 150, 200, 300, 400] {
        let datos = payload(tam);
        if ida_y_vuelta(&datos, Ecc::H, &Conditions::harsh()).as_deref() == Some(&datos[..]) {
            maximo = tam;
        }
    }
    assert!(
        maximo >= 50,
        "con corrección alta debería entrar algo aun con mano temblorosa y poca luz"
    );
    println!("payload legible en condiciones duras (Ecc::H): {maximo} B");
}

#[test]
fn una_pdu_entera_sobrevive_al_lazo() {
    let original = pdu(150);
    let bytes = original.to_vec().unwrap();
    let modules = encode(&bytes, Ecc::Q).unwrap();
    let (w, h, px) = capture(&modules, &Conditions::typical());

    let (pdus, scan) = scan_pdus(w, h, &px);
    assert_eq!(scan.detections.len(), 1, "debería verse un solo código");
    assert_eq!(pdus, vec![original], "la PDU debe llegar intacta");
}

/// El CRC de la PDU tiene que cazar lo que la corrección del QR no arregle.
/// Si un marco ilegible se colara como PDU válida, el archivo saldría corrupto.
#[test]
fn ninguna_lectura_erronea_pasa_como_pdu_valida() {
    let original = pdu(200);
    let bytes = original.to_vec().unwrap();
    let modules = encode(&bytes, Ecc::L).unwrap();

    let mut leidas = 0;
    let mut vistas = 0;

    // Se barre hasta romper el enlace: lo que se afirma no es que se lea, sino
    // que lo que se lea sea correcto o no se lea nada.
    for paso in 0..25 {
        let cond = Conditions {
            blur: paso as f32 * 0.35,
            noise: paso as f32 * 2.0,
            contrast: 1.0 - paso as f32 * 0.03,
            fill: 0.6,
            tilt_x: 0.05,
            seed: 1000 + paso,
            ..Conditions::default()
        };
        let (w, h, px) = capture(&modules, &cond);
        let (pdus, scan) = scan_pdus(w, h, &px);
        vistas += scan.grids_seen();
        for p in &pdus {
            leidas += 1;
            assert_eq!(
                p, &original,
                "una PDU que pasa la validación tiene que ser la original (paso {paso})"
            );
        }
    }

    assert!(vistas > 0, "el barrido debería haber visto códigos");
    assert!(leidas > 0, "y haber leído alguno correctamente");
}

#[test]
fn un_codigo_demasiado_lejos_no_se_lee_pero_se_avisa() {
    let datos = payload(600);
    let modules = encode(&datos, Ecc::Q).unwrap();

    // Ocupando el 12 % del frame, un código denso cae por debajo del mínimo de
    // píxeles por módulo.
    let cond = Conditions {
        fill: 0.12,
        ..Conditions::ideal()
    };
    let (w, h, px) = capture(&modules, &cond);
    let scan = scan_greyscale(w, h, &px);

    assert!(
        scan.detections.is_empty(),
        "a esta distancia no debería poder leerse"
    );
    if let Some(g) = scan.best_geometry() {
        assert!(
            g.pixels_per_module < MIN_PIXELS_PER_MODULE,
            "si se vio la rejilla, debe quedar por debajo del mínimo"
        );
    }
}

#[test]
fn la_geometria_refleja_el_encuadre() {
    let datos = payload(200);
    let modules = encode(&datos, Ecc::Q).unwrap();

    let grande = Conditions {
        fill: 0.8,
        ..Conditions::ideal()
    };
    let pequeno = Conditions {
        fill: 0.5,
        ..Conditions::ideal()
    };

    let (w1, h1, p1) = capture(&modules, &grande);
    let (w2, h2, p2) = capture(&modules, &pequeno);

    let g1 = scan_greyscale(w1, h1, &p1).best_geometry().expect("visto");
    let g2 = scan_greyscale(w2, h2, &p2).best_geometry().expect("visto");

    assert!(
        g1.frame_coverage > g2.frame_coverage,
        "más cerca debe ocupar más frame"
    );
    assert!(
        g1.pixels_per_module > g2.pixels_per_module,
        "más cerca debe dar más píxeles por módulo"
    );
    assert!(
        g1.side_px > g2.side_px,
        "más cerca debe medir más lado en píxeles"
    );
}

#[test]
fn el_descentrado_se_detecta() {
    let datos = payload(200);
    let modules = encode(&datos, Ecc::Q).unwrap();

    let centrado = Conditions {
        fill: 0.7,
        ..Conditions::ideal()
    };
    let torcido = Conditions {
        fill: 0.7,
        offset_x: 0.2,
        ..Conditions::ideal()
    };

    let (w1, h1, p1) = capture(&modules, &centrado);
    let (w2, h2, p2) = capture(&modules, &torcido);

    let g1 = scan_greyscale(w1, h1, &p1).best_geometry().unwrap();
    let g2 = scan_greyscale(w2, h2, &p2).best_geometry().unwrap();

    assert!(g1.offset < 0.1, "centrado debería dar desplazamiento bajo");
    assert!(g2.offset > g1.offset, "descentrado debería notarse");
}

#[test]
fn el_desenfoque_baja_la_nitidez_medida() {
    let datos = payload(200);
    let modules = encode(&datos, Ecc::Q).unwrap();

    let nitido = Conditions {
        fill: 0.6,
        ..Conditions::ideal()
    };
    let borroso = Conditions {
        fill: 0.6,
        blur: 3.0,
        ..Conditions::ideal()
    };

    let (w1, h1, p1) = capture(&modules, &nitido);
    let (w2, h2, p2) = capture(&modules, &borroso);

    let s1 = scan_greyscale(w1, h1, &p1).sharpness;
    let s2 = scan_greyscale(w2, h2, &p2).sharpness;

    assert!(
        s1 > s2 * 2.0,
        "la varianza del laplaciano debe desplomarse al desenfocar: {s1:.1} vs {s2:.1}"
    );
}

#[test]
fn el_consejo_prioriza_lo_que_de_verdad_bloquea() {
    let datos = payload(200);
    let modules = encode(&datos, Ecc::Q).unwrap();

    // Lejos: por muy bien centrado y enfocado que esté, sin píxeles por módulo
    // no hay nada que hacer, y ese debe ser el consejo.
    let lejos = Conditions {
        fill: 0.15,
        ..Conditions::ideal()
    };
    let (w, h, px) = capture(&modules, &lejos);
    let scan = scan_greyscale(w, h, &px);
    if let Some(g) = scan.best_geometry() {
        assert_eq!(advise(&g, scan.sharpness), Advice::MoveCloser);
    }

    // Encuadre bueno: sin quejas.
    let bien = Conditions {
        fill: 0.7,
        ..Conditions::ideal()
    };
    let (w, h, px) = capture(&modules, &bien);
    let scan = scan_greyscale(w, h, &px);
    let g = scan.best_geometry().expect("visto");
    assert_eq!(advise(&g, scan.sharpness), Advice::Ok);
}

#[test]
fn cada_nivel_de_correccion_reduce_la_capacidad() {
    let caps: Vec<usize> = Ecc::all().iter().map(|e| max_payload(*e)).collect();
    for par in caps.windows(2) {
        assert!(
            par[0] > par[1],
            "más corrección debe dejar menos sitio: {caps:?}"
        );
    }
    // Referencia del estándar: versión 40 en modo byte con corrección L.
    assert_eq!(caps[0], 2953);
}

#[test]
fn un_payload_que_no_cabe_se_rechaza_al_codificar() {
    // Con el mismo tipo de relleno con el que se midió la cota: contenido con
    // tramos de dígitos ASCII entra en modo numérico y cabe más, así que
    // comparar contra otro contenido no probaría nada.
    let demasiado = vec![0u8; max_payload(Ecc::H) + 1];
    assert!(encode(&demasiado, Ecc::H).is_err());
    assert!(encode(&vec![0u8; max_payload(Ecc::H)], Ecc::H).is_ok());
}

#[test]
fn el_frame_sin_codigo_no_inventa_detecciones() {
    let (w, h) = (640usize, 480usize);
    let liso = vec![200u8; w * h];
    let scan = scan_greyscale(w, h, &liso);

    assert_eq!(scan.grids_seen(), 0);
    assert_eq!(scan.best_geometry(), None);
}

#[test]
fn un_frame_vacio_o_incoherente_no_provoca_panico() {
    assert_eq!(scan_greyscale(0, 0, &[]).grids_seen(), 0);
    assert_eq!(scan_greyscale(10, 10, &[0u8; 5]).grids_seen(), 0);
    assert_eq!(scan_greyscale(100, 0, &[0u8; 10]).grids_seen(), 0);
}

/// Barrido sistemático: cuánta densidad aguanta cada nivel de corrección bajo
/// condiciones típicas. Es la clase de dato que la calibración de la Fase 3 va
/// a medir en vivo, y tenerlo en CI detecta regresiones del códec.
#[test]
fn barrido_de_densidad_por_nivel_de_correccion() {
    let mut resumen = Vec::new();

    for ecc in Ecc::all() {
        let mut maximo = 0usize;
        for tam in (100..=1600).step_by(100) {
            let bytes = payload(tam);
            if encode(&bytes, ecc).is_err() {
                break;
            }
            let cond = Conditions {
                fill: 0.8,
                ..Conditions::typical()
            };
            if ida_y_vuelta(&bytes, ecc, &cond).as_deref() == Some(&bytes[..]) {
                maximo = tam;
            }
        }
        resumen.push((ecc, maximo));
        assert!(
            maximo >= 100,
            "con {ecc:?} debería leerse al menos 100 B en condiciones típicas"
        );
    }

    println!("payload máximo legible en condiciones típicas: {resumen:?}");
}

/// Cuánto payload cabe **de forma fiable** por marco a 720p.
///
/// «Fiable» y no «alguna vez»: con 3,3 px/módulo un marco decodifica a veces, y
/// quedarse con ese número llevaría a negociar un perfil que falla uno de cada
/// cuatro marcos. Aquí se exige acertar en todas las repeticiones, que es el
/// criterio con el que la calibración de la Fase 3 tiene que elegir.
///
/// Tenerlo en CI convierte una regresión del códec —o del umbral de píxeles por
/// módulo— en un fallo con un número, en vez de en «va más lento».
#[test]
fn capacidad_fiable_por_marco_a_720p() {
    const REPETICIONES: u64 = 4;

    let mut resumen = Vec::new();
    for ecc in Ecc::all() {
        let mut maximo = 0usize;
        for tam in (100..=1400).step_by(100) {
            let datos = payload(tam);
            if encode(&datos, ecc).is_err() {
                break;
            }
            // Se varía la semilla y un poco el encuadre: un perfil que solo
            // funciona con la mano perfectamente quieta no es utilizable.
            let fiable = (0..REPETICIONES).all(|r| {
                let cond = Conditions {
                    fill: 0.75 - r as f32 * 0.01,
                    noise: 2.0,
                    seed: 7000 + r,
                    ..Conditions::ideal()
                };
                ida_y_vuelta(&datos, ecc, &cond).as_deref() == Some(&datos[..])
            });
            if fiable {
                maximo = tam;
            } else {
                break;
            }
        }
        resumen.push((ecc, maximo));
    }

    println!("payload fiable por marco a 720p, fill ~0,75: {resumen:?}");

    let h = resumen.iter().find(|(e, _)| *e == Ecc::H).unwrap().1;
    assert!(h >= 100, "con Ecc::H solo entran {h} B fiables por marco");

    let l = resumen.iter().find(|(e, _)| *e == Ecc::L).unwrap().1;
    assert!(l >= h, "L={l} debería admitir al menos tanto como H={h}");
}
