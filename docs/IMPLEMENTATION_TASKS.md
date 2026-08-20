# Implementation tasks

<!-- rev:001 (RFC 3339) 2026-08-20T16:05:00Z -->

Granular work behind the open items in [MILESTONES.md](MILESTONES.md) and
[BACKLOG.md](BACKLOG.md), in an order that respects what depends on what.

Effort: **S** under an hour · **M** a few hours · **L** a day or more.

## Diagnosis — blocks everything in v0.2

| ID | What | Files | Effort |
|---|---|---|---|
| D-1 | Measure read rate at three distances on the marginal direction, with the code's pixel width recorded at each. Separates a focus explanation from a framing one. | none — procedure | S |
| D-2 | Expose autofocus capability and current mode alongside exposure in the metrics panel. It is untouched today and a screen at close range defeats it routinely. | `tauri-app/src/app.rs` | S |
| D-3 | Make the display hold per-direction, lengthening it toward a peer reporting bad reads. One number serves both directions today. | `engine.rs`, `session.rs` (report already carries quality) | M |

D-1 first: it is ten minutes and it decides whether D-2 or D-3 is worth doing.

## Encryption on the data path

Depends on nothing. Blocked only by care.

| ID | What | Files | Effort |
|---|---|---|---|
| E-1 | Seal outgoing data payloads with the direction's key and the sequence number as nonce. | `crates/modem/src/lib.rs` | M |
| E-2 | Open incoming payloads, and treat an authentication failure as a rejected frame rather than an error. | `crates/modem/src/lib.rs` | M |
| E-3 | Set and require the `ENCRYPTED` flag once keys exist, so an unencrypted data frame after pairing is refused. | `optical-protocol/src/wire.rs`, `modem` | S |
| E-4 | A test that a payload sealed for one direction cannot be opened as the other. | `crates/modem/tests/` | S |

Getting the direction or sequence wrong produces a session where each side can
encrypt and neither can decrypt, which on this link is indistinguishable from a
dirty lens. E-4 is what makes that visible.

## Surviving a lost peer

| ID | What | Files | Effort |
|---|---|---|---|
| R-1 | Keep the receiver's accumulated symbols across a peer loss rather than discarding them. Fountain coding makes this natural: symbols are order-independent. | `crates/modem/src/lib.rs` | M |
| R-2 | Re-announce the object on re-pairing and let the receiver say what it still needs. | `modem`, `metadata` | M |
| R-3 | A test that a transfer interrupted mid-object completes after a reconnect without resending what arrived. | `crates/modem/tests/end_to_end.rs` | M |

## The acoustic driver

| ID | What | Files | Effort |
|---|---|---|---|
| A-1 | `cpal` capture and playback behind ring buffers, as a `Channel` implementation. | new `crates/acoustic-driver` | L |
| A-2 | Device enumeration and per-band noise floor, feeding the existing calibration. | `acoustic-codec/src/calibration.rs` | M |
| A-3 | Strictly alternating sweep, sequenced over the optical channel that is already up. | `modem`, session | M |
| A-4 | Route acknowledgements to audio when viability allows, falling back on sustained loss. | `optical-protocol/src/mux.rs` | M |

The multiplexer already classifies traffic and duplicates control across
channels; A-4 is wiring rather than design.

## Release engineering

| ID | What | Files | Effort |
|---|---|---|---|
| S-1 | Signing key in repository secrets; `keyAlias`/`storeFile` wiring. The Gradle file is committed with deliberate edits, so it is modified, never regenerated. | `gen/android/app/build.gradle.kts`, `.github/workflows/release.yml` | M |
| S-2 | Desktop signing certificates per platform. | `.github/workflows/release.yml` | M |

## Cleanup

| ID | What | Files | Effort |
|---|---|---|---|
| C-1 | Say nothing until something has been seen: `last_advice` starts as `MoveCloser`, so the interface advises before its camera has observed anything. | `tauri-app/src-tauri/src/engine.rs` | S |
| C-2 | Find why every log line is emitted twice on Android, with one target registered at a time. | `tauri-app/src-tauri/src/lib.rs` | S |
