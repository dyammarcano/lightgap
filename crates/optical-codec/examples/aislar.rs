//! Experimento para separar hipótesis sobre por qué falla la cámara sintética.

use optical_codec::decode::scan_greyscale;
use optical_codec::encode::{encode, Ecc};

fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

/// Reescala por área exacta (sin proyección) y coloca sobre un fondo dado.
fn reescalar(
    src: &[u8],
    sw: usize,
    sh: usize,
    dw: usize,
    dh: usize,
    fondo: u8,
    margen: usize,
) -> (usize, usize, Vec<u8>) {
    let fw = dw + margen * 2;
    let fh = dh + margen * 2;
    let mut out = vec![fondo; fw * fh];
    for y in 0..dh {
        for x in 0..dw {
            // Promedio sobre la huella exacta del píxel de destino.
            let x0 = x * sw / dw;
            let x1 = (((x + 1) * sw).div_ceil(dw)).min(sw).max(x0 + 1);
            let y0 = y * sh / dh;
            let y1 = (((y + 1) * sh).div_ceil(dh)).min(sh).max(y0 + 1);
            let mut acc = 0u32;
            let mut n = 0u32;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    acc += u32::from(src[sy * sw + sx]);
                    n += 1;
                }
            }
            out[(y + margen) * fw + (x + margen)] = (acc / n.max(1)) as u8;
        }
    }
    (fw, fh, out)
}

fn main() {
    println!("payload  modulos  destino  fondo  leido");
    for n in [100usize, 200, 400] {
        let m = encode(&payload(n), Ecc::Q).unwrap();
        let (sw, sh, src) = m.render_greyscale(10, 4);
        for destino in [200usize, 288, 400, 500] {
            for fondo in [255u8, 235] {
                let (w, h, px) = reescalar(&src, sw, sh, destino, destino, fondo, 60);
                let scan = scan_greyscale(w, h, &px);
                let ok = scan
                    .detections
                    .first()
                    .map(|d| d.payload == payload(n))
                    .unwrap_or(false);
                println!(
                    "{n:7}  {:7}  {destino:7}  {fondo:5}  {}",
                    m.size(),
                    if ok { "si" } else { "NO" }
                );
            }
        }
    }
}
