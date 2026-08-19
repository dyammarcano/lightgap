//! Diagnostic for the optical transfer loop. Prints what each frame is and
//! whether the camera could read it, so a stall can be located rather than
//! guessed at.

use std::time::Duration;

use modem::{Event, Modem};
use optical_codec::distort::{capture, Conditions};
use optical_codec::encode::{encode, Ecc};
use optical_codec::scan_greyscale;
use optical_protocol::session::PeerId;
use optical_protocol::wire::{Pdu, OVERHEAD};

fn peer(n: u8) -> PeerId {
    let mut b = [0u8; 16];
    b[0] = n;
    PeerId::from_bytes(b)
}

fn main() {
    let payload = 200usize;
    let mtu = payload + OVERHEAD;
    let mut a = Modem::new(peer(1), mtu);
    let mut b = Modem::new(peer(9), mtu);

    let original: Vec<u8> = (0..2000u32).map(|i| (i % 251) as u8).collect();
    a.send_file("probe", original.clone());

    println!("mtu={mtu} payload_per_frame={}", a.payload_per_frame());

    let cond = Conditions {
        fill: 0.75,
        ..Conditions::typical()
    };

    let shoot = |frame: &[u8]| -> (usize, Option<Vec<u8>>) {
        let Ok(modules) = encode(frame, Ecc::Q) else {
            return (0, None);
        };
        let (w, h, px) = capture(&modules, &cond);
        let scan = scan_greyscale(w, h, &px);
        (
            modules.size(),
            scan.detections.first().map(|d| d.payload.clone()),
        )
    };

    let mut now = Duration::ZERO;
    println!("tick  dir  kind          bytes  modules  read");
    for tick in 0..60 {
        now += Duration::from_millis(80);
        a.tick(now);
        b.tick(now);

        if let Some(frame) = a.poll_frame() {
            let kind = Pdu::decode(&frame)
                .map(|p| format!("{:?}", p.kind))
                .unwrap_or("?".into());
            let (m, seen) = shoot(&frame);
            println!(
                "{tick:4}  a>b  {kind:<12} {:5}  {m:7}  {}",
                frame.len(),
                if seen.is_some() { "yes" } else { "NO" }
            );
            if let Some(s) = seen {
                for e in b.handle_frame(&s) {
                    if let Event::FileReceived { name, bytes } = e {
                        println!(
                            "  -> received {name} ({} B, match={})",
                            bytes.len(),
                            bytes == original
                        );
                        return;
                    }
                }
            }
        } else {
            println!("{tick:4}  a>b  (nothing)");
        }

        if let Some(frame) = b.poll_frame() {
            let kind = Pdu::decode(&frame)
                .map(|p| format!("{:?}", p.kind))
                .unwrap_or("?".into());
            let (m, seen) = shoot(&frame);
            println!(
                "{tick:4}  b>a  {kind:<12} {:5}  {m:7}  {}",
                frame.len(),
                if seen.is_some() { "yes" } else { "NO" }
            );
            if let Some(s) = seen {
                a.handle_frame(&s);
            }
        }
    }
    println!(
        "stopped without completing; rx progress {:.3}",
        b.receive_progress()
    );
}
