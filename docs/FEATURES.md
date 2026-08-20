# Features

<!-- rev:001 (RFC 3339) 2026-08-20T16:05:00Z -->

## Implemented

| Feature | Notes |
|---|---|
| Optical transport over animated QR | Full transport layer: wire format with CRC32, session state machine, sequence numbers, retransmission. |
| Fountain coding (RaptorQ) | Order-independent, loss-tolerant. Verified moving 5 MB across a channel dropping 40% of frames. |
| Sliding-window ARQ | Selective retransmission, chosen per profile behind a trait. Verified at 15% loss. |
| Two-stage handshake | A bare 42-byte beacon acquires; the fuller announcement follows only once the peer has demonstrably read it. |
| Five-second lock | On acquiring a peer, the code freezes and says so, giving the other end a stationary target. |
| X25519 pairing with a six-digit string | Per-direction keys, nonce derived from `(session, direction, seq)`. Both ends derive the same digits. |
| Frame-size calibration | Seeded from the pair's real camera and display sizes, then refined by AIMD from the peer's own reports. |
| Brightness calibration | Descends from full output while the peer reports clean reads; sweeps blindly when the peer cannot report at all. |
| Camera exposure calibration | Driven by the fraction of the frame at the top of the sensor's range, measured off the decoder's own buffer. |
| Region tracking | Wide search until a code is found, then native-resolution tracking of its neighbourhood. |
| Bidirectional link indicators | Two, not one: seeing and being seen are different questions and the link fails one direction at a time. |
| Live throughput, both directions | Labelled honestly — down is measured, up is only offered. |
| Camera selection, remembered | Keyed on the device label, because identifiers are re-salted between sessions. |
| Desktop and Android from one tree | Same interface, same protocol; platform differences gated rather than forked. |
| Synthetic camera | Perspective warp, supersampling, blur, noise, moiré. Turns a two-device test into a CI test. |
| Acoustic 2-FSK codec | Modulation, Goertzel detection, framing, band assignment. Tested; no driver. |

## Proposed

| Feature | Why |
|---|---|
| Encryption on the data path | The keys are agreed and unused. See [ISSUES.md](ISSUES.md). |
| An acoustic driver | Would carry control traffic and acknowledgements, sparing the optical channel a round trip. |
| Per-direction hold time | The display hold is one number for both directions; a peer that reads badly would benefit from longer. |
| Autofocus control | Untouched, and a screen at close range defeats it routinely. A candidate explanation for the open read-rate bug. |
| Resume after a lost peer | A transfer currently restarts. Fountain coding makes resuming natural — the receiver keeps what it has. |
