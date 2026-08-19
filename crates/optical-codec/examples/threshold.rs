//! Measures how many pixels per module are actually needed, by sweeping
//! fractional scales. The result is what fixes `MIN_PIXELS_PER_MODULE` from data
//! rather than from intuition.

use optical_codec::decode::scan_greyscale;
use optical_codec::distort::{capture, Conditions};
use optical_codec::encode::{encode, Ecc};

fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

fn main() {
    // The threshold is not one number: it depends on the capture conditions,
    // and measuring it only under ideal ones understates what a real link needs.
    for (name, base) in [
        ("ideal", Conditions::ideal()),
        ("typical", Conditions::typical()),
        ("harsh", Conditions::harsh()),
    ] {
        println!();
        println!("=== {name} conditions ===");
        sweep(&base);
    }
}

fn sweep(base: &Conditions) {
    // Bins of 0.5 px/module between 2 and 12.
    let mut ok = [0u32; 20];
    let mut total = [0u32; 20];

    for n in [80usize, 150, 250, 400, 600, 900] {
        let Ok(m) = encode(&payload(n), Ecc::Q) else {
            continue;
        };
        let modules = m.size() as f32;
        for step in 0..90 {
            let fill = 0.15 + step as f32 * 0.009;
            let cond = Conditions {
                fill,
                ..base.clone()
            };
            let (w, h, px) = capture(&m, &cond);
            let scan = scan_greyscale(w, h, &px);
            let read = scan
                .detections
                .first()
                .map(|d| d.payload == payload(n))
                .unwrap_or(false);

            // Pixels per module expected from the framing, not from the detected
            // geometry: if nothing is detected there is no geometry to read it
            // from.
            let side = (w.min(h) as f32) * fill;
            let ppm = side * (modules / (modules + 8.0)) / modules;

            let bin = ((ppm - 2.0) / 0.5).floor();
            if (0.0..20.0).contains(&bin) {
                let b = bin as usize;
                total[b] += 1;
                if read {
                    ok[b] += 1;
                }
            }
        }
    }

    println!("px/module   read/total    rate");
    for b in 0..20 {
        if total[b] == 0 {
            continue;
        }
        let lo = 2.0 + b as f32 * 0.5;
        println!(
            "{lo:4.1}-{:4.1}   {:5}/{:<5}   {:5.1}%",
            lo + 0.5,
            ok[b],
            total[b],
            100.0 * ok[b] as f32 / total[b] as f32
        );
    }
}
