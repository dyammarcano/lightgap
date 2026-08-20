# Milestones

<!-- rev:002 (RFC 3339) 2026-08-20T16:03:37Z -->

No tags have been cut. The versions below describe what was reached, and what
the next one would need.

## v0.1 — a link that exists · reached

The full stack, from wire format to two real devices reading each other.

- [x] Sans-io protocol core: wire format, session, leader election
- [x] Fountain coding and ARQ behind a common trait
- [x] Optical codec with a synthetic camera for testing
- [x] Calibration crate: ladder, goodput scoring, AIMD
- [x] Acoustic codec, tested against synthetic impairment
- [x] Transfer engine composing all of it
- [x] Desktop and Android applications from one source tree
- [x] X25519 pairing with a comparable six-digit string
- [x] Frame size, brightness and exposure calibrated from measurement
- [x] Two physical devices discovering each other, pairing, and agreeing digits

Measured at this point: a 5 MB object across a channel losing 40% of frames; a
100 kB object through rendered QR codes photographed by the synthetic camera;
and, on real hardware, 1161 B/s in the working direction with a frame that
climbed from 82 to 306 bytes on its own.

**Coverage at this milestone:** 91.4% of regions across the library crates,
60.4% across the workspace — see [ROADMAP.md](ROADMAP.md) for why the two
figures differ so much and which one to watch.

## v0.2 — a link that finishes · next

The gap between "two devices read each other" and "a file arrives".

- [ ] The desktop direction reads better than one frame in ten — see
      [BUGS.md](BUGS.md); this gates everything else here
- [ ] A large file transferred end to end over real hardware, measured
- [ ] Encryption wired into the data path
- [ ] A transfer that survives a lost peer instead of restarting

**Coverage target:** hold the library crates at or above 90%. Encryption on the
data path and reconnect handling both land in `modem`, the lowest-covered
library crate at 85.5%; neither should lower it.

## v0.3 — a second medium

- [ ] A `cpal` driver behind the acoustic codec
- [ ] Acknowledgements routed over audio when calibration says it is viable
- [ ] Honest reporting when it is not, with automatic fallback to optical only

## Not scheduled

Signed releases, an iOS build. Both are decisions rather than work: signing
needs certificates and a keystore, iOS needs macOS and Xcode.
