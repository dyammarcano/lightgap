//! Diagnostic for the acoustic loop. Not a test: prints what each stage does to
//! the signal, so the failure can be located instead of guessed at.

use acoustic_codec::framing::Framer;
use acoustic_codec::fsk::{demodulate, modulate_frame, AcousticProfile, PREAMBLE};
use acoustic_codec::impair::{impair, Impairment};

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

fn main() {
    let p = AcousticProfile::conservative();
    let data: Vec<u8> = (0..8u8).collect();
    let bits = Framer::encode(&data).unwrap();
    let clean = modulate_frame(&bits, &p);
    let expect = Framer::encoded_bits(data.len());

    println!(
        "sps={} symbols={} samples={}",
        p.samples_per_symbol(),
        PREAMBLE.len() + bits.len(),
        clean.len()
    );
    println!("clean rms={:.4}", rms(&clean));
    println!();

    let cases: Vec<(&str, Impairment)> = vec![
        ("clean", Impairment::clean()),
        (
            "gain only",
            Impairment {
                gain: 0.6,
                ..Impairment::clean()
            },
        ),
        (
            "band only",
            Impairment {
                band: (300.0, 19000.0),
                ..Impairment::clean()
            },
        ),
        (
            "hp only",
            Impairment {
                band: (300.0, f32::INFINITY),
                ..Impairment::clean()
            },
        ),
        (
            "lp only",
            Impairment {
                band: (0.0, 19000.0),
                ..Impairment::clean()
            },
        ),
        (
            "drift only",
            Impairment {
                clock_drift: 5e-5,
                ..Impairment::clean()
            },
        ),
        ("snr 30", Impairment::clean().with_snr(30.0)),
        ("snr 20", Impairment::clean().with_snr(20.0)),
        ("snr 10", Impairment::clean().with_snr(10.0)),
        ("typical", Impairment::typical()),
    ];

    println!(
        "{:<12} {:>10} {:>8} {:>10} {:>7}",
        "case", "rms", "offset", "confid", "ok"
    );
    for (name, imp) in cases {
        let sig = impair(&clean, p.sample_rate, &imp);
        let r = rms(&sig);
        match demodulate(&sig, &p, expect) {
            Some(d) => {
                let ok = Framer::decode(&d.bits).map(|v| v == data).unwrap_or(false);
                println!(
                    "{name:<12} {r:>10.4} {:>8} {:>10.3} {:>7}",
                    d.offset, d.confidence, ok
                );
            }
            None => println!(
                "{name:<12} {r:>10.4} {:>8} {:>10} {:>7}",
                "-", "-", "no sync"
            ),
        }
    }
}
