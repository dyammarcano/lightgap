//! Mide cuántos píxeles por módulo hace falta de verdad, barriendo escalas
//! fraccionarias. El resultado fija MIN_PIXELS_PER_MODULE con datos.

use optical_codec::decode::scan_greyscale;
use optical_codec::distort::{capture, Conditions};
use optical_codec::encode::{encode, Ecc};

fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

fn main() {
    // Bins de 0,5 px/módulo entre 2 y 10.
    let mut ok = [0u32; 20];
    let mut total = [0u32; 20];

    for n in [80usize, 150, 250, 400, 600, 900] {
        let Ok(m) = encode(&payload(n), Ecc::Q) else {
            continue;
        };
        let modulos = m.size() as f32;
        for paso in 0..90 {
            let fill = 0.15 + paso as f32 * 0.009;
            let cond = Conditions {
                fill,
                ..Conditions::ideal()
            };
            let (w, h, px) = capture(&m, &cond);
            let scan = scan_greyscale(w, h, &px);
            let leido = scan
                .detections
                .first()
                .map(|d| d.payload == payload(n))
                .unwrap_or(false);

            // px/módulo esperado del encuadre, no del detectado: si no se
            // detecta nada no hay geometría de la que leerlo.
            let lado = (w.min(h) as f32) * fill;
            let ppm = lado * (modulos / (modulos + 8.0)) / modulos;

            let bin = ((ppm - 2.0) / 0.5).floor();
            if (0.0..20.0).contains(&bin) {
                let b = bin as usize;
                total[b] += 1;
                if leido {
                    ok[b] += 1;
                }
            }
        }
    }

    println!("px/mod    leidos/total   tasa");
    for b in 0..20 {
        if total[b] == 0 {
            continue;
        }
        let lo = 2.0 + b as f32 * 0.5;
        println!(
            "{lo:4.1}-{:4.1}  {:5}/{:<5}   {:5.1}%",
            lo + 0.5,
            ok[b],
            total[b],
            100.0 * ok[b] as f32 / total[b] as f32
        );
    }
}
