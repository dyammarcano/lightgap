# ADR-0001 — The protocol core performs no I/O

**Status:** Accepted

## Context

This project moves files between two devices using a display and a camera. Every
reliability property that matters — retransmission, ordering, loss tolerance,
leader election, key agreement — lives in that protocol. And every one of them is
hard to observe through a camera: reproducing a specific loss pattern requires
two physical devices, a controlled light source, and the patience to run the same
scenario until it recurs.

The original sketch put the medium's concerns inside the session, including
states like `AudioNoiseMeasurement` and `AudioFrequencySweep`. That ties the
session to audio: adding a third medium would mean editing the handshake.

## Decision

The protocol is a pure state machine. It opens no sockets, no cameras and no
audio devices, and it owns no clock. The caller hands it PDUs
(`handle_incoming`), asks what to transmit (`poll_transmit`), and tells it that
time has passed (`handle_timeout`).

Calibration belongs to each *channel*, which carries its own lifecycle, rather
than to the session. The session knows only whether there is a peer, whether it
is negotiating, and whether it is transferring.

This is the pattern `quinn` and `rustls` use, for the same reason.

## Consequences

A 5 MB transfer across a channel losing 40% of frames is a unit test that runs in
seconds with no hardware. So is a second where two engines find each other by
photographing one another's screens through a synthetic lens with perspective
warp, blur and noise. Those are the tests that catch protocol bugs, and they run
on every commit.

The caller carries more: the application owns the clock, the pacing and the
frame lifetimes. That has been a real cost — the display hold, the rate windows
and the calibration cadence all live in the engine and had to be got right
there.

Adding a medium is implementing a trait. The acoustic codec was written and
tested without touching the session once.
