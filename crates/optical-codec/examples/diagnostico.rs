//! Diagnóstico del lazo óptico sintético. No es un test: imprime la tabla que
//! permite elegir constantes con datos en vez de con intuición.

use optical_codec::decode::scan_greyscale;
use optical_codec::distort::{capture, Conditions};
use optical_codec::encode::{encode, max_payload, Ecc};

fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

fn main() {
    control_sin_camara();

    println!("=== capacidad por nivel ===");
    for ecc in Ecc::all() {
        let cap = max_payload(ecc);
        let m = encode(&payload(cap), ecc).unwrap();
        let m1 = encode(&payload(cap + 1), ecc);
        println!(
            "{ecc:?}: max={cap} modulos={} | cap+1 codifica? {}",
            m.size(),
            m1.is_ok()
        );
    }

    println!();
    println!("=== modulos por payload (Ecc::Q) ===");
    for n in [100usize, 200, 300, 400, 600, 900, 1200] {
        if let Ok(m) = encode(&payload(n), Ecc::Q) {
            println!("{n} B -> {} modulos", m.size());
        }
    }

    println!();
    println!("=== px/modulo y lectura, frame 1280x720 ===");
    println!("payload  fill  modulos  px/mod  blur  leido");
    for n in [100usize, 200, 400] {
        let m = encode(&payload(n), Ecc::Q).unwrap();
        for fill in [0.4f32, 0.6, 0.8] {
            for blur in [0.0f32, 1.2, 2.4] {
                let cond = Conditions {
                    fill,
                    blur,
                    ..Conditions::ideal()
                };
                let (w, h, px) = capture(&m, &cond);
                let scan = scan_greyscale(w, h, &px);
                let ppm = scan
                    .best_geometry()
                    .map(|g| g.pixels_per_module)
                    .unwrap_or(0.0);
                let ok = scan
                    .detections
                    .first()
                    .map(|d| d.payload == payload(n))
                    .unwrap_or(false);
                println!(
                    "{n:7}  {fill:.1}  {:7}  {ppm:6.2}  {blur:.1}  {}",
                    m.size(),
                    if ok { "si" } else { "NO" }
                );
            }
        }
    }

    println!();
    println!("=== perfiles de condiciones (Ecc::Q, 200 B) ===");
    let m = encode(&payload(200), Ecc::Q).unwrap();
    for (nombre, cond) in [
        ("ideal", Conditions::ideal()),
        ("typical", Conditions::typical()),
        ("harsh", Conditions::harsh()),
    ] {
        let (w, h, px) = capture(&m, &cond);
        let scan = scan_greyscale(w, h, &px);
        println!(
            "{nombre:8}: rejillas={} leido={} ppm={:.2} nitidez={:.1}",
            scan.grids_seen(),
            scan.detections
                .first()
                .map(|d| d.payload == payload(200))
                .unwrap_or(false),
            scan.best_geometry()
                .map(|g| g.pixels_per_module)
                .unwrap_or(0.0),
            scan.sharpness
        );
    }
}

/// Control: renderizado directo, sin proyección ni cámara. Aísla el detector.
fn control_sin_camara() {
    println!();
    println!("=== control: render directo, sin proyeccion ===");
    println!("payload  modulos  px/mod  leido");
    for n in [100usize, 200, 400, 800] {
        let m = encode(&payload(n), Ecc::Q).unwrap();
        for escala in [2usize, 3, 4, 6, 8] {
            let (w, h, px) = m.render_greyscale(escala, 4);
            let scan = scan_greyscale(w, h, &px);
            let ok = scan
                .detections
                .first()
                .map(|d| d.payload == payload(n))
                .unwrap_or(false);
            println!(
                "{n:7}  {:7}  {escala:6}  {}",
                m.size(),
                if ok { "si" } else { "NO" }
            );
        }
    }
}
